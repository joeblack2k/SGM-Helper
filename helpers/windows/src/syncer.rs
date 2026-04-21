use std::collections::HashMap;
use std::fs;

use anyhow::{Context, Result};

use crate::api::{ApiClient, ConflictCheckResponse};
use crate::config::AppConfig;
use crate::scanner::{
    RomIndexEntry, discover_rom_index, discover_save_files, filename_stem, infer_system_slug,
    md5_file, sha1_file, sha256_bytes, sha256_file,
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
    }

    if !options.dry_run {
        save_sync_state(&state_dir, &sync_state)?;
    }

    Ok(report)
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
    let local_sha = sha256_file(save_path)?;
    let stem = filename_stem(save_path);
    let stem_key = stem.to_ascii_lowercase();

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
        && let Some(rom_entry) = rom_index.get(&stem_key)
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

    let system_slug = infer_system_slug(save_path);
    let latest = api.latest_save(&rom_sha1, &options.slot_name)?;

    if !latest.exists {
        if options.dry_run {
            report.uploaded += 1;
            return Ok(Some(synced_entry(local_sha, Some(rom_sha1), None)));
        }

        let bytes = fs::read(save_path)
            .with_context(|| format!("kan save bestand niet lezen: {}", save_path.display()))?;
        let filename = save_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("save.bin");

        let _upload = api.upload_save(
            filename,
            bytes,
            &rom_sha1,
            rom_md5.as_deref(),
            &options.slot_name,
            fingerprint,
            system_slug.as_deref(),
        )?;

        report.uploaded += 1;
        return Ok(Some(synced_entry(
            local_sha,
            Some(rom_sha1),
            latest.version,
        )));
    }

    if latest.sha256.as_deref() == Some(local_sha.as_str()) {
        report.in_sync += 1;
        return Ok(Some(synced_entry(
            local_sha,
            Some(rom_sha1),
            latest.version,
        )));
    }

    if options.force_upload {
        if options.dry_run {
            report.uploaded += 1;
            return Ok(Some(synced_entry(
                local_sha,
                Some(rom_sha1),
                latest.version,
            )));
        }

        let bytes = fs::read(save_path)
            .with_context(|| format!("kan save bestand niet lezen: {}", save_path.display()))?;
        let filename = save_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("save.bin");

        api.upload_save(
            filename,
            bytes,
            &rom_sha1,
            rom_md5.as_deref(),
            &options.slot_name,
            fingerprint,
            system_slug.as_deref(),
        )?;
        report.uploaded += 1;
        return Ok(Some(synced_entry(
            local_sha,
            Some(rom_sha1),
            latest.version,
        )));
    }

    let conflict = api.conflict_check(&rom_sha1, &options.slot_name)?;
    if conflict.exists {
        handle_conflict(
            api,
            save_path,
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
        )));
    }

    if let Some(save_id) = latest.id {
        if options.dry_run {
            report.downloaded += 1;
            return Ok(Some(synced_entry(
                local_sha,
                Some(rom_sha1),
                latest.version,
            )));
        }

        let bytes = api.download_save(&save_id)?;
        if let Some(parent) = save_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("kan map niet maken: {}", parent.display()))?;
        }
        fs::write(save_path, &bytes).with_context(|| {
            format!(
                "kan save bestand niet overschrijven: {}",
                save_path.display()
            )
        })?;
        report.downloaded += 1;

        return Ok(Some(synced_entry(
            sha256_bytes(&bytes),
            Some(rom_sha1),
            latest.version,
        )));
    }

    report.skipped += 1;
    if verbose {
        eprintln!("Cloud save had no ID and no conflict path for {}", save_key);
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn handle_conflict(
    api: &ApiClient,
    save_path: &std::path::Path,
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

    let bytes = fs::read(save_path).with_context(|| {
        format!(
            "kan save bestand niet lezen voor conflict report: {}",
            save_path.display()
        )
    })?;
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
        bytes,
        rom_sha1,
        &options.slot_name,
        local_sha,
        &cloud_sha,
        &device_name,
    )?;

    Ok(())
}

fn synced_entry(sha256: String, rom_sha1: Option<String>, version: Option<i64>) -> SyncedEntry {
    SyncedEntry {
        sha256,
        rom_sha1,
        version,
        updated_at: now_rfc3339(),
    }
}
