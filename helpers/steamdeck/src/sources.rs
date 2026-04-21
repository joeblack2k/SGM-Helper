use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::config::AppConfig;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    MisterFpga,
    RetroArch,
    Custom,
    OpenEmu,
    AnaloguePocket,
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
            Self::SteamDeck => "steamdeck",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub name: String,
    pub kind: SourceKind,
    pub save_roots: Vec<PathBuf>,
    pub rom_roots: Vec<PathBuf>,
    pub recursive: bool,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedSource {
    pub name: String,
    pub kind: SourceKind,
    pub save_roots: Vec<PathBuf>,
    pub rom_roots: Vec<PathBuf>,
    pub recursive: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceStore {
    pub sources: Vec<Source>,
}

impl Source {
    pub fn new(
        name: String,
        kind: SourceKind,
        save_roots: Vec<PathBuf>,
        rom_roots: Vec<PathBuf>,
        recursive: bool,
    ) -> Self {
        Self {
            name,
            kind,
            save_roots,
            rom_roots,
            recursive,
            created_at: now_rfc3339(),
        }
    }

    pub fn resolve(&self, binary_dir: &Path) -> ResolvedSource {
        ResolvedSource {
            name: self.name.clone(),
            kind: self.kind.clone(),
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
}

pub fn source_store_path(state_dir: &Path) -> PathBuf {
    state_dir.join("sources.json")
}

pub fn load_source_store(state_dir: &Path) -> Result<SourceStore> {
    let path = source_store_path(state_dir);
    if !path.exists() {
        return Ok(SourceStore::default());
    }

    let body = fs::read_to_string(&path)
        .with_context(|| format!("kan source store niet lezen: {}", path.display()))?;
    match serde_json::from_str::<SourceStore>(&body) {
        Ok(store) => Ok(store),
        Err(err) => {
            backup_corrupt(&path)?;
            eprintln!(
                "Waarschuwing: corrupt source store gereset ({}): {}",
                path.display(),
                err
            );
            Ok(SourceStore::default())
        }
    }
}

pub fn save_source_store(state_dir: &Path, store: &SourceStore) -> Result<()> {
    let path = source_store_path(state_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("kan map niet maken: {}", parent.display()))?;
    }
    fs::write(&path, serde_json::to_string_pretty(store)?)
        .with_context(|| format!("kan source store niet schrijven: {}", path.display()))?;
    Ok(())
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

    let source = match kind {
        SourceKind::MisterFpga => Source::new(
            "default-mister".to_string(),
            SourceKind::MisterFpga,
            vec![root.join("saves"), root.clone()],
            vec![root.join("games"), root.clone()],
            true,
        ),
        SourceKind::RetroArch => Source::new(
            "default-retroarch".to_string(),
            SourceKind::RetroArch,
            vec![root.join("saves")],
            vec![root.join("roms"), root.join("content")],
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
            vec![root.join("Saves"), root.join("saves")],
            vec![root.clone()],
            true,
        ),
        SourceKind::SteamDeck => {
            if let Some(emudeck_root) = detect_emudeck_root() {
                Source::new(
                    "auto-emudeck".to_string(),
                    SourceKind::SteamDeck,
                    vec![emudeck_root.join("saves")],
                    vec![emudeck_root.join("roms"), emudeck_root.join("content")],
                    true,
                )
            } else {
                Source::new(
                    "default-steamdeck".to_string(),
                    SourceKind::SteamDeck,
                    vec![root.clone()],
                    vec![root.clone(), PathBuf::from("/home/deck/Emulation/roms")],
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

    Ok(source)
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

pub fn upsert_source(store: &mut SourceStore, source: Source) {
    if let Some(existing) = store
        .sources
        .iter_mut()
        .find(|value| value.name == source.name)
    {
        *existing = source;
        return;
    }
    store.sources.push(source);
}

pub fn remove_source(store: &mut SourceStore, name: &str) -> bool {
    let before = store.sources.len();
    store.sources.retain(|source| source.name != name);
    before != store.sources.len()
}

pub fn resolve_path(binary_dir: &Path, value: &Path) -> PathBuf {
    if value.is_absolute() {
        value.to_path_buf()
    } else {
        binary_dir.join(value)
    }
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

    #[test]
    fn default_steamdeck_source_uses_compatdata_layout() {
        let cfg = AppConfig {
            url: "127.0.0.1".to_string(),
            port: 3001,
            email: String::new(),
            app_password: String::new(),
            root: PathBuf::from("/home/deck/.steam/steam/steamapps/compatdata"),
            state_dir: PathBuf::from("./state"),
            watch: false,
            watch_interval: 30,
            force_upload: false,
            dry_run: false,
            route_prefix: String::new(),
            binary_dir: PathBuf::from("/tmp"),
        };

        let source = default_source(&cfg, SourceKind::SteamDeck).unwrap();
        assert_eq!(
            source.save_roots[0].to_string_lossy(),
            "/home/deck/.steam/steam/steamapps/compatdata"
        );
        assert_eq!(
            source.rom_roots[1].to_string_lossy(),
            "/home/deck/Emulation/roms"
        );
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
}
