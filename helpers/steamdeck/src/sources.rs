use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use walkdir::WalkDir;

use crate::config::AppConfig;
use crate::scanner::{classify_supported_save, discover_save_files, known_save_extensions};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    MisterFpga,
    RetroArch,
    Custom,
    OpenEmu,
    AnaloguePocket,
    Windows,
    SteamDeck,
}

impl SourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MisterFpga => "mister-fpga",
            Self::RetroArch => "retroarch",
            Self::Custom => "custom",
            Self::OpenEmu => "openemu",
            Self::AnaloguePocket => "analogue-pocket",
            Self::Windows => "windows",
            Self::SteamDeck => "steamdeck",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mister-fpga" | "mister" => Some(Self::MisterFpga),
            "retroarch" => Some(Self::RetroArch),
            "custom" => Some(Self::Custom),
            "openemu" => Some(Self::OpenEmu),
            "analogue-pocket" | "analoguepocket" => Some(Self::AnaloguePocket),
            "windows" => Some(Self::Windows),
            "steamdeck" | "steam-deck" => Some(Self::SteamDeck),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum EmulatorProfile {
    Mister,
    RetroArch,
    Snes9x,
    Zsnes,
    EverDrive,
    Generic,
}

impl EmulatorProfile {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Mister => "mister",
            Self::RetroArch => "retroarch",
            Self::Snes9x => "snes9x",
            Self::Zsnes => "zsnes",
            Self::EverDrive => "everdrive",
            Self::Generic => "generic",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mister" | "mister-fpga" => Some(Self::Mister),
            "retroarch" | "retro-arch" => Some(Self::RetroArch),
            "snes9x" => Some(Self::Snes9x),
            "zsnes" => Some(Self::Zsnes),
            "everdrive" | "ever-drive" => Some(Self::EverDrive),
            "generic" | "custom" => Some(Self::Generic),
            _ => None,
        }
    }
}

pub fn default_profile_for_kind(kind: &SourceKind) -> EmulatorProfile {
    match kind {
        SourceKind::MisterFpga => EmulatorProfile::Mister,
        SourceKind::RetroArch => EmulatorProfile::RetroArch,
        _ => EmulatorProfile::Generic,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub id: String,
    pub name: String,
    pub kind: SourceKind,
    pub profile: EmulatorProfile,
    pub save_roots: Vec<PathBuf>,
    pub rom_roots: Vec<PathBuf>,
    pub recursive: bool,
    pub managed: bool,
    pub origin: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedSource {
    pub name: String,
    pub kind: SourceKind,
    pub profile: EmulatorProfile,
    pub save_roots: Vec<PathBuf>,
    pub rom_roots: Vec<PathBuf>,
    pub recursive: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceStore {
    pub sources: Vec<Source>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    pub mode: String,
    pub applied: bool,
    pub generated_at: String,
    pub candidates: Vec<ScanCandidate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanCandidate {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub profile: String,
    pub save_path: PathBuf,
    pub rom_path: PathBuf,
    pub recursive: bool,
    pub managed: bool,
    pub origin: String,
    pub detected_saves: usize,
    pub systems: Vec<String>,
    pub confidence: f32,
    pub evidence: String,
}

#[derive(Debug, Clone)]
struct CandidatePath {
    id: String,
    label: String,
    kind: SourceKind,
    profile: EmulatorProfile,
    save_path: PathBuf,
    rom_path: PathBuf,
    recursive: bool,
    origin: String,
}

#[derive(Debug, Clone)]
struct EvaluatedCandidate {
    source: Source,
    detected_saves: usize,
    systems: Vec<String>,
    confidence: f32,
    evidence: String,
}

#[derive(Debug, Clone, Default)]
struct SourceSection {
    id: String,
    values: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
struct LegacySourceStore {
    sources: Vec<LegacySource>,
}

#[derive(Debug, Clone, Deserialize)]
struct LegacySource {
    name: String,
    kind: SourceKind,
    save_roots: Vec<PathBuf>,
    rom_roots: Vec<PathBuf>,
    recursive: bool,
    created_at: Option<String>,
}

impl Source {
    pub fn new(
        name: String,
        kind: SourceKind,
        save_roots: Vec<PathBuf>,
        rom_roots: Vec<PathBuf>,
        recursive: bool,
    ) -> Self {
        let id = normalize_source_id(&name);
        let profile = default_profile_for_kind(&kind);
        Self {
            id,
            name,
            kind,
            profile,
            save_roots,
            rom_roots,
            recursive,
            managed: false,
            origin: "manual".to_string(),
            created_at: now_rfc3339(),
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn managed(
        id: String,
        label: String,
        kind: SourceKind,
        profile: EmulatorProfile,
        save_path: PathBuf,
        rom_path: PathBuf,
        recursive: bool,
        origin: String,
    ) -> Self {
        Self {
            id,
            name: label,
            kind,
            profile,
            save_roots: vec![save_path],
            rom_roots: vec![rom_path],
            recursive,
            managed: true,
            origin,
            created_at: now_rfc3339(),
        }
    }

    pub fn resolve(&self, binary_dir: &Path) -> ResolvedSource {
        ResolvedSource {
            name: self.name.clone(),
            kind: self.kind.clone(),
            profile: self.profile.clone(),
            save_roots: self
                .save_roots
                .iter()
                .map(|path| resolve_path(binary_dir, path))
                .collect(),
            rom_roots: self
                .rom_roots
                .iter()
                .map(|path| resolve_path(binary_dir, path))
                .collect(),
            recursive: self.recursive,
        }
    }

    fn save_path(&self) -> PathBuf {
        self.save_roots
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn rom_path(&self) -> PathBuf {
        self.rom_roots
            .first()
            .cloned()
            .unwrap_or_else(|| self.save_path())
    }
}

pub fn source_store_path(state_dir: &Path) -> PathBuf {
    state_dir.join("sources.json")
}

pub fn scan_report_path(state_dir: &Path) -> PathBuf {
    state_dir.join("scan_report.json")
}

pub fn load_source_store(config_path: &Path) -> Result<SourceStore> {
    let body = read_file_if_exists(config_path)?;
    let sections = parse_source_sections(&body)?;
    let mut sources = Vec::new();

    for section in sections {
        if let Some(source) = source_from_section(&section) {
            sources.push(source);
        }
    }

    Ok(SourceStore { sources })
}

pub fn save_source_store(config_path: &Path, store: &SourceStore) -> Result<()> {
    let existing = read_file_if_exists(config_path)?;
    let base = strip_source_sections(&existing);
    let rendered = render_config_with_sources(&base, &store.sources);
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("kan map niet maken: {}", parent.display()))?;
    }
    fs::write(config_path, rendered)
        .with_context(|| format!("kan config niet schrijven: {}", config_path.display()))?;
    Ok(())
}

pub fn migrate_legacy_sources_if_needed(config: &AppConfig, verbose: bool) -> Result<()> {
    let store = load_source_store(&config.config_path)?;
    if !store.sources.is_empty() {
        return Ok(());
    }

    let state_dir = config.resolved_state_dir()?;
    let legacy_path = source_store_path(&state_dir);
    if !legacy_path.exists() {
        return Ok(());
    }

    let body = fs::read_to_string(&legacy_path).with_context(|| {
        format!(
            "kan legacy source store niet lezen: {}",
            legacy_path.display()
        )
    })?;
    let legacy: LegacySourceStore = match serde_json::from_str(&body) {
        Ok(value) => value,
        Err(err) => {
            backup_corrupt(&legacy_path)?;
            bail!(
                "legacy source store is corrupt en is gebackupt ({}): {}",
                legacy_path.display(),
                err
            );
        }
    };

    let mut migrated = SourceStore::default();
    let mut seen_ids = BTreeSet::new();
    for item in legacy.sources {
        let mut id = normalize_source_id(&item.name);
        if id.is_empty() {
            id = "source".to_string();
        }
        id = dedupe_id(id, &seen_ids);
        seen_ids.insert(id.clone());

        let save_path = item
            .save_roots
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("."));
        let rom_path = item
            .rom_roots
            .first()
            .cloned()
            .unwrap_or_else(|| save_path.clone());
        let kind = item.kind.clone();
        let profile = default_profile_for_kind(&kind);

        migrated.sources.push(Source {
            id,
            name: item.name,
            kind,
            profile,
            save_roots: vec![save_path],
            rom_roots: vec![rom_path],
            recursive: item.recursive,
            managed: false,
            origin: "legacy-migrated".to_string(),
            created_at: item.created_at.unwrap_or_else(now_rfc3339),
        });
    }

    save_source_store(&config.config_path, &migrated)?;

    let backup = legacy_path.with_file_name(format!(
        "sources.migrated.{}.json",
        OffsetDateTime::now_utc().unix_timestamp()
    ));
    fs::rename(&legacy_path, &backup).with_context(|| {
        format!(
            "kan legacy source store niet verplaatsen: {} -> {}",
            legacy_path.display(),
            backup.display()
        )
    })?;

    if verbose {
        eprintln!(
            "Legacy source store gemigreerd naar config.ini en gebackupt als {}",
            backup.display()
        );
    }

    Ok(())
}

pub fn prepare_sources_for_sync(
    config: &AppConfig,
    default_kind: SourceKind,
    scan: bool,
    deep_scan: bool,
    apply_scan: bool,
    verbose: bool,
) -> Result<Vec<ResolvedSource>> {
    if apply_scan && !deep_scan {
        bail!("--apply-scan werkt alleen samen met --deep-scan");
    }

    migrate_legacy_sources_if_needed(config, verbose)?;
    let mut store = load_source_store(&config.config_path)?;

    if store.sources.is_empty() {
        let first_run = known_scan_candidates(config, &default_kind)?;
        let evaluated = evaluate_candidates(first_run);
        let mut generated: Vec<Source> = evaluated.iter().map(|v| v.source.clone()).collect();
        if generated.is_empty() {
            generated.push(default_managed_source(config, default_kind.clone())?);
        }
        store.sources = generated;
        save_source_store(&config.config_path, &store)?;
        write_scan_report(config, "first-run", true, &evaluated)?;

        if verbose {
            eprintln!(
                "First-run autoscan opgeslagen in {} ({} source(s))",
                config.config_path.display(),
                store.sources.len()
            );
        }
    }

    if scan {
        let candidates = known_scan_candidates(config, &default_kind)?;
        let evaluated = evaluate_candidates(candidates);
        let mut managed: Vec<Source> = evaluated.iter().map(|value| value.source.clone()).collect();
        if managed.is_empty() {
            managed.push(default_managed_source(config, default_kind.clone())?);
        }
        store.sources = replace_managed_sources(&store.sources, managed);
        save_source_store(&config.config_path, &store)?;
        write_scan_report(config, "scan", true, &evaluated)?;

        if verbose {
            eprintln!(
                "Known scan voltooid: {} source(s) in config bijgewerkt.",
                store.sources.len()
            );
        }
    }

    if deep_scan {
        let evaluated = deep_scan_candidates(config)?;
        write_scan_report(config, "deep-scan", apply_scan, &evaluated)?;

        if apply_scan {
            let managed: Vec<Source> = evaluated.iter().map(|value| value.source.clone()).collect();
            store.sources = replace_managed_sources(&store.sources, managed);
            save_source_store(&config.config_path, &store)?;
            if verbose {
                eprintln!(
                    "Deep scan applied: {} source(s) actief in config.",
                    store.sources.len()
                );
            }
        } else if verbose {
            eprintln!(
                "Deep scan report geschreven naar {} (review-only).",
                scan_report_path(&config.resolved_state_dir()?).display()
            );
        }
    }

    resolved_sources_or_default(&store, config, default_kind)
}

pub fn resolved_sources_or_default(
    store: &SourceStore,
    config: &AppConfig,
    default_kind: SourceKind,
) -> Result<Vec<ResolvedSource>> {
    if store.sources.is_empty() {
        let source = default_source(config, default_kind)?;
        return Ok(vec![source.resolve(&config.binary_dir)]);
    }

    Ok(store
        .sources
        .iter()
        .map(|source| source.resolve(&config.binary_dir))
        .collect())
}

pub fn default_source(config: &AppConfig, kind: SourceKind) -> Result<Source> {
    let root = config.resolved_root()?;

    let mut source = match kind {
        SourceKind::MisterFpga => Source::new(
            "default-mister".to_string(),
            SourceKind::MisterFpga,
            vec![root.join("saves")],
            vec![root.join("games")],
            true,
        ),
        SourceKind::RetroArch => Source::new(
            "default-retroarch".to_string(),
            SourceKind::RetroArch,
            vec![root.join("saves")],
            vec![root.join("roms")],
            true,
        ),
        SourceKind::OpenEmu => Source::new(
            "default-openemu".to_string(),
            SourceKind::OpenEmu,
            vec![root.join("Save States")],
            vec![root.clone()],
            true,
        ),
        SourceKind::AnaloguePocket => Source::new(
            "default-analogue-pocket".to_string(),
            SourceKind::AnaloguePocket,
            vec![root.join("Saves")],
            vec![root.clone()],
            true,
        ),
        SourceKind::Windows => Source::new(
            "default-windows".to_string(),
            SourceKind::Windows,
            vec![root.clone()],
            vec![root.clone()],
            true,
        ),
        SourceKind::SteamDeck => {
            if let Some(emudeck_root) = detect_emudeck_root() {
                Source::new(
                    "auto-emudeck".to_string(),
                    SourceKind::SteamDeck,
                    vec![emudeck_root.join("saves")],
                    vec![emudeck_root.join("roms")],
                    true,
                )
            } else {
                Source::new(
                    "default-steamdeck".to_string(),
                    SourceKind::SteamDeck,
                    vec![root.clone()],
                    vec![root],
                    true,
                )
            }
        }
        SourceKind::Custom => Source::new(
            "default-custom".to_string(),
            SourceKind::Custom,
            vec![root.clone()],
            vec![root],
            true,
        ),
    };

    source.managed = false;
    source.origin = "default".to_string();
    Ok(source)
}

fn default_managed_source(config: &AppConfig, kind: SourceKind) -> Result<Source> {
    let base = default_source(config, kind)?;
    Ok(Source {
        managed: true,
        origin: "autoscan-default".to_string(),
        ..base
    })
}

pub fn steamdeck_autodetect_note() -> Option<String> {
    detect_emudeck_root().map(|root| {
        format!(
            "EmuDeck detected: using {} as save location.",
            root.join("saves").display()
        )
    })
}

fn detect_emudeck_root() -> Option<PathBuf> {
    detect_emudeck_root_from_candidates(&emudeck_candidates())
}

fn detect_emudeck_root_from_candidates(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|root| root.join("saves").is_dir())
        .cloned()
}

fn emudeck_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("/home/deck/Emulation")];

    if let Ok(entries) = fs::read_dir("/run/media") {
        for entry in entries.flatten() {
            candidates.push(entry.path().join("Emulation"));
        }
    }

    candidates.sort();
    candidates.dedup();
    candidates
}

fn known_scan_candidates(
    config: &AppConfig,
    default_kind: &SourceKind,
) -> Result<Vec<CandidatePath>> {
    let mut candidates = Vec::new();

    if let Some(note_root) = detect_emudeck_root() {
        candidates.push(CandidatePath {
            id: "steamdeck_emudeck".to_string(),
            label: "SteamDeck EmuDeck".to_string(),
            kind: SourceKind::SteamDeck,
            profile: EmulatorProfile::RetroArch,
            save_path: note_root.join("saves"),
            rom_path: note_root.join("roms"),
            recursive: true,
            origin: "autoscan-known".to_string(),
        });
    }

    if let Ok(home) = env::var("HOME") {
        let home = PathBuf::from(home);
        candidates.push(CandidatePath {
            id: "retroarch_home".to_string(),
            label: "RetroArch Home".to_string(),
            kind: SourceKind::RetroArch,
            profile: EmulatorProfile::RetroArch,
            save_path: home.join(".config/retroarch/saves"),
            rom_path: home.join("Emulation/roms"),
            recursive: true,
            origin: "autoscan-known".to_string(),
        });
        candidates.push(CandidatePath {
            id: "snes9x_home".to_string(),
            label: "Snes9x Home".to_string(),
            kind: SourceKind::Custom,
            profile: EmulatorProfile::Snes9x,
            save_path: home.join("snes9x/save"),
            rom_path: home.join("roms/snes"),
            recursive: true,
            origin: "autoscan-known".to_string(),
        });
    }

    candidates.push(CandidatePath {
        id: "mister_sd".to_string(),
        label: "MiSTer SD".to_string(),
        kind: SourceKind::MisterFpga,
        profile: EmulatorProfile::Mister,
        save_path: PathBuf::from("/media/fat/saves"),
        rom_path: PathBuf::from("/media/fat/games"),
        recursive: true,
        origin: "autoscan-known".to_string(),
    });

    candidates.push(CandidatePath {
        id: "retroarch_system".to_string(),
        label: "RetroArch System".to_string(),
        kind: SourceKind::RetroArch,
        profile: EmulatorProfile::RetroArch,
        save_path: PathBuf::from("/var/lib/retroarch/saves"),
        rom_path: PathBuf::from("/var/lib/retroarch/roms"),
        recursive: true,
        origin: "autoscan-known".to_string(),
    });

    let fallback = default_source(config, default_kind.clone())?;
    candidates.push(CandidatePath {
        id: format!("{}_default", default_kind.as_str()),
        label: format!("{} Default", default_kind.as_str()),
        kind: default_kind.clone(),
        profile: default_profile_for_kind(default_kind),
        save_path: fallback.save_path(),
        rom_path: fallback.rom_path(),
        recursive: true,
        origin: "autoscan-known".to_string(),
    });
    let root = config.resolved_root()?;
    candidates.push(CandidatePath {
        id: format!("{}_root", default_kind.as_str()),
        label: format!("{} Root Direct", default_kind.as_str()),
        kind: default_kind.clone(),
        profile: default_profile_for_kind(default_kind),
        save_path: root.clone(),
        rom_path: root,
        recursive: true,
        origin: "autoscan-known".to_string(),
    });

    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    candidates.dedup_by(|a, b| a.save_path == b.save_path && a.rom_path == b.rom_path);
    Ok(candidates)
}

fn evaluate_candidates(candidates: Vec<CandidatePath>) -> Vec<EvaluatedCandidate> {
    let mut out = Vec::new();

    for candidate in candidates {
        if !candidate.save_path.is_dir() {
            continue;
        }

        let discovered = discover_save_files(
            std::slice::from_ref(&candidate.save_path),
            candidate.recursive,
        )
        .unwrap_or_default();

        let mut systems = BTreeSet::new();
        let mut valid = 0usize;
        for save in discovered.iter().take(1000) {
            if let Some(classification) = classify_supported_save(save, None) {
                valid += 1;
                systems.insert(classification.system_slug);
            }
        }

        if valid == 0 {
            continue;
        }

        let confidence = ((valid as f32) / 20.0).min(1.0);
        let mut id = normalize_source_id(&candidate.id);
        if id.is_empty() {
            id = normalize_source_id(&candidate.label);
        }
        if id.is_empty() {
            id = format!("source_{}", out.len() + 1);
        }

        out.push(EvaluatedCandidate {
            source: Source::managed(
                id,
                candidate.label.clone(),
                candidate.kind.clone(),
                candidate.profile.clone(),
                candidate.save_path.clone(),
                candidate.rom_path.clone(),
                candidate.recursive,
                candidate.origin.clone(),
            ),
            detected_saves: valid,
            systems: systems.into_iter().collect(),
            confidence,
            evidence: format!(
                "{} valid save(s) gevonden in {}",
                valid,
                candidate.save_path.display()
            ),
        });
    }

    out.sort_by(|a, b| {
        b.detected_saves
            .cmp(&a.detected_saves)
            .then_with(|| a.source.id.cmp(&b.source.id))
    });
    out
}

fn deep_scan_candidates(config: &AppConfig) -> Result<Vec<EvaluatedCandidate>> {
    let roots = deep_scan_roots();
    let save_extensions = known_save_extensions();

    let mut per_dir: BTreeMap<PathBuf, (usize, BTreeSet<String>)> = BTreeMap::new();

    for root in roots {
        if !root.exists() {
            continue;
        }

        let walker = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| !is_skipped_deep_scan_path(entry.path()));

        for entry in walker.filter_map(|entry| entry.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            let Some(ext) = path.extension().and_then(|value| value.to_str()) else {
                continue;
            };
            if !save_extensions.contains(&ext.to_ascii_lowercase().as_str()) {
                continue;
            }

            let Some(classification) = classify_supported_save(path, None) else {
                continue;
            };

            let parent = path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            let slot = per_dir.entry(parent).or_insert((0usize, BTreeSet::new()));
            slot.0 += 1;
            slot.1.insert(classification.system_slug);
        }
    }

    let mut candidates = Vec::new();
    for (index, (dir, (count, systems))) in per_dir.into_iter().enumerate() {
        if count == 0 {
            continue;
        }

        let id = normalize_source_id(&format!("deep_{}_{}", index + 1, dir.display()));
        let label = dir
            .file_name()
            .and_then(|value| value.to_str())
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("Deep Scan {}", index + 1));

        candidates.push(EvaluatedCandidate {
            source: Source::managed(
                id,
                label,
                SourceKind::Custom,
                EmulatorProfile::Generic,
                dir.clone(),
                dir.clone(),
                true,
                "deep-scan".to_string(),
            ),
            detected_saves: count,
            systems: systems.into_iter().collect(),
            confidence: ((count as f32) / 30.0).min(1.0),
            evidence: format!("{} valid save(s) in {}", count, dir.display()),
        });
    }

    candidates.sort_by(|a, b| {
        b.detected_saves
            .cmp(&a.detected_saves)
            .then_with(|| a.source.id.cmp(&b.source.id))
    });

    if candidates.len() > 200 {
        candidates.truncate(200);
    }

    let _ = config;
    Ok(candidates)
}

fn deep_scan_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/")];

    if cfg!(target_os = "windows") {
        roots.clear();
        for drive in 'C'..='Z' {
            let candidate = PathBuf::from(format!("{}:\\", drive));
            if candidate.exists() {
                roots.push(candidate);
            }
        }
    }

    roots
}

fn is_skipped_deep_scan_path(path: &Path) -> bool {
    let text = path.to_string_lossy().to_ascii_lowercase();
    [
        "/proc",
        "/sys",
        "/dev",
        "/run",
        "/tmp",
        "/var/lib/docker",
        "\\windows\\winsxs",
        "\\windows\\system32\\driverstore",
        "/node_modules",
        "/.git",
        "/target",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn replace_managed_sources(existing: &[Source], mut replacements: Vec<Source>) -> Vec<Source> {
    let mut out: Vec<Source> = existing.iter().filter(|v| !v.managed).cloned().collect();
    let existing_ids: BTreeSet<String> = out.iter().map(|value| value.id.clone()).collect();

    for mut replacement in replacements.drain(..) {
        replacement.id = dedupe_id(replacement.id, &existing_ids);
        out.push(replacement);
    }

    out
}

fn write_scan_report(
    config: &AppConfig,
    mode: &str,
    applied: bool,
    evaluated: &[EvaluatedCandidate],
) -> Result<()> {
    let state_dir = config.resolved_state_dir()?;
    fs::create_dir_all(&state_dir)
        .with_context(|| format!("kan state map niet maken: {}", state_dir.display()))?;

    let report = ScanReport {
        mode: mode.to_string(),
        applied,
        generated_at: now_rfc3339(),
        candidates: evaluated
            .iter()
            .map(|value| ScanCandidate {
                id: value.source.id.clone(),
                label: value.source.name.clone(),
                kind: value.source.kind.as_str().to_string(),
                profile: value.source.profile.as_str().to_string(),
                save_path: value.source.save_path(),
                rom_path: value.source.rom_path(),
                recursive: value.source.recursive,
                managed: value.source.managed,
                origin: value.source.origin.clone(),
                detected_saves: value.detected_saves,
                systems: value.systems.clone(),
                confidence: value.confidence,
                evidence: value.evidence.clone(),
            })
            .collect(),
    };

    let path = scan_report_path(&state_dir);
    fs::write(&path, serde_json::to_string_pretty(&report)?)
        .with_context(|| format!("kan scan report niet schrijven: {}", path.display()))?;
    Ok(())
}

pub fn upsert_source(store: &mut SourceStore, source: Source) {
    if let Some(existing) = store
        .sources
        .iter_mut()
        .find(|value| value.id == source.id || value.name == source.name)
    {
        *existing = source;
        return;
    }
    store.sources.push(source);
}

pub fn remove_source(store: &mut SourceStore, name: &str) -> bool {
    let normalized = normalize_source_id(name);
    let before = store.sources.len();
    store.sources.retain(|source| {
        source.id != normalized
            && source.name != name
            && normalize_source_id(&source.name) != normalized
    });
    before != store.sources.len()
}

pub fn resolve_path(binary_dir: &Path, value: &Path) -> PathBuf {
    if value.is_absolute() {
        value.to_path_buf()
    } else {
        binary_dir.join(value)
    }
}

fn source_from_section(section: &SourceSection) -> Option<Source> {
    let kind = section
        .values
        .get("KIND")
        .and_then(|value| SourceKind::parse(value))
        .unwrap_or(SourceKind::Custom);
    let profile = section
        .values
        .get("PROFILE")
        .or_else(|| section.values.get("EMULATOR"))
        .and_then(|value| EmulatorProfile::parse(value))
        .unwrap_or_else(|| default_profile_for_kind(&kind));

    let label = section
        .values
        .get("LABEL")
        .cloned()
        .unwrap_or_else(|| section.id.clone());

    let save_path = section.values.get("SAVE_PATH").map(PathBuf::from)?;
    let rom_path = section
        .values
        .get("ROM_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| save_path.clone());

    let recursive = section
        .values
        .get("RECURSIVE")
        .and_then(|value| parse_bool(value).ok())
        .unwrap_or(true);

    let managed = section
        .values
        .get("MANAGED")
        .and_then(|value| parse_bool(value).ok())
        .unwrap_or(false);

    let origin = section.values.get("ORIGIN").cloned().unwrap_or_else(|| {
        if managed {
            "autoscan-known".to_string()
        } else {
            "manual".to_string()
        }
    });

    Some(Source {
        id: section.id.clone(),
        name: label,
        kind,
        profile,
        save_roots: vec![save_path],
        rom_roots: vec![rom_path],
        recursive,
        managed,
        origin,
        created_at: now_rfc3339(),
    })
}

fn parse_source_sections(content: &str) -> Result<Vec<SourceSection>> {
    let mut out = Vec::new();
    let mut current: Option<SourceSection> = None;

    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if let Some(section) = current.take() {
                out.push(section);
            }

            let name = trimmed.trim_start_matches('[').trim_end_matches(']');
            if let Some(id) = name.strip_prefix("source.") {
                current = Some(SourceSection {
                    id: normalize_source_id(id),
                    values: HashMap::new(),
                });
            } else {
                current = None;
            }
            continue;
        }

        let Some(section) = current.as_mut() else {
            continue;
        };

        let Some(eq_pos) = trimmed.find('=') else {
            bail!("ongeldige INI regel {}: ontbrekende '='", idx + 1);
        };

        let key = trimmed[..eq_pos].trim().to_uppercase();
        let mut value = trimmed[eq_pos + 1..].trim().to_string();
        if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
            value = value[1..value.len() - 1].to_string();
        }
        section.values.insert(key, value);
    }

    if let Some(section) = current {
        out.push(section);
    }

    Ok(out)
}

fn strip_source_sections(content: &str) -> String {
    let mut out = Vec::new();
    let mut skipping = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let section = trimmed.trim_start_matches('[').trim_end_matches(']');
            skipping = section.starts_with("source.");
            if !skipping {
                out.push(line.to_string());
            }
            continue;
        }

        if !skipping {
            out.push(line.to_string());
        }
    }

    while out
        .last()
        .map(|line| line.trim().is_empty())
        .unwrap_or(false)
    {
        out.pop();
    }

    out.join("\n")
}

fn render_config_with_sources(base: &str, sources: &[Source]) -> String {
    let mut lines = Vec::new();
    if !base.trim().is_empty() {
        lines.push(base.to_string());
    }

    let mut sorted = sources.to_vec();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));

    for source in sorted {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(format!("[source.{}]", source.id));
        lines.push(format!("LABEL=\"{}\"", escape_ini(&source.name)));
        lines.push(format!("KIND=\"{}\"", source.kind.as_str()));
        lines.push(format!("PROFILE=\"{}\"", source.profile.as_str()));
        lines.push(format!(
            "SAVE_PATH=\"{}\"",
            escape_ini(&source.save_path().to_string_lossy())
        ));
        lines.push(format!(
            "ROM_PATH=\"{}\"",
            escape_ini(&source.rom_path().to_string_lossy())
        ));
        lines.push(format!("RECURSIVE=\"{}\"", source.recursive));
        lines.push(format!("MANAGED=\"{}\"", source.managed));
        lines.push(format!("ORIGIN=\"{}\"", escape_ini(&source.origin)));
    }

    format!("{}\n", lines.join("\n"))
}

fn read_file_if_exists(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(path).with_context(|| format!("kan bestand niet lezen: {}", path.display()))
}

fn normalize_source_id(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if (ch.is_ascii_whitespace() || ch == '-' || ch == '_' || ch == '/')
            && !out.ends_with('_')
        {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

fn dedupe_id(base: String, existing: &BTreeSet<String>) -> String {
    if !existing.contains(&base) {
        return base;
    }

    for idx in 2..10000 {
        let candidate = format!("{}_{}", base, idx);
        if !existing.contains(&candidate) {
            return candidate;
        }
    }
    format!("{}_{}", base, OffsetDateTime::now_utc().unix_timestamp())
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => bail!("ongeldige bool '{}'", value),
    }
}

fn escape_ini(value: &str) -> String {
    value.replace('"', "\\\"")
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn backup_corrupt(path: &Path) -> Result<()> {
    let suffix = OffsetDateTime::now_utc().unix_timestamp();
    let backup = path.with_extension(format!("corrupt.{}", suffix));
    fs::rename(path, &backup).with_context(|| {
        format!(
            "kan corrupt source store niet back-uppen: {} -> {}",
            path.display(),
            backup.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(tmp: &tempfile::TempDir, root: PathBuf) -> AppConfig {
        AppConfig {
            url: "127.0.0.1".to_string(),
            port: 3001,
            email: String::new(),
            app_password: String::new(),
            root,
            state_dir: tmp.path().join("state"),
            watch: false,
            watch_interval: 30,
            force_upload: false,
            dry_run: false,
            route_prefix: String::new(),
            binary_dir: tmp.path().to_path_buf(),
            config_path: tmp.path().join("config.ini"),
        }
    }

    #[test]
    fn default_steamdeck_source_uses_compatdata_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(
            &tmp,
            PathBuf::from("/home/deck/.steam/steam/steamapps/compatdata"),
        );

        let source = default_source(&cfg, SourceKind::SteamDeck).unwrap();
        assert_eq!(
            source.save_roots[0].to_string_lossy(),
            "/home/deck/.steam/steam/steamapps/compatdata"
        );
    }

    #[test]
    fn parses_source_sections_from_config() {
        let content = r#"
URL="127.0.0.1"

[source.super_nintendo]
LABEL="Super Nintendo"
KIND="retroarch"
PROFILE="snes9x"
SAVE_PATH="/home/snes9x/save"
ROM_PATH="/home/roms/snes"
RECURSIVE="true"
MANAGED="false"
ORIGIN="manual"
"#;

        let sections = parse_source_sections(content).unwrap();
        assert_eq!(sections.len(), 1);
        let source = source_from_section(&sections[0]).unwrap();
        assert_eq!(source.id, "super_nintendo");
        assert_eq!(source.name, "Super Nintendo");
        assert_eq!(source.kind, SourceKind::RetroArch);
        assert_eq!(source.profile, EmulatorProfile::Snes9x);
        assert_eq!(source.save_path().to_string_lossy(), "/home/snes9x/save");
        assert!(!source.managed);
    }

    #[test]
    fn profile_defaults_to_kind_mapping_when_omitted() {
        let content = r#"
[source.legacy]
LABEL="Legacy"
KIND="mister-fpga"
SAVE_PATH="/media/fat/saves"
"#;

        let sections = parse_source_sections(content).unwrap();
        let source = source_from_section(&sections[0]).unwrap();
        assert_eq!(source.profile, EmulatorProfile::Mister);
    }

    #[test]
    fn strip_and_render_replaces_source_sections_only() {
        let existing = r#"URL="127.0.0.1"
PORT="9096"

[source.old]
LABEL="Old"
SAVE_PATH="/tmp/old"

[other.section]
HELLO="world"
"#;

        let base = strip_source_sections(existing);
        assert!(base.contains("URL=\"127.0.0.1\""));
        assert!(base.contains("[other.section]"));
        assert!(!base.contains("[source.old]"));

        let rendered = render_config_with_sources(
            &base,
            &[Source::managed(
                "new_source".to_string(),
                "New Source".to_string(),
                SourceKind::Custom,
                EmulatorProfile::Generic,
                PathBuf::from("/tmp/new"),
                PathBuf::from("/tmp/new"),
                true,
                "autoscan-known".to_string(),
            )],
        );
        assert!(rendered.contains("[source.new_source]"));
        assert!(rendered.contains("[other.section]"));
    }

    #[test]
    fn detects_emudeck_root_from_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let emudeck_root = tmp.path().join("Emulation");
        fs::create_dir_all(emudeck_root.join("saves")).unwrap();

        let selected = detect_emudeck_root_from_candidates(&[
            tmp.path().join("missing"),
            emudeck_root.clone(),
        ]);
        assert_eq!(selected, Some(emudeck_root));
    }

    #[test]
    fn migrates_legacy_sources_json_into_config() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(&tmp, tmp.path().join("root"));
        fs::create_dir_all(cfg.resolved_state_dir().unwrap()).unwrap();
        fs::write(&cfg.config_path, "URL=\"127.0.0.1\"\nPORT=\"3001\"\n").unwrap();

        let legacy = serde_json::json!({
            "sources": [
                {
                    "name": "Legacy RetroArch",
                    "kind": "retro-arch",
                    "save_roots": ["/tmp/legacy/saves"],
                    "rom_roots": ["/tmp/legacy/roms"],
                    "recursive": true,
                    "created_at": "2026-01-01T00:00:00Z"
                }
            ]
        });
        let legacy_path = source_store_path(&cfg.resolved_state_dir().unwrap());
        fs::write(&legacy_path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();

        migrate_legacy_sources_if_needed(&cfg, false).unwrap();
        let loaded = load_source_store(&cfg.config_path).unwrap();
        assert_eq!(loaded.sources.len(), 1);
        assert_eq!(loaded.sources[0].name, "Legacy RetroArch");
        assert!(!loaded.sources[0].managed);

        let backups = fs::read_dir(cfg.resolved_state_dir().unwrap())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with("sources.migrated."))
            .count();
        assert_eq!(backups, 1);
    }

    #[test]
    fn first_run_autoscan_writes_managed_source_blocks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        fs::create_dir_all(root.join("Nintendo")).unwrap();
        fs::write(root.join("Nintendo/zelda.srm"), vec![0x00u8; 8192]).unwrap();

        let cfg = test_config(&tmp, root);
        fs::create_dir_all(cfg.resolved_state_dir().unwrap()).unwrap();
        fs::write(
            &cfg.config_path,
            "URL=\"127.0.0.1\"\nPORT=\"3001\"\nROOT=\"./root\"\nSTATE_DIR=\"./state\"\n",
        )
        .unwrap();

        let resolved =
            prepare_sources_for_sync(&cfg, SourceKind::SteamDeck, false, false, false, false)
                .unwrap();
        assert!(!resolved.is_empty());

        let loaded = load_source_store(&cfg.config_path).unwrap();
        assert!(!loaded.sources.is_empty());
        assert!(loaded.sources.iter().any(|source| source.managed));

        let config_body = fs::read_to_string(&cfg.config_path).unwrap();
        assert!(config_body.contains("[source."));
        assert!(scan_report_path(&cfg.resolved_state_dir().unwrap()).exists());
    }
}
