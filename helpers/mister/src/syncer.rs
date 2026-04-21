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
use crate::sources::{SourceKind, load_source_store, resolved_sources_or_default};
use crate::state::{AuthState, SyncedEntry, load_sync_state, now_rfc3339, save_sync_state};

#[derive(Debug, Clone)]
pub struct SyncOptions {
    pub force_upload: bool,
    pub dry_run: bool,
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

pub fn run_sync(
    config: &AppConfig,
    auth: Option<&AuthState>,
    options: &SyncOptions,
    verbose: bool,
) -> Result<SyncReport> {
    let state_dir = config.resolved_state_dir()?;
    let mut sync_state = load_sync_state(&state_dir)?;

    let token = auth.map(|value| value.token.clone());
    let api = ApiClient::new(config.base_url(), config.route_prefix.clone(), token)?;

    let source_store = load_source_store(&state_dir)?;
    let sources =
        resolved_sources_or_default(&source_store, config, options.default_source_kind.clone())?;

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
        report.scanned += save_files.len();

        let fingerprint = hostname::get()
            .ok()
            .and_then(|value| value.into_string().ok())
            .unwrap_or_else(|| source.kind.as_str().to_string());

        for save_path in save_files {
            let save_key = save_path.to_string_lossy().to_string();
            let process_result = process_single_save(
                &api,
                &save_path,
                &save_key,
                &fingerprint,
                &source.name,
                &source.kind,
                &rom_index,
                &mut rom_hash_cache,
                options,
                &mut report,
                verbose,
            );

            match process_result {
                Ok(entry) => {
                    if let Some(entry) = entry {
                        sync_state.entries.insert(save_key, entry);
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
                if candidate.exists() || !path_is_under_roots(&candidate, &source.save_roots) {
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
                options,
                &mut report,
                verbose,
            );

            match restore {
                Ok(entry) => {
                    if let Some(entry) = entry {
                        sync_state.entries.insert(save_key, entry);
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
    rom_index: &HashMap<String, RomIndexEntry>,
    rom_hash_cache: &mut HashMap<String, (String, String)>,
    options: &SyncOptions,
    report: &mut SyncReport,
    verbose: bool,
) -> Result<Option<SyncedEntry>> {
    let stem = filename_stem(save_path);
    let stem_key = stem.to_ascii_lowercase();
    let rom_entry = rom_index.get(&stem_key);
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

    let lookup = api.lookup_rom(&stem).ok();
    let mut rom_sha1 = lookup
        .as_ref()
        .and_then(|value| value.rom.as_ref())
        .and_then(|value| value.sha1.clone());
    let mut rom_md5 = lookup
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

    let Some(rom_sha1) = rom_sha1 else {
        report.skipped += 1;
        if verbose {
            eprintln!("No ROM mapping found for save {}", stem);
        }
        return Ok(None);
    };

    let latest = api.latest_save(&rom_sha1, &options.slot_name)?;

    if !latest.exists {
        if options.dry_run {
            report.uploaded += 1;
            return Ok(Some(synced_entry(
                local_sha,
                Some(rom_sha1),
                None,
                Some(&system_slug),
                Some(normalized_save.local_container),
                Some(normalized_save.adapter_profile),
                Some(source_kind),
                Some(source_name),
            )));
        }

        let filename = save_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("save.bin");

        let _upload = api.upload_save(
            filename,
            normalized_save.canonical_bytes.clone(),
            &rom_sha1,
            rom_md5.as_deref(),
            &options.slot_name,
            fingerprint,
            Some(&system_slug),
        )?;

        report.uploaded += 1;
        return Ok(Some(synced_entry(
            local_sha,
            Some(rom_sha1),
            latest.version,
            Some(&system_slug),
            Some(normalized_save.local_container),
            Some(normalized_save.adapter_profile),
            Some(source_kind),
            Some(source_name),
        )));
    }

    if latest.sha256.as_deref() == Some(local_sha.as_str()) {
        report.in_sync += 1;
        return Ok(Some(synced_entry(
            local_sha,
            Some(rom_sha1),
            latest.version,
            Some(&system_slug),
            Some(normalized_save.local_container),
            Some(normalized_save.adapter_profile),
            Some(source_kind),
            Some(source_name),
        )));
    }

    if options.force_upload {
        if options.dry_run {
            report.uploaded += 1;
            return Ok(Some(synced_entry(
                local_sha,
                Some(rom_sha1),
                latest.version,
                Some(&system_slug),
                Some(normalized_save.local_container),
                Some(normalized_save.adapter_profile),
                Some(source_kind),
                Some(source_name),
            )));
        }

        let filename = save_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("save.bin");

        api.upload_save(
            filename,
            normalized_save.canonical_bytes.clone(),
            &rom_sha1,
            rom_md5.as_deref(),
            &options.slot_name,
            fingerprint,
            Some(&system_slug),
        )?;
        report.uploaded += 1;
        return Ok(Some(synced_entry(
            local_sha,
            Some(rom_sha1),
            latest.version,
            Some(&system_slug),
            Some(normalized_save.local_container),
            Some(normalized_save.adapter_profile),
            Some(source_kind),
            Some(source_name),
        )));
    }

    let conflict = api.conflict_check(&rom_sha1, &options.slot_name)?;
    if conflict.exists {
        handle_conflict(
            api,
            save_path,
            &normalized_save.canonical_bytes,
            &local_sha,
            &rom_sha1,
            options,
            &conflict,
            source_name,
            source_kind,
        )?;
        report.conflicts += 1;
        return Ok(Some(synced_entry(
            local_sha,
            Some(rom_sha1),
            latest.version,
            Some(&system_slug),
            Some(normalized_save.local_container),
            Some(normalized_save.adapter_profile),
            Some(source_kind),
            Some(source_name),
        )));
    }

    if let Some(save_id) = latest.id {
        if options.dry_run {
            report.downloaded += 1;
            return Ok(Some(synced_entry(
                local_sha,
                Some(rom_sha1),
                latest.version,
                Some(&system_slug),
                Some(normalized_save.local_container),
                Some(normalized_save.adapter_profile),
                Some(source_kind),
                Some(source_name),
            )));
        }

        let canonical_bytes = api.download_save(&save_id)?;
        let local_bytes =
            encode_download_for_local_container(&canonical_bytes, normalized_save.local_container)?;
        if let Some(parent) = save_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("kan map niet maken: {}", parent.display()))?;
        }
        fs::write(save_path, &local_bytes).with_context(|| {
            format!(
                "kan save bestand niet overschrijven: {}",
                save_path.display()
            )
        })?;
        report.downloaded += 1;
        if verbose {
            eprintln!(
                "Downloaded canonical save for {} and wrote local container {}",
                save_path.display(),
                normalized_save.local_container.as_str()
            );
        }

        return Ok(Some(synced_entry(
            sha256_bytes(&canonical_bytes),
            Some(rom_sha1),
            latest.version,
            Some(&system_slug),
            Some(normalized_save.local_container),
            Some(normalized_save.adapter_profile),
            Some(source_kind),
            Some(source_name),
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
    options: &SyncOptions,
    report: &mut SyncReport,
    verbose: bool,
) -> Result<Option<SyncedEntry>> {
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

    let latest = api.latest_save(rom_sha1, &options.slot_name)?;
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
    let system_slug = existing_entry.system_slug.as_deref();

    if options.dry_run {
        report.downloaded += 1;
        return Ok(Some(synced_entry(
            existing_entry.sha256.clone(),
            Some(rom_sha1.to_string()),
            latest.version,
            system_slug,
            Some(local_container),
            Some(adapter_profile),
            Some(source_kind),
            Some(source_name),
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

    Ok(Some(synced_entry(
        sha256_bytes(&canonical_bytes),
        Some(rom_sha1.to_string()),
        latest.version,
        system_slug,
        Some(local_container),
        Some(adapter_profile),
        Some(source_kind),
        Some(source_name),
    )))
}

#[allow(clippy::too_many_arguments)]
fn handle_conflict(
    api: &ApiClient,
    save_path: &std::path::Path,
    canonical_bytes: &[u8],
    local_sha: &str,
    rom_sha1: &str,
    options: &SyncOptions,
    conflict: &ConflictCheckResponse,
    source_name: &str,
    source_kind: &SourceKind,
) -> Result<()> {
    if options.dry_run {
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
        &options.slot_name,
        local_sha,
        &cloud_sha,
        &device_name,
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
        updated_at: now_rfc3339(),
    }
}
