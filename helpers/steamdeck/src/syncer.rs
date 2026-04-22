use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::api::{ApiClient, ConflictCheckResponse};
use crate::config::AppConfig;
use crate::scanner::{
    RomIndexEntry, SaveAdapterProfile, SaveContainerFormat, classify_supported_save,
    discover_rom_index, discover_save_files, encode_download_for_local_container, filename_stem,
    md5_file, normalize_save_for_sync, sha1_file, sha256_bytes,
};
use crate::sources::{EmulatorProfile, SourceKind, prepare_sources_for_sync};
use crate::state::{AuthState, SyncedEntry, load_sync_state, now_rfc3339, save_sync_state};

#[derive(Debug, Clone)]
pub struct SyncOptions {
    pub force_upload: bool,
    pub dry_run: bool,
    pub scan: bool,
    pub deep_scan: bool,
    pub apply_scan: bool,
    pub slot_name: String,
    pub default_source_kind: SourceKind,
}

#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    pub scanned: usize,
    pub uploaded: usize,
    pub downloaded: usize,
    pub in_sync: usize,
    pub conflicts: usize,
    pub skipped: usize,
    pub errors: usize,
}

struct ProcessedEntry {
    state_key: String,
    entry: SyncedEntry,
}

struct SyncLock {
    path: PathBuf,
}

impl SyncLock {
    fn acquire(state_dir: &Path) -> Result<Self> {
        let path = state_dir.join("sync.lock");
        let lock_content = format!(
            "pid={}\\nstarted_at={}\\n",
            std::process::id(),
            now_rfc3339()
        );
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(lock_content.as_bytes())
                    .with_context(|| format!("kan lockfile niet schrijven: {}", path.display()))?;
                Ok(Self { path })
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                anyhow::bail!("sync is al actief (lockfile bestaat): {}", path.display());
            }
            Err(err) => Err(err)
                .with_context(|| format!("kan sync lockfile niet maken: {}", path.display())),
        }
    }
}

impl Drop for SyncLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn run_sync(
    config: &AppConfig,
    auth: Option<&AuthState>,
    options: &SyncOptions,
    verbose: bool,
) -> Result<SyncReport> {
    let state_dir = config.resolved_state_dir()?;
    fs::create_dir_all(&state_dir)
        .with_context(|| format!("kan state map niet maken: {}", state_dir.display()))?;
    let _lock = SyncLock::acquire(&state_dir)?;

    let mut sync_state = load_sync_state(&state_dir)?;

    let token = auth.map(|value| value.token.clone());
    let api = ApiClient::new(config.base_url(), config.route_prefix.clone(), token)?;

    let sources = prepare_sources_for_sync(
        config,
        options.default_source_kind.clone(),
        options.scan,
        options.deep_scan,
        options.apply_scan,
        verbose,
    )?;
    let app_password = if config.app_password.trim().is_empty() {
        None
    } else {
        Some(config.app_password.trim())
    };

    let mut report = SyncReport::default();
    let mut rom_hash_cache: HashMap<String, (String, String)> = HashMap::new();

    for source in sources {
        let save_files = discover_save_files(&source.save_roots, source.recursive)?;
        if verbose {
            eprintln!(
                "Source '{}' ({}) discovered {} save file(s)",
                source.name,
                source.kind.as_str(),
                save_files.len()
            );
        }

        let rom_index = discover_rom_index(&source.rom_roots, source.recursive)?;
        let preferred_save_by_stem =
            select_preferred_save_per_stem(&source.kind, &source.profile, &save_files, &rom_index);
        report.scanned += save_files.len();

        let fingerprint = hostname::get()
            .ok()
            .and_then(|value| value.into_string().ok())
            .unwrap_or_else(|| source.kind.as_str().to_string());

        for save_path in save_files {
            let stem_key = filename_stem(&save_path).to_ascii_lowercase();
            if let Some(preferred_path) = preferred_save_by_stem.get(&stem_key)
                && preferred_path != &save_path
            {
                report.skipped += 1;
                if verbose {
                    eprintln!(
                        "Skipping duplicate save variant {} in favor of preferred {}",
                        save_path.display(),
                        preferred_path.display()
                    );
                }
                continue;
            }

            let save_key = save_path.to_string_lossy().to_string();
            let process_result = process_single_save(
                &api,
                &save_path,
                &save_key,
                &fingerprint,
                &source.name,
                &source.kind,
                &source.profile,
                &rom_index,
                &mut rom_hash_cache,
                app_password,
                options,
                &mut report,
                verbose,
            );

            match process_result {
                Ok(processed) => {
                    if let Some(processed) = processed {
                        if processed.state_key != save_key {
                            sync_state.entries.remove(&save_key);
                        }
                        sync_state
                            .entries
                            .insert(processed.state_key, processed.entry);
                    }
                }
                Err(err) => {
                    report.errors += 1;
                    if verbose {
                        eprintln!("Sync error for {}: {}", save_path.display(), err);
                    }
                }
            }
        }

        let missing_entries: Vec<(String, SyncedEntry)> = sync_state
            .entries
            .iter()
            .filter_map(|(path, entry)| {
                let candidate = PathBuf::from(path);
                let linked_to_source = entry
                    .source_kind
                    .as_deref()
                    .map(|kind| kind == source.kind.as_str())
                    .unwrap_or(false)
                    && entry
                        .source_name
                        .as_deref()
                        .map(|name| name == source.name)
                        .unwrap_or(false);
                if candidate.exists()
                    || !(path_is_under_roots(&candidate, &source.save_roots) || linked_to_source)
                {
                    return None;
                }
                Some((path.clone(), entry.clone()))
            })
            .collect();

        for (save_key, entry) in missing_entries {
            let save_path = PathBuf::from(&save_key);
            let restore = process_missing_save(
                &api,
                &save_path,
                &entry,
                &source.name,
                &source.kind,
                &source.profile,
                options,
                &mut report,
                verbose,
            );

            match restore {
                Ok(processed) => {
                    if let Some(processed) = processed {
                        if processed.state_key != save_key {
                            sync_state.entries.remove(&save_key);
                        }
                        sync_state
                            .entries
                            .insert(processed.state_key, processed.entry);
                    }
                }
                Err(err) => {
                    report.errors += 1;
                    if verbose {
                        eprintln!("Restore error for {}: {}", save_path.display(), err);
                    }
                }
            }
        }
    }

    if !options.dry_run {
        save_sync_state(&state_dir, &sync_state)?;
    }

    Ok(report)
}

fn path_is_under_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

#[allow(clippy::too_many_arguments)]
fn process_single_save(
    api: &ApiClient,
    save_path: &std::path::Path,
    save_key: &str,
    fingerprint: &str,
    source_name: &str,
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    rom_index: &HashMap<String, RomIndexEntry>,
    rom_hash_cache: &mut HashMap<String, (String, String)>,
    app_password: Option<&str>,
    options: &SyncOptions,
    report: &mut SyncReport,
    verbose: bool,
) -> Result<Option<ProcessedEntry>> {
    let stem = filename_stem(save_path);
    let stem_key = stem.to_ascii_lowercase();
    let rom_entry = rom_index.get(&stem_key);
    let mut state_key = save_key.to_string();
    let Some(classification) =
        classify_supported_save(save_path, rom_entry.map(|entry| entry.path.as_path()))
    else {
        report.skipped += 1;
        if verbose {
            eprintln!(
                "Skipping non-supported save (outside allowed console families): {}",
                save_path.display()
            );
        }
        return Ok(None);
    };
    let system_slug = classification.system_slug;
    let classification_evidence = classification.evidence;
    let normalized_save = match normalize_save_for_sync(save_path, &system_slug)? {
        Some(value) => value,
        None => {
            report.skipped += 1;
            if verbose {
                eprintln!(
                    "Skipping {}: failed strict binary validation for {}",
                    save_path.display(),
                    system_slug
                );
            }
            return Ok(None);
        }
    };
    let local_sha = sha256_bytes(&normalized_save.canonical_bytes);
    if verbose {
        eprintln!(
            "Detected {} savegame for {} ({}) [adapter={} container={}]",
            system_slug,
            save_path.display(),
            classification_evidence,
            normalized_save.adapter_profile.as_str(),
            normalized_save.local_container.as_str(),
        );
    }

    let effective_slot_name =
        resolve_slot_name_for_sync(&system_slug, save_path, &options.slot_name);
    let device_type = helper_device_type_for_upload(source_kind, source_profile, &system_slug);

    let mut rom_sha1 = if is_playstation_system(&system_slug) {
        Some(playstation_line_key(
            &system_slug,
            device_type,
            &effective_slot_name,
        ))
    } else {
        None
    };
    let mut rom_md5: Option<String> = None;

    if rom_sha1.is_none() {
        let lookup = api.lookup_rom(&stem).ok();
        rom_sha1 = lookup
            .as_ref()
            .and_then(|value| value.rom.as_ref())
            .and_then(|value| value.sha1.clone());
        rom_md5 = lookup
            .as_ref()
            .and_then(|value| value.rom.as_ref())
            .and_then(|value| value.md5.clone());

        if rom_sha1.is_none()
            && let Some(rom_entry) = rom_entry
        {
            if let Some((cached_sha1, cached_md5)) = rom_hash_cache.get(&stem_key).cloned() {
                rom_sha1 = Some(cached_sha1);
                rom_md5 = Some(cached_md5);
            } else {
                let local_rom_sha1 = sha1_file(&rom_entry.path)?;
                let local_rom_md5 = md5_file(&rom_entry.path)?;
                rom_hash_cache.insert(
                    stem_key.clone(),
                    (local_rom_sha1.clone(), local_rom_md5.clone()),
                );
                rom_sha1 = Some(local_rom_sha1);
                rom_md5 = Some(local_rom_md5);
            }
        }
    }

    let Some(active_rom_sha1) = rom_sha1 else {
        report.skipped += 1;
        if verbose {
            eprintln!("No ROM mapping found for save {}", stem);
        }
        return Ok(None);
    };

    let latest = api.latest_save(&active_rom_sha1, &effective_slot_name)?;

    if !latest.exists {
        if options.dry_run {
            report.uploaded += 1;
            return Ok(Some(processed_entry(
                state_key.clone(),
                synced_entry(
                    local_sha,
                    Some(active_rom_sha1.clone()),
                    None,
                    Some(&system_slug),
                    Some(normalized_save.local_container),
                    Some(normalized_save.adapter_profile),
                    Some(source_kind),
                    Some(source_name),
                    Some(&effective_slot_name),
                ),
            )));
        }

        let filename = save_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("save.bin");

        let _upload = api.upload_save(
            filename,
            normalized_save.canonical_bytes.clone(),
            &active_rom_sha1,
            rom_md5.as_deref(),
            &effective_slot_name,
            device_type,
            fingerprint,
            app_password,
            Some(&system_slug),
        )?;

        report.uploaded += 1;
        return Ok(Some(processed_entry(
            state_key.clone(),
            synced_entry(
                local_sha,
                Some(active_rom_sha1.clone()),
                latest.version,
                Some(&system_slug),
                Some(normalized_save.local_container),
                Some(normalized_save.adapter_profile),
                Some(source_kind),
                Some(source_name),
                Some(&effective_slot_name),
            ),
        )));
    }

    if latest.sha256.as_deref() == Some(local_sha.as_str()) {
        report.in_sync += 1;
        return Ok(Some(processed_entry(
            state_key.clone(),
            synced_entry(
                local_sha,
                Some(active_rom_sha1.clone()),
                latest.version,
                Some(&system_slug),
                Some(normalized_save.local_container),
                Some(normalized_save.adapter_profile),
                Some(source_kind),
                Some(source_name),
                Some(&effective_slot_name),
            ),
        )));
    }

    if options.force_upload {
        if options.dry_run {
            report.uploaded += 1;
            return Ok(Some(processed_entry(
                state_key.clone(),
                synced_entry(
                    local_sha,
                    Some(active_rom_sha1.clone()),
                    latest.version,
                    Some(&system_slug),
                    Some(normalized_save.local_container),
                    Some(normalized_save.adapter_profile),
                    Some(source_kind),
                    Some(source_name),
                    Some(&effective_slot_name),
                ),
            )));
        }

        let filename = save_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("save.bin");

        api.upload_save(
            filename,
            normalized_save.canonical_bytes.clone(),
            &active_rom_sha1,
            rom_md5.as_deref(),
            &effective_slot_name,
            device_type,
            fingerprint,
            app_password,
            Some(&system_slug),
        )?;
        report.uploaded += 1;
        return Ok(Some(processed_entry(
            state_key.clone(),
            synced_entry(
                local_sha,
                Some(active_rom_sha1.clone()),
                latest.version,
                Some(&system_slug),
                Some(normalized_save.local_container),
                Some(normalized_save.adapter_profile),
                Some(source_kind),
                Some(source_name),
                Some(&effective_slot_name),
            ),
        )));
    }

    let conflict = api.conflict_check(&active_rom_sha1, &effective_slot_name)?;
    if conflict.exists {
        handle_conflict(
            api,
            save_path,
            &normalized_save.canonical_bytes,
            &local_sha,
            &active_rom_sha1,
            &effective_slot_name,
            options.dry_run,
            &conflict,
            source_name,
            source_kind,
            app_password,
        )?;
        report.conflicts += 1;
        return Ok(Some(processed_entry(
            state_key.clone(),
            synced_entry(
                local_sha,
                Some(active_rom_sha1.clone()),
                latest.version,
                Some(&system_slug),
                Some(normalized_save.local_container),
                Some(normalized_save.adapter_profile),
                Some(source_kind),
                Some(source_name),
                Some(&effective_slot_name),
            ),
        )));
    }

    if let Some(save_id) = latest.id {
        if options.dry_run {
            report.downloaded += 1;
            let target_save_path = preferred_save_path(
                save_path,
                source_kind,
                source_profile,
                Some(&system_slug),
                normalized_save.local_container,
                Some(normalized_save.canonical_bytes.len() as u64),
            );
            state_key = target_save_path.to_string_lossy().to_string();
            return Ok(Some(processed_entry(
                state_key.clone(),
                synced_entry(
                    local_sha,
                    Some(active_rom_sha1.clone()),
                    latest.version,
                    Some(&system_slug),
                    Some(normalized_save.local_container),
                    Some(normalized_save.adapter_profile),
                    Some(source_kind),
                    Some(source_name),
                    Some(&effective_slot_name),
                ),
            )));
        }

        let canonical_bytes = api.download_save(&save_id)?;
        let local_bytes =
            encode_download_for_local_container(&canonical_bytes, normalized_save.local_container)?;
        let target_save_path = preferred_save_path(
            save_path,
            source_kind,
            source_profile,
            Some(&system_slug),
            normalized_save.local_container,
            Some(canonical_bytes.len() as u64),
        );
        if let Some(parent) = target_save_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("kan map niet maken: {}", parent.display()))?;
        }
        fs::write(&target_save_path, &local_bytes).with_context(|| {
            format!(
                "kan save bestand niet overschrijven: {}",
                target_save_path.display()
            )
        })?;
        if target_save_path != save_path && save_path.exists() {
            fs::remove_file(save_path).with_context(|| {
                format!(
                    "kan oude savevariant niet verwijderen: {}",
                    save_path.display()
                )
            })?;
        }
        state_key = target_save_path.to_string_lossy().to_string();
        report.downloaded += 1;
        if verbose {
            eprintln!(
                "Downloaded canonical save for {} and wrote local container {}",
                target_save_path.display(),
                normalized_save.local_container.as_str()
            );
        }

        return Ok(Some(processed_entry(
            state_key.clone(),
            synced_entry(
                sha256_bytes(&canonical_bytes),
                Some(active_rom_sha1.clone()),
                latest.version,
                Some(&system_slug),
                Some(normalized_save.local_container),
                Some(normalized_save.adapter_profile),
                Some(source_kind),
                Some(source_name),
                Some(&effective_slot_name),
            ),
        )));
    }

    report.skipped += 1;
    if verbose {
        eprintln!("Cloud save had no ID and no conflict path for {}", save_key);
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn process_missing_save(
    api: &ApiClient,
    save_path: &Path,
    existing_entry: &SyncedEntry,
    source_name: &str,
    source_kind: &SourceKind,
    _source_profile: &EmulatorProfile,
    options: &SyncOptions,
    report: &mut SyncReport,
    verbose: bool,
) -> Result<Option<ProcessedEntry>> {
    let Some(rom_sha1) = existing_entry.rom_sha1.as_deref() else {
        report.skipped += 1;
        if verbose {
            eprintln!(
                "Skipping missing save restore for {}: no ROM SHA1 in state",
                save_path.display()
            );
        }
        return Ok(None);
    };

    let system_slug = existing_entry.system_slug.as_deref();
    let effective_slot_name = existing_entry.slot_name.clone().unwrap_or_else(|| {
        resolve_slot_name_for_sync(system_slug.unwrap_or(""), save_path, &options.slot_name)
    });

    let latest = api.latest_save(rom_sha1, &effective_slot_name)?;
    if !latest.exists {
        report.skipped += 1;
        if verbose {
            eprintln!(
                "Skipping missing save restore for {}: no cloud save found",
                save_path.display()
            );
        }
        return Ok(None);
    }

    let Some(save_id) = latest.id.as_deref() else {
        report.skipped += 1;
        if verbose {
            eprintln!(
                "Skipping missing save restore for {}: cloud save has no downloadable ID",
                save_path.display()
            );
        }
        return Ok(None);
    };

    let local_container = existing_entry
        .local_container
        .unwrap_or(SaveContainerFormat::Native);
    let adapter_profile = existing_entry
        .adapter_profile
        .unwrap_or_else(|| default_adapter_profile_for_container(local_container));
    let state_key = save_path.to_string_lossy().to_string();

    if options.dry_run {
        report.downloaded += 1;
        return Ok(Some(processed_entry(
            state_key,
            synced_entry(
                existing_entry.sha256.clone(),
                Some(rom_sha1.to_string()),
                latest.version,
                system_slug,
                Some(local_container),
                Some(adapter_profile),
                Some(source_kind),
                Some(source_name),
                Some(&effective_slot_name),
            ),
        )));
    }

    let canonical_bytes = api.download_save(save_id)?;
    let local_bytes = encode_download_for_local_container(&canonical_bytes, local_container)?;
    if let Some(parent) = save_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("kan map niet maken: {}", parent.display()))?;
    }
    fs::write(save_path, &local_bytes).with_context(|| {
        format!(
            "kan ontbrekend save bestand niet herstellen: {}",
            save_path.display()
        )
    })?;
    report.downloaded += 1;
    if verbose {
        eprintln!(
            "Restored missing save {} using adapter {} and container {}",
            save_path.display(),
            adapter_profile.as_str(),
            local_container.as_str(),
        );
    }

    Ok(Some(processed_entry(
        state_key,
        synced_entry(
            sha256_bytes(&canonical_bytes),
            Some(rom_sha1.to_string()),
            latest.version,
            system_slug,
            Some(local_container),
            Some(adapter_profile),
            Some(source_kind),
            Some(source_name),
            Some(&effective_slot_name),
        ),
    )))
}

fn processed_entry(state_key: String, entry: SyncedEntry) -> ProcessedEntry {
    ProcessedEntry { state_key, entry }
}

fn is_playstation_system(system_slug: &str) -> bool {
    matches!(system_slug, "psx" | "ps2")
}

fn helper_device_type_for_upload(
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    system_slug: &str,
) -> &'static str {
    if system_slug == "psx" {
        return if matches!(source_profile, EmulatorProfile::Mister)
            || matches!(source_kind, SourceKind::MisterFpga)
        {
            "mister"
        } else {
            "retroarch"
        };
    }
    if system_slug == "ps2" {
        return "pcsx2";
    }

    match source_kind {
        SourceKind::MisterFpga => "mister",
        SourceKind::RetroArch => "retroarch",
        SourceKind::Custom => "custom",
        SourceKind::OpenEmu => "openemu",
        SourceKind::AnaloguePocket => "analogue-pocket",
        SourceKind::Windows => "windows",
        SourceKind::SteamDeck => "steamdeck",
    }
}

fn resolve_slot_name_for_sync(
    system_slug: &str,
    save_path: &Path,
    configured_slot: &str,
) -> String {
    if !is_playstation_system(system_slug) {
        return configured_slot.to_string();
    }

    if let Some(slot) = parse_playstation_slot(configured_slot) {
        return slot;
    }
    infer_playstation_slot_from_path(save_path)
}

fn parse_playstation_slot(value: &str) -> Option<String> {
    let text = value.trim().to_ascii_lowercase();
    if text.is_empty() || text == "default" {
        return None;
    }

    if text.contains("memory card 1")
        || text.contains("memory_card_1")
        || text.contains("slot 1")
        || text.contains("slot1")
        || text.contains("card 1")
        || text.contains("card1")
    {
        return Some("Memory Card 1".to_string());
    }
    if text.contains("memory card 2")
        || text.contains("memory_card_2")
        || text.contains("slot 2")
        || text.contains("slot2")
        || text.contains("card 2")
        || text.contains("card2")
    {
        return Some("Memory Card 2".to_string());
    }

    if text.contains("mcd001") || text.contains("mcd1") {
        return Some("Memory Card 1".to_string());
    }
    if text.contains("mcd002") || text.contains("mcd2") {
        return Some("Memory Card 2".to_string());
    }

    None
}

fn infer_playstation_slot_from_path(path: &Path) -> String {
    let text = path.to_string_lossy().to_ascii_lowercase();
    parse_playstation_slot(&text).unwrap_or_else(|| "Memory Card 1".to_string())
}

fn playstation_line_key(system_slug: &str, device_type: &str, slot_name: &str) -> String {
    let normalized_slot = slot_name
        .trim()
        .to_ascii_lowercase()
        .replace("memory card", "memory-card")
        .replace(' ', "-");
    format!(
        "ps-line:{}:{}:{}",
        system_slug, device_type, normalized_slot
    )
}

fn select_preferred_save_per_stem(
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    save_files: &[PathBuf],
    rom_index: &HashMap<String, RomIndexEntry>,
) -> HashMap<String, PathBuf> {
    let mut selected: HashMap<String, (PathBuf, u8)> = HashMap::new();

    for save_path in save_files {
        let stem_key = filename_stem(save_path).to_ascii_lowercase();
        let rom_path = rom_index.get(&stem_key).map(|entry| entry.path.as_path());
        let Some(classification) = classify_supported_save(save_path, rom_path) else {
            continue;
        };

        let extension = save_extension(save_path);
        let save_size = save_path.metadata().ok().map(|meta| meta.len());
        let score = preferred_extension_for_system(
            source_kind,
            source_profile,
            &classification.system_slug,
            save_size,
        )
        .map(|preferred| {
            if extension.as_deref() == Some(preferred) {
                2
            } else {
                1
            }
        })
        .unwrap_or(1);

        match selected.get_mut(&stem_key) {
            Some((existing_path, existing_score)) => {
                if score > *existing_score {
                    *existing_path = save_path.clone();
                    *existing_score = score;
                }
            }
            None => {
                selected.insert(stem_key, (save_path.clone(), score));
            }
        }
    }

    selected
        .into_iter()
        .map(|(stem, (path, _))| (stem, path))
        .collect()
}

fn preferred_save_path(
    save_path: &Path,
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    system_slug: Option<&str>,
    local_container: SaveContainerFormat,
    canonical_size: Option<u64>,
) -> PathBuf {
    if local_container != SaveContainerFormat::Native {
        return save_path.to_path_buf();
    }
    let Some(system_slug) = system_slug else {
        return save_path.to_path_buf();
    };
    let Some(preferred_extension) =
        preferred_extension_for_system(source_kind, source_profile, system_slug, canonical_size)
    else {
        return save_path.to_path_buf();
    };
    if save_extension(save_path).as_deref() == Some(preferred_extension) {
        return save_path.to_path_buf();
    }
    let mut target = save_path.to_path_buf();
    target.set_extension(preferred_extension);
    target
}

fn preferred_extension_for_system(
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    system_slug: &str,
    save_size: Option<u64>,
) -> Option<&'static str> {
    match system_slug {
        "nes" | "snes" | "gameboy" | "gba" | "genesis" | "master-system" | "game-gear"
        | "neogeo" => preferred_extension_for_cartridge(source_profile),
        "n64" => preferred_extension_for_n64(source_kind, source_profile, save_size),
        "nds" | "psp" | "psvita" | "ps3" | "ps4" | "ps5" => Some("sav"),
        "ps2" => Some("ps2"),
        _ => None,
    }
}

fn preferred_extension_for_cartridge(source_profile: &EmulatorProfile) -> Option<&'static str> {
    match source_profile {
        EmulatorProfile::Mister => Some("sav"),
        EmulatorProfile::RetroArch
        | EmulatorProfile::Snes9x
        | EmulatorProfile::Zsnes
        | EmulatorProfile::EverDrive
        | EmulatorProfile::Generic => Some("srm"),
    }
}

fn preferred_extension_for_n64(
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    save_size: Option<u64>,
) -> Option<&'static str> {
    if !matches!(
        source_profile,
        EmulatorProfile::Mister | EmulatorProfile::EverDrive
    ) && !matches!(source_kind, SourceKind::MisterFpga)
    {
        return Some("sav");
    }

    match save_size {
        Some(512) | Some(2_048) => Some("eep"),
        Some(32_768) => Some("sra"),
        Some(131_072) => Some("fla"),
        Some(786_432) => Some("sav"),
        _ => None,
    }
}

fn save_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
}

#[allow(clippy::too_many_arguments)]
fn handle_conflict(
    api: &ApiClient,
    save_path: &std::path::Path,
    canonical_bytes: &[u8],
    local_sha: &str,
    rom_sha1: &str,
    slot_name: &str,
    dry_run: bool,
    conflict: &ConflictCheckResponse,
    source_name: &str,
    source_kind: &SourceKind,
    app_password: Option<&str>,
) -> Result<()> {
    if dry_run {
        return Ok(());
    }

    let file_name = save_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("save.bin");
    let cloud_sha = conflict
        .cloud_sha256
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    let device_name = format!("{} ({})", source_kind.as_str(), source_name);

    let _ = api.conflict_report(
        file_name,
        canonical_bytes.to_vec(),
        rom_sha1,
        slot_name,
        local_sha,
        &cloud_sha,
        &device_name,
        app_password,
    )?;

    Ok(())
}

fn default_adapter_profile_for_container(container: SaveContainerFormat) -> SaveAdapterProfile {
    match container {
        SaveContainerFormat::Native => SaveAdapterProfile::Identity,
        SaveContainerFormat::Ps1Raw => SaveAdapterProfile::Ps1Raw,
        SaveContainerFormat::Ps1DexDrive => SaveAdapterProfile::Ps1DexDrive,
        SaveContainerFormat::Ps1Vmp => SaveAdapterProfile::Ps1Vmp,
    }
}

#[allow(clippy::too_many_arguments)]
fn synced_entry(
    sha256: String,
    rom_sha1: Option<String>,
    version: Option<i64>,
    system_slug: Option<&str>,
    local_container: Option<SaveContainerFormat>,
    adapter_profile: Option<SaveAdapterProfile>,
    source_kind: Option<&SourceKind>,
    source_name: Option<&str>,
    slot_name: Option<&str>,
) -> SyncedEntry {
    SyncedEntry {
        sha256,
        rom_sha1,
        version,
        system_slug: system_slug.map(ToString::to_string),
        local_container,
        adapter_profile,
        source_kind: source_kind.map(|kind| kind.as_str().to_string()),
        source_name: source_name.map(ToString::to_string),
        slot_name: slot_name.map(ToString::to_string),
        updated_at: now_rfc3339(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_sav_for_mister_snes() {
        let ext = preferred_extension_for_system(
            &SourceKind::MisterFpga,
            &EmulatorProfile::Mister,
            "snes",
            None,
        );
        assert_eq!(ext, Some("sav"));
    }

    #[test]
    fn prefers_srm_for_retroarch_snes() {
        let ext = preferred_extension_for_system(
            &SourceKind::RetroArch,
            &EmulatorProfile::RetroArch,
            "snes",
            None,
        );
        assert_eq!(ext, Some("srm"));
    }

    #[test]
    fn prefers_srm_for_zsnes_profile() {
        let ext = preferred_extension_for_system(
            &SourceKind::Custom,
            &EmulatorProfile::Zsnes,
            "snes",
            None,
        );
        assert_eq!(ext, Some("srm"));
    }

    #[test]
    fn rewrites_native_save_to_preferred_extension() {
        let path = PathBuf::from("/userdata/saves/snes/zelda.srm");
        let target = preferred_save_path(
            &path,
            &SourceKind::MisterFpga,
            &EmulatorProfile::Mister,
            Some("snes"),
            SaveContainerFormat::Native,
            None,
        );
        assert_eq!(target.to_string_lossy(), "/userdata/saves/snes/zelda.sav");
    }

    #[test]
    fn does_not_rewrite_non_native_container() {
        let path = PathBuf::from("/userdata/saves/psx/card.srm");
        let target = preferred_save_path(
            &path,
            &SourceKind::MisterFpga,
            &EmulatorProfile::Mister,
            Some("psx"),
            SaveContainerFormat::Ps1Raw,
            None,
        );
        assert_eq!(target, path);
    }

    #[test]
    fn prefers_n64_native_extension_on_mister_by_size() {
        assert_eq!(
            preferred_extension_for_system(
                &SourceKind::MisterFpga,
                &EmulatorProfile::Mister,
                "n64",
                Some(512)
            ),
            Some("eep")
        );
        assert_eq!(
            preferred_extension_for_system(
                &SourceKind::MisterFpga,
                &EmulatorProfile::Mister,
                "n64",
                Some(32_768)
            ),
            Some("sra")
        );
        assert_eq!(
            preferred_extension_for_system(
                &SourceKind::MisterFpga,
                &EmulatorProfile::Mister,
                "n64",
                Some(131_072)
            ),
            Some("fla")
        );
    }

    #[test]
    fn prefers_n64_sav_for_non_mister_sources() {
        assert_eq!(
            preferred_extension_for_system(
                &SourceKind::RetroArch,
                &EmulatorProfile::RetroArch,
                "n64",
                Some(32_768)
            ),
            Some("sav")
        );
    }

    #[test]
    fn prefers_n64_native_extension_for_everdrive_profile() {
        assert_eq!(
            preferred_extension_for_system(
                &SourceKind::Custom,
                &EmulatorProfile::EverDrive,
                "n64",
                Some(32_768)
            ),
            Some("sra")
        );
    }

    #[test]
    fn parses_playstation_slot_aliases() {
        assert_eq!(
            parse_playstation_slot("memory_card_1"),
            Some("Memory Card 1".to_string())
        );
        assert_eq!(
            parse_playstation_slot("memory card 2"),
            Some("Memory Card 2".to_string())
        );
        assert_eq!(
            parse_playstation_slot("Mcd001.ps2"),
            Some("Memory Card 1".to_string())
        );
        assert_eq!(
            parse_playstation_slot("Mcd002.ps2"),
            Some("Memory Card 2".to_string())
        );
    }

    #[test]
    fn infers_playstation_slot_from_path_with_default() {
        assert_eq!(
            infer_playstation_slot_from_path(Path::new("/games/psx/memory_card_2.mcd")),
            "Memory Card 2".to_string()
        );
        assert_eq!(
            infer_playstation_slot_from_path(Path::new("/games/ps2/custom.ps2")),
            "Memory Card 1".to_string()
        );
    }
}
