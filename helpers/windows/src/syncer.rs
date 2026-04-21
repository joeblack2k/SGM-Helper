use std::fs;

use anyhow::{Context, Result};

use crate::api::{ApiClient, ConflictCheckResponse};
use crate::config::AppConfig;
use crate::scanner::{discover_save_files, filename_stem, sha256_bytes, sha256_file};
use crate::state::{AuthState, SyncedEntry, load_sync_state, now_rfc3339, save_sync_state};

#[derive(Debug, Clone)]
pub struct SyncOptions {
    pub force_upload: bool,
    pub dry_run: bool,
    pub slot_name: String,
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
    let root = config.resolved_root()?;
    if !root.exists() {
        anyhow::bail!("Windows save root directory not found: {}", root.display());
    }

    let state_dir = config.resolved_state_dir()?;
    let mut sync_state = load_sync_state(&state_dir)?;

    let token = auth.map(|value| value.token.clone());
    let api = ApiClient::new(config.base_url(), config.route_prefix.clone(), token)?;

    let fingerprint = hostname::get()
        .ok()
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| "mister".to_string());

    let mut report = SyncReport::default();
    let files = discover_save_files(&root)?;
    report.scanned = files.len();

    for save_path in files {
        let save_key = save_path.to_string_lossy().to_string();
        let process_result = process_single_save(
            &api,
            &save_path,
            &save_key,
            &fingerprint,
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

    if !options.dry_run {
        save_sync_state(&state_dir, &sync_state)?;
    }

    Ok(report)
}

fn process_single_save(
    api: &ApiClient,
    save_path: &std::path::Path,
    save_key: &str,
    fingerprint: &str,
    options: &SyncOptions,
    report: &mut SyncReport,
    verbose: bool,
) -> Result<Option<SyncedEntry>> {
    let local_sha = sha256_file(save_path)?;
    let stem = filename_stem(save_path);

    let lookup = api.lookup_rom(&stem)?;
    let Some(rom) = lookup.rom else {
        report.skipped += 1;
        if verbose {
            eprintln!("No ROM found on backend for {}", stem);
        }
        return Ok(None);
    };
    let Some(rom_sha1) = rom.sha1 else {
        report.skipped += 1;
        if verbose {
            eprintln!("Backend ROM has no SHA1 hash for {}", stem);
        }
        return Ok(None);
    };

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
            rom.md5.as_deref(),
            &options.slot_name,
            fingerprint,
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
            rom.md5.as_deref(),
            &options.slot_name,
            fingerprint,
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
        handle_conflict(api, save_path, &local_sha, &rom_sha1, options, &conflict)?;
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

fn handle_conflict(
    api: &ApiClient,
    save_path: &std::path::Path,
    local_sha: &str,
    rom_sha1: &str,
    options: &SyncOptions,
    conflict: &ConflictCheckResponse,
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

    api.conflict_report(
        file_name,
        bytes,
        rom_sha1,
        &options.slot_name,
        local_sha,
        &cloud_sha,
        "Windows",
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
