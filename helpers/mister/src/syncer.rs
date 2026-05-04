use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::api::{
    ApiClient, CloudSaveSummary, ConflictCheckResponse, LatestSaveResponse, RuntimeTarget,
};
use crate::backend_config::sync_config_with_backend;
use crate::config::AppConfig;
use crate::scanner::{
    RomIndexEntry, SaveAdapterProfile, SaveContainerFormat, classify_supported_save,
    discover_rom_index, discover_save_files, dreamcast_skip_reason,
    encode_download_for_local_container, filename_stem, md5_file, normalize_save_bytes_for_sync,
    normalize_save_for_sync, saturn_skip_reason, sha1_file, sha256_bytes, wii_skip_reason,
    wii_title_code_from_path,
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

    let mut sources = prepare_sources_for_sync(
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
    let mut effective_options = options.clone();
    match sync_config_with_backend(
        &api,
        config,
        &mut sources,
        &options.default_source_kind,
        app_password,
        verbose,
    ) {
        Ok(overrides) => {
            if let Some(value) = overrides.force_upload {
                effective_options.force_upload = value;
            }
            if let Some(value) = overrides.dry_run {
                effective_options.dry_run = value;
            }
        }
        Err(err) => {
            if verbose {
                eprintln!("Backend config sync skipped: {:#}", err);
            }
        }
    }
    let options = &effective_options;

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
            let selection_key = save_selection_key(&save_path);
            if let Some(preferred_path) = preferred_save_by_stem.get(&selection_key)
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
                &source.systems,
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
                &fingerprint,
                &entry,
                &source.name,
                &source.kind,
                &source.profile,
                &source.save_roots,
                &source.systems,
                source.create_missing_system_dirs,
                app_password,
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

        restore_cloud_only_saves(
            &api,
            &source.name,
            &source.kind,
            &source.profile,
            &source.save_roots,
            &source.systems,
            source.create_missing_system_dirs,
            &fingerprint,
            app_password,
            options,
            &mut sync_state.entries,
            &mut report,
            verbose,
        )?;
    }

    if !options.dry_run {
        save_sync_state(&state_dir, &sync_state)?;
    }

    Ok(report)
}

fn path_is_under_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

fn source_allows_system(source_systems: &[String], system_slug: &str) -> bool {
    let clean = system_slug.trim().to_ascii_lowercase();
    if clean.is_empty() {
        return false;
    }
    source_systems
        .iter()
        .any(|system| system == "*" || system.eq_ignore_ascii_case(&clean))
}

fn target_parent_allowed(
    target_path: &Path,
    save_roots: &[PathBuf],
    create_missing_system_dirs: bool,
) -> bool {
    if create_missing_system_dirs {
        return true;
    }
    if target_path.parent().map(Path::exists).unwrap_or(false) {
        return true;
    }

    save_roots.iter().any(|root| {
        if !root.exists() {
            return false;
        }
        let Ok(relative) = target_path.strip_prefix(root) else {
            return false;
        };
        let mut components = relative.components();
        let Some(first) = components.next() else {
            return true;
        };
        if components.next().is_none() {
            return true;
        }
        root.join(first.as_os_str()).is_dir()
    })
}

#[allow(clippy::too_many_arguments)]
fn restore_cloud_only_saves(
    api: &ApiClient,
    source_name: &str,
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    save_roots: &[PathBuf],
    source_systems: &[String],
    create_missing_system_dirs: bool,
    fingerprint: &str,
    app_password: Option<&str>,
    options: &SyncOptions,
    sync_entries: &mut HashMap<String, SyncedEntry>,
    report: &mut SyncReport,
    verbose: bool,
) -> Result<()> {
    if save_roots.is_empty() {
        return Ok(());
    }

    let mut offset = 0usize;
    let limit = 100usize;
    let mut restored_targets = HashSet::new();

    loop {
        let page = match api.list_saves(limit, offset, app_password) {
            Ok(page) => page,
            Err(err) => {
                if verbose {
                    eprintln!(
                        "Cloud restore skipped for source '{}': /saves unavailable ({})",
                        source_name, err
                    );
                }
                return Ok(());
            }
        };

        if page.saves.is_empty() {
            break;
        }

        if verbose && offset == 0 {
            eprintln!(
                "Cloud restore scan for source '{}' saw {} save track(s)",
                source_name, page.total
            );
        }

        let page_len = page.saves.len();
        for cloud_save in page.saves {
            match restore_single_cloud_save(
                api,
                &cloud_save,
                source_name,
                source_kind,
                source_profile,
                save_roots,
                source_systems,
                create_missing_system_dirs,
                fingerprint,
                app_password,
                options,
                sync_entries,
                &mut restored_targets,
                report,
                verbose,
            ) {
                Ok(()) => {}
                Err(err) => {
                    report.errors += 1;
                    if verbose {
                        let label = cloud_save.display_name();
                        eprintln!("Cloud restore error for '{}': {}", label, err);
                    }
                }
            }
        }

        offset += page_len;
        if page.total > 0 {
            if offset >= page.total {
                break;
            }
        } else if page_len < limit {
            break;
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn restore_single_cloud_save(
    api: &ApiClient,
    cloud_save: &CloudSaveSummary,
    source_name: &str,
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    save_roots: &[PathBuf],
    source_systems: &[String],
    create_missing_system_dirs: bool,
    fingerprint: &str,
    app_password: Option<&str>,
    options: &SyncOptions,
    sync_entries: &mut HashMap<String, SyncedEntry>,
    restored_targets: &mut HashSet<String>,
    report: &mut SyncReport,
    verbose: bool,
) -> Result<()> {
    let Some(system_slug) = cloud_save
        .system_slug()
        .map(|value| value.to_ascii_lowercase())
    else {
        report.skipped += 1;
        return Ok(());
    };

    if !supports_cloud_restore_system(&system_slug) {
        report.skipped += 1;
        return Ok(());
    }
    if !source_allows_system(source_systems, &system_slug) {
        report.skipped += 1;
        if verbose {
            eprintln!(
                "Skipping cloud {} save '{}': system is disabled for source '{}'",
                system_slug,
                cloud_save.display_name(),
                source_name
            );
        }
        return Ok(());
    }

    let effective_profile = effective_profile_for_cloud_restore(source_kind, source_profile);
    let device_type = helper_device_type_for_upload(source_kind, &effective_profile, &system_slug);
    let runtime_target =
        runtime_target_for_system(source_kind, &effective_profile, &system_slug, device_type);
    let runtime_profile = runtime_target
        .system_profile_value
        .as_deref()
        .or(runtime_target.runtime_profile.as_deref());
    let target_extension = cloud_target_extension(cloud_save, runtime_profile);
    let provisional_path = cloud_target_path(
        cloud_save,
        save_roots,
        source_kind,
        &effective_profile,
        &system_slug,
        target_extension.as_deref(),
    );

    if existing_local_save_is_valid(&provisional_path, &system_slug) {
        return Ok(());
    }

    let native_target_path = cloud_restore_native_target_path(
        &provisional_path,
        source_kind,
        &effective_profile,
        &system_slug,
        cloud_save,
    );
    if native_target_path != provisional_path
        && existing_local_save_is_valid(&native_target_path, &system_slug)
    {
        return Ok(());
    }
    if !target_parent_allowed(&provisional_path, save_roots, create_missing_system_dirs) {
        report.skipped += 1;
        if verbose {
            eprintln!(
                "Skipping cloud {} save '{}': target system folder is not present for source '{}'",
                system_slug,
                cloud_save.display_name(),
                source_name
            );
        }
        return Ok(());
    }

    let provisional_key = provisional_path.to_string_lossy().to_string();
    if restored_targets.contains(&provisional_key) {
        return Ok(());
    }
    let native_target_key = native_target_path.to_string_lossy().to_string();
    if native_target_key != provisional_key && restored_targets.contains(&native_target_key) {
        return Ok(());
    }

    if options.dry_run {
        report.downloaded += 1;
        return Ok(());
    }

    let downloaded_bytes = match api.download_save(
        &cloud_save.id,
        device_type,
        fingerprint,
        app_password,
        Some(&runtime_target),
    ) {
        Ok(bytes) => bytes,
        Err(err) if is_playstation_projection_unavailable(&err, &system_slug) => {
            report.skipped += 1;
            if verbose {
                eprintln!(
                    "Skipping cloud {} save '{}': backend projection is unavailable ({})",
                    system_slug,
                    cloud_save.display_name(),
                    err
                );
            }
            return Ok(());
        }
        Err(err) => return Err(err),
    };
    let local_container =
        local_container_for_cloud_download(&system_slug, &provisional_path, &downloaded_bytes)?;
    let adapter_profile = default_adapter_profile_for_container(local_container);
    let target_path = preferred_save_path(
        &provisional_path,
        source_kind,
        &effective_profile,
        Some(&system_slug),
        local_container,
        Some(downloaded_bytes.len() as u64),
    );

    if existing_local_save_is_valid(&target_path, &system_slug) {
        return Ok(());
    }
    if !target_parent_allowed(&target_path, save_roots, create_missing_system_dirs) {
        report.skipped += 1;
        if verbose {
            eprintln!(
                "Skipping cloud {} save '{}': final target folder is not present for source '{}'",
                system_slug,
                cloud_save.display_name(),
                source_name
            );
        }
        return Ok(());
    }

    let target_key = target_path.to_string_lossy().to_string();
    if restored_targets.contains(&target_key) {
        return Ok(());
    }

    let local_bytes = encode_download_for_local_container(&downloaded_bytes, local_container)?;
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("kan map niet maken: {}", parent.display()))?;
    }
    fs::write(&target_path, &local_bytes)
        .with_context(|| format!("kan cloud save niet herstellen: {}", target_path.display()))?;

    let slot_name = cloud_save
        .card_slot
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            resolve_slot_name_for_sync(&system_slug, &target_path, &options.slot_name)
        });

    let entry = synced_entry(
        sha256_bytes(&downloaded_bytes),
        cloud_save.rom_sha1.clone(),
        cloud_save.version,
        Some(&system_slug),
        Some(local_container),
        Some(adapter_profile),
        Some(source_kind),
        Some(source_name),
        Some(&slot_name),
    );
    sync_entries.insert(target_key.clone(), entry);
    restored_targets.insert(provisional_key);
    restored_targets.insert(target_key);
    report.downloaded += 1;

    if verbose {
        eprintln!(
            "Restored cloud-only {} save '{}' to {} using profile {}",
            system_slug,
            cloud_save.display_name(),
            target_path.display(),
            runtime_profile.unwrap_or("original")
        );
    }

    Ok(())
}

fn supports_cloud_restore_system(system_slug: &str) -> bool {
    matches!(
        system_slug,
        "nes"
            | "snes"
            | "gameboy"
            | "gba"
            | "n64"
            | "nds"
            | "genesis"
            | "master-system"
            | "game-gear"
            | "sega-cd"
            | "sega-32x"
            | "saturn"
            | "dreamcast"
            | "neogeo"
            | "wii"
            | "psx"
            | "ps2"
            | "psp"
            | "psvita"
            | "ps3"
            | "ps4"
            | "ps5"
    )
}

fn effective_profile_for_cloud_restore(
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
) -> EmulatorProfile {
    match source_kind {
        SourceKind::MisterFpga => EmulatorProfile::Mister,
        SourceKind::RetroArch => EmulatorProfile::RetroArch,
        _ => source_profile.clone(),
    }
}

fn cloud_target_extension(
    cloud_save: &CloudSaveSummary,
    runtime_profile: Option<&str>,
) -> Option<String> {
    if let Some(runtime_profile) = runtime_profile
        && let Some(profile) = cloud_save.download_profiles.iter().find(|profile| {
            profile.id.eq_ignore_ascii_case(runtime_profile)
                && profile
                    .target_extension
                    .as_deref()
                    .map(|value| !value.trim().is_empty())
                    .unwrap_or(false)
        })
    {
        return profile
            .target_extension
            .as_deref()
            .and_then(normalize_extension_value);
    }

    cloud_save
        .download_profiles
        .iter()
        .find(|profile| {
            profile.id.eq_ignore_ascii_case("original")
                && profile
                    .target_extension
                    .as_deref()
                    .map(|value| !value.trim().is_empty())
                    .unwrap_or(false)
        })
        .and_then(|profile| {
            profile
                .target_extension
                .as_deref()
                .and_then(normalize_extension_value)
        })
        .or_else(|| extension_from_filename(&cloud_save.filename))
}

fn normalize_extension_value(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_start_matches('.').trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_ascii_lowercase())
    }
}

fn extension_from_filename(filename: &str) -> Option<String> {
    let path = Path::new(filename.trim());
    path.extension()
        .and_then(|value| value.to_str())
        .and_then(normalize_extension_value)
}

fn cloud_target_path(
    cloud_save: &CloudSaveSummary,
    save_roots: &[PathBuf],
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    system_slug: &str,
    target_extension: Option<&str>,
) -> PathBuf {
    let root = select_cloud_restore_root(save_roots);
    if system_slug == "wii"
        && let Some(title_code) = cloud_wii_title_code(cloud_save)
    {
        let target_dir = if root_is_system_specific(root, source_profile, system_slug) {
            root.join(title_code)
        } else {
            root.join(preferred_system_directory_for_root(
                root,
                source_kind,
                source_profile,
                system_slug,
            ))
            .join(title_code)
        };
        return target_dir.join("data.bin");
    }
    let filename = cloud_restore_filename(cloud_save, target_extension);
    let target_dir = if root_is_system_specific(root, source_profile, system_slug) {
        root.to_path_buf()
    } else {
        if let Some(existing_target) = existing_cloud_target_for_alias(
            root,
            source_kind,
            source_profile,
            system_slug,
            &filename,
        ) {
            return existing_target;
        }
        root.join(preferred_system_directory_for_root(
            root,
            source_kind,
            source_profile,
            system_slug,
        ))
    };
    target_dir.join(filename)
}

fn cloud_wii_title_code(cloud_save: &CloudSaveSummary) -> Option<String> {
    for candidate in [
        cloud_json_string(&cloud_save.metadata, "/rsm/wii/titleCode"),
        cloud_json_string(&cloud_save.metadata, "/rsm/wii/gameCode"),
        cloud_json_string(&cloud_save.metadata, "/rsm/wii/wiiTitleId"),
        cloud_json_string(&cloud_save.metadata, "/rsm/wii/wiiTitleID"),
        cloud_json_string(&cloud_save.metadata, "/rsm/wii/sourcePath"),
        cloud_json_string(&cloud_save.inspection, "/semanticFields/titleCode"),
        cloud_json_string(&cloud_save.inspection, "/semanticFields/gameCode"),
        cloud_json_string(&cloud_save.inspection, "/semanticFields/sourcePath"),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(code) = wii_title_code_from_candidate(&candidate) {
            return Some(code);
        }
    }
    if let Some(code) = cloud_wii_title_code_from_evidence(cloud_save.inspection.as_ref()) {
        return Some(code);
    }
    for candidate in [
        cloud_save.card_slot.as_deref(),
        Some(cloud_save.filename.as_str()),
        Some(cloud_save.display_name()),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(code) = wii_title_code_from_candidate(candidate) {
            return Some(code);
        }
    }
    None
}

fn cloud_json_string(value: &Option<serde_json::Value>, pointer: &str) -> Option<String> {
    value
        .as_ref()
        .and_then(|root| root.pointer(pointer))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn cloud_wii_title_code_from_evidence(value: Option<&serde_json::Value>) -> Option<String> {
    let evidence = value?.pointer("/evidence")?.as_array()?;
    for entry in evidence {
        let Some(text) = entry.as_str() else {
            continue;
        };
        let Some((key, value)) = text.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("titleCode")
            && let Some(code) = wii_title_code_from_candidate(value)
        {
            return Some(code);
        }
    }
    None
}

fn wii_title_code_from_candidate(candidate: &str) -> Option<String> {
    let path = Path::new(candidate);
    if let Some(code) = wii_title_code_from_path(path) {
        return Some(code);
    }
    let clean = candidate.trim().to_ascii_uppercase();
    if clean.len() == 4
        && clean
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
    {
        return Some(clean);
    }
    None
}

fn existing_local_save_is_valid(path: &Path, system_slug: &str) -> bool {
    path.exists()
        && classify_supported_save(path, None)
            .map(|classification| classification.system_slug == system_slug)
            .unwrap_or(false)
}

fn cloud_restore_native_target_path(
    provisional_path: &Path,
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    system_slug: &str,
    cloud_save: &CloudSaveSummary,
) -> PathBuf {
    preferred_save_path(
        provisional_path,
        source_kind,
        source_profile,
        Some(system_slug),
        SaveContainerFormat::Native,
        cloud_save.latest_size_bytes.or(cloud_save.file_size),
    )
}

fn existing_cloud_target_for_alias(
    root: &Path,
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    system_slug: &str,
    filename: &str,
) -> Option<PathBuf> {
    let default_dir = system_directory_for_source(source_kind, source_profile, system_slug);
    let candidates = std::iter::once(default_dir).chain(
        system_directory_aliases(system_slug)
            .iter()
            .copied()
            .filter(move |alias| *alias != default_dir),
    );
    for alias in candidates {
        let candidate = root.join(alias).join(filename);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn preferred_system_directory_for_root(
    root: &Path,
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    system_slug: &str,
) -> String {
    let default_dir = system_directory_for_source(source_kind, source_profile, system_slug);
    if root.join(default_dir).is_dir() {
        return default_dir.to_string();
    }
    for alias in system_directory_aliases(system_slug) {
        if root.join(alias).is_dir() {
            return (*alias).to_string();
        }
    }
    default_dir.to_string()
}

fn select_cloud_restore_root(save_roots: &[PathBuf]) -> &PathBuf {
    save_roots
        .iter()
        .find(|root| root.exists())
        .unwrap_or(&save_roots[0])
}

fn cloud_restore_filename(cloud_save: &CloudSaveSummary, target_extension: Option<&str>) -> String {
    let fallback_name = cloud_save.display_name();
    let raw_name = if cloud_save.filename.trim().is_empty() {
        fallback_name
    } else {
        cloud_save.filename.trim()
    };
    let safe_name = sanitize_filename(raw_name);
    let stem = Path::new(&safe_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .map(sanitize_filename)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| sanitize_filename(fallback_name));

    if let Some(extension) = target_extension.and_then(normalize_extension_value) {
        return format!("{}.{}", stem, extension);
    }

    if Path::new(&safe_name).extension().is_some() {
        safe_name
    } else {
        format!("{}.sav", stem)
    }
}

fn sanitize_filename(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.trim().chars() {
        if ch.is_ascii_control()
            || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    let trimmed = out.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        "save".to_string()
    } else {
        trimmed
    }
}

fn root_is_system_specific(
    root: &Path,
    source_profile: &EmulatorProfile,
    system_slug: &str,
) -> bool {
    if profile_is_system_specific(source_profile, system_slug) {
        return true;
    }

    let Some(name) = root.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let normalized = normalize_path_token(name);
    system_directory_aliases(system_slug)
        .iter()
        .any(|alias| normalize_path_token(alias) == normalized)
}

fn profile_is_system_specific(source_profile: &EmulatorProfile, system_slug: &str) -> bool {
    matches!(
        (source_profile, system_slug),
        (EmulatorProfile::Snes9x, "snes")
            | (EmulatorProfile::Zsnes, "snes")
            | (EmulatorProfile::Project64, "n64")
            | (EmulatorProfile::MupenFamily, "n64")
            | (EmulatorProfile::EverDrive, "n64")
    )
}

fn system_directory_for_source(
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    system_slug: &str,
) -> &'static str {
    if matches!(source_kind, SourceKind::MisterFpga)
        || matches!(source_profile, EmulatorProfile::Mister)
    {
        return mister_system_directory(system_slug);
    }
    generic_system_directory(system_slug)
}

fn mister_system_directory(system_slug: &str) -> &'static str {
    match system_slug {
        "nes" => "NES",
        "snes" => "SNES",
        "gameboy" => "GameBoy",
        "gba" => "GBA",
        "n64" => "N64",
        "nds" => "NDS",
        "genesis" => "MegaDrive",
        "master-system" => "SMS",
        "game-gear" => "GameGear",
        "sega-cd" => "MegaCD",
        "sega-32x" => "MegaDrive",
        "saturn" => "Saturn",
        "dreamcast" => "Dreamcast",
        "neogeo" => "NeoGeo",
        "wii" => "Wii",
        "psx" => "PSX",
        "ps2" => "PS2",
        "psp" => "PSP",
        "psvita" => "PSVita",
        "ps3" => "PS3",
        "ps4" => "PS4",
        "ps5" => "PS5",
        _ => "Other",
    }
}

fn generic_system_directory(system_slug: &str) -> &'static str {
    match system_slug {
        "nes" => "nes",
        "snes" => "snes",
        "gameboy" => "gb",
        "gba" => "gba",
        "n64" => "n64",
        "nds" => "nds",
        "genesis" => "genesis",
        "master-system" => "mastersystem",
        "game-gear" => "gamegear",
        "sega-cd" => "segacd",
        "sega-32x" => "sega32x",
        "saturn" => "saturn",
        "dreamcast" => "dreamcast",
        "neogeo" => "neogeo",
        "wii" => "wii",
        "psx" => "psx",
        "ps2" => "ps2",
        "psp" => "psp",
        "psvita" => "psvita",
        "ps3" => "ps3",
        "ps4" => "ps4",
        "ps5" => "ps5",
        _ => "unknown",
    }
}

fn system_directory_aliases(system_slug: &str) -> &'static [&'static str] {
    match system_slug {
        "nes" => &["nes", "famicom"],
        "snes" => &["snes", "sfc", "super nintendo", "supernintendo"],
        "gameboy" => &["gb", "gbc", "gameboy", "game boy", "gameboy color"],
        "gba" => &["gba", "gameboy advance", "game boy advance"],
        "n64" => &["n64", "nintendo 64", "nintendo64"],
        "nds" => &["nds", "nintendo ds", "nintendods"],
        "genesis" => &["genesis", "megadrive", "mega drive", "md"],
        "master-system" => &["mastersystem", "master system", "sms"],
        "game-gear" => &["gamegear", "game gear", "gg"],
        "sega-cd" => &["segacd", "sega cd", "mega cd", "megacd"],
        "sega-32x" => &["sega32x", "sega 32x", "32x"],
        "saturn" => &["saturn", "sega saturn"],
        "dreamcast" => &["dreamcast", "dc"],
        "neogeo" => &["neogeo", "neo geo", "mvs", "aes"],
        "wii" => &["wii", "nintendo wii", "dolphin", "rvl"],
        "psx" => &["psx", "ps1", "playstation", "playstation 1"],
        "ps2" => &["ps2", "playstation 2"],
        "psp" => &["psp"],
        "psvita" => &["psvita", "vita", "ps vita"],
        "ps3" => &["ps3", "playstation 3"],
        "ps4" => &["ps4", "playstation 4"],
        "ps5" => &["ps5", "playstation 5"],
        _ => &[],
    }
}

fn normalize_path_token(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn local_container_for_cloud_download(
    system_slug: &str,
    target_path: &Path,
    downloaded_bytes: &[u8],
) -> Result<SaveContainerFormat> {
    if system_slug != "psx" {
        return Ok(SaveContainerFormat::Native);
    }
    let normalized = normalize_save_bytes_for_sync(target_path, system_slug, downloaded_bytes)?
        .context("backend leverde geen geldige PS1 memory card projectie")?;
    Ok(normalized.local_container)
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
    source_systems: &[String],
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
        if let Some(reason) =
            dreamcast_skip_reason(save_path, rom_entry.map(|entry| entry.path.as_path()))
        {
            eprintln!(
                "Skipping Dreamcast save {}: {}",
                save_path.display(),
                reason
            );
        } else if let Some(reason) =
            saturn_skip_reason(save_path, rom_entry.map(|entry| entry.path.as_path()))
        {
            eprintln!("Skipping Saturn save {}: {}", save_path.display(), reason);
        } else if let Some(reason) = wii_skip_reason(save_path) {
            eprintln!("Skipping Wii save {}: {}", save_path.display(), reason);
        } else if verbose {
            eprintln!(
                "Skipping non-supported save (outside allowed console families): {}",
                save_path.display()
            );
        }
        return Ok(None);
    };
    let system_slug = classification.system_slug;
    let classification_evidence = classification.evidence;
    if !source_allows_system(source_systems, &system_slug) {
        report.skipped += 1;
        if verbose {
            eprintln!(
                "Skipping {}: system {} is disabled for source '{}'",
                save_path.display(),
                system_slug,
                source_name
            );
        }
        return Ok(None);
    }
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

    let effective_profile =
        effective_profile_for_save(source_kind, source_profile, &system_slug, save_path);
    let effective_slot_name =
        resolve_slot_name_for_sync(&system_slug, save_path, &options.slot_name);
    let device_type = helper_device_type_for_upload(source_kind, &effective_profile, &system_slug);
    let runtime_target =
        runtime_target_for_system(source_kind, &effective_profile, &system_slug, device_type);

    let mut rom_sha1 = if is_playstation_system(&system_slug) {
        Some(playstation_line_key(
            &system_slug,
            device_type,
            &effective_slot_name,
        ))
    } else if is_dreamcast_system(&system_slug) {
        Some(dreamcast_line_key(
            &system_slug,
            device_type,
            &effective_slot_name,
        ))
    } else if is_wii_system(&system_slug) {
        Some(wii_line_key(
            wii_title_code_from_path(save_path).as_deref(),
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

    let latest = match api.latest_save(
        &active_rom_sha1,
        &effective_slot_name,
        device_type,
        fingerprint,
        app_password,
        Some(&runtime_target),
    ) {
        Ok(value) => value,
        Err(err) if is_legacy_n64_latest_mismatch(&err, &system_slug) => {
            if verbose {
                eprintln!(
                    "Legacy N64 EEPROM cloud payload mismatch for {}; forcing repair upload",
                    save_path.display()
                );
            }
            LatestSaveResponse {
                exists: false,
                sha256: None,
                version: None,
                id: None,
            }
        }
        Err(err) if is_missing_cloud_payload_reference(&err) => {
            if verbose {
                eprintln!(
                    "Cloud latest points to missing payload for {}; forcing repair upload",
                    save_path.display()
                );
            }
            LatestSaveResponse {
                exists: false,
                sha256: None,
                version: None,
                id: None,
            }
        }
        Err(err) if is_playstation_projection_unavailable(&err, &system_slug) => {
            if verbose {
                eprintln!(
                    "Cloud latest points to missing PSX projection for {}; forcing repair upload",
                    save_path.display()
                );
            }
            LatestSaveResponse {
                exists: false,
                sha256: None,
                version: None,
                id: None,
            }
        }
        Err(err) => return Err(err),
    };

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

        let filename = upload_filename_for_sync(save_path, &system_slug);

        let upload_result = upload_with_n64_mister_cpk_fallback(
            api,
            &filename,
            &normalized_save.canonical_bytes,
            &active_rom_sha1,
            rom_md5.as_deref(),
            &effective_slot_name,
            device_type,
            fingerprint,
            app_password,
            &system_slug,
            wii_title_code_from_path(save_path).as_deref(),
            &runtime_target,
            verbose,
        );
        if let Err(err) = upload_result {
            if is_empty_n64_controller_pak_rejection(&err, &system_slug) {
                report.skipped += 1;
                if verbose {
                    eprintln!(
                        "Skipping empty N64 controller pak {}: {}",
                        save_path.display(),
                        err
                    );
                }
                return Ok(None);
            }
            return Err(err);
        }

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

        let filename = upload_filename_for_sync(save_path, &system_slug);

        let upload_result = upload_with_n64_mister_cpk_fallback(
            api,
            &filename,
            &normalized_save.canonical_bytes,
            &active_rom_sha1,
            rom_md5.as_deref(),
            &effective_slot_name,
            device_type,
            fingerprint,
            app_password,
            &system_slug,
            wii_title_code_from_path(save_path).as_deref(),
            &runtime_target,
            verbose,
        );
        if let Err(err) = upload_result {
            if is_empty_n64_controller_pak_rejection(&err, &system_slug) {
                report.skipped += 1;
                if verbose {
                    eprintln!(
                        "Skipping empty N64 controller pak {}: {}",
                        save_path.display(),
                        err
                    );
                }
                return Ok(None);
            }
            return Err(err);
        }
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

    let conflict = match api.conflict_check(
        &active_rom_sha1,
        &effective_slot_name,
        device_type,
        fingerprint,
        app_password,
        Some(&runtime_target),
    ) {
        Ok(conflict) => conflict,
        Err(err) if is_playstation_projection_unavailable(&err, &system_slug) => {
            if verbose {
                eprintln!(
                    "Conflict check points to missing PSX projection for {}; uploading local card as repair",
                    save_path.display()
                );
            }
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
            let filename = upload_filename_for_sync(save_path, &system_slug);
            api.upload_save(
                &filename,
                normalized_save.canonical_bytes.clone(),
                &active_rom_sha1,
                rom_md5.as_deref(),
                &effective_slot_name,
                device_type,
                fingerprint,
                app_password,
                Some(&system_slug),
                wii_title_code_from_path(save_path).as_deref(),
                Some(&runtime_target),
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
        Err(err) => return Err(err),
    };
    if conflict.exists {
        let conflict_result = handle_conflict(
            api,
            save_path,
            &normalized_save.canonical_bytes,
            &local_sha,
            &active_rom_sha1,
            &effective_slot_name,
            options.dry_run,
            device_type,
            fingerprint,
            &conflict,
            source_name,
            source_kind,
            app_password,
            &runtime_target,
        );
        if let Err(err) = conflict_result {
            if is_playstation_projection_unavailable(&err, &system_slug) {
                if verbose {
                    eprintln!(
                        "Conflict report points to missing PSX projection for {}; uploading local card as repair",
                        save_path.display()
                    );
                }
                let filename = upload_filename_for_sync(save_path, &system_slug);
                api.upload_save(
                    &filename,
                    normalized_save.canonical_bytes.clone(),
                    &active_rom_sha1,
                    rom_md5.as_deref(),
                    &effective_slot_name,
                    device_type,
                    fingerprint,
                    app_password,
                    Some(&system_slug),
                    wii_title_code_from_path(save_path).as_deref(),
                    Some(&runtime_target),
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
            return Err(err);
        }
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

        let canonical_bytes = match api.download_save(
            &save_id,
            device_type,
            fingerprint,
            app_password,
            Some(&runtime_target),
        ) {
            Ok(bytes) => bytes,
            Err(err) if is_playstation_projection_unavailable(&err, &system_slug) => {
                if verbose {
                    eprintln!(
                        "Backend PSX projection is unavailable for {}; uploading local card as repair",
                        save_path.display()
                    );
                }
                let filename = upload_filename_for_sync(save_path, &system_slug);
                api.upload_save(
                    &filename,
                    normalized_save.canonical_bytes.clone(),
                    &active_rom_sha1,
                    None,
                    &effective_slot_name,
                    device_type,
                    fingerprint,
                    app_password,
                    Some(&system_slug),
                    wii_title_code_from_path(save_path).as_deref(),
                    Some(&runtime_target),
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
            Err(err) => return Err(err),
        };
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
    fingerprint: &str,
    existing_entry: &SyncedEntry,
    source_name: &str,
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    save_roots: &[PathBuf],
    source_systems: &[String],
    create_missing_system_dirs: bool,
    app_password: Option<&str>,
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

    let Some(system_slug) = existing_entry.system_slug.as_deref() else {
        report.skipped += 1;
        if verbose {
            eprintln!(
                "Skipping missing save restore for {}: no system slug in state",
                save_path.display()
            );
        }
        return Ok(None);
    };
    if !source_allows_system(source_systems, system_slug) {
        report.skipped += 1;
        if verbose {
            eprintln!(
                "Skipping missing {} save restore for {}: system is disabled for source '{}'",
                system_slug,
                save_path.display(),
                source_name
            );
        }
        return Ok(None);
    }
    if !target_parent_allowed(save_path, save_roots, create_missing_system_dirs) {
        report.skipped += 1;
        if verbose {
            eprintln!(
                "Skipping missing {} save restore for {}: target system folder is not present",
                system_slug,
                save_path.display()
            );
        }
        return Ok(None);
    }
    let effective_slot_name = existing_entry
        .slot_name
        .clone()
        .unwrap_or_else(|| resolve_slot_name_for_sync(system_slug, save_path, &options.slot_name));
    let effective_profile =
        effective_profile_for_save(source_kind, source_profile, system_slug, save_path);
    let device_type = helper_device_type_for_upload(source_kind, &effective_profile, system_slug);
    let runtime_target =
        runtime_target_for_system(source_kind, &effective_profile, system_slug, device_type);

    let latest = match api.latest_save(
        rom_sha1,
        &effective_slot_name,
        device_type,
        fingerprint,
        app_password,
        Some(&runtime_target),
    ) {
        Ok(value) => value,
        Err(err) if is_missing_cloud_payload_reference(&err) => LatestSaveResponse {
            exists: false,
            sha256: None,
            version: None,
            id: None,
        },
        Err(err) => return Err(err),
    };
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
                Some(system_slug),
                Some(local_container),
                Some(adapter_profile),
                Some(source_kind),
                Some(source_name),
                Some(&effective_slot_name),
            ),
        )));
    }

    let canonical_bytes = api.download_save(
        save_id,
        device_type,
        fingerprint,
        app_password,
        Some(&runtime_target),
    )?;
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
            Some(system_slug),
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

fn is_dreamcast_system(system_slug: &str) -> bool {
    system_slug == "dreamcast"
}

fn is_wii_system(system_slug: &str) -> bool {
    system_slug == "wii"
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

fn runtime_target_for_system(
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    system_slug: &str,
    device_type: &str,
) -> RuntimeTarget {
    let clean_system = system_slug.trim().to_ascii_lowercase();
    if clean_system.is_empty() {
        return RuntimeTarget::default();
    }

    let Some(runtime_profile) = projection_runtime_profile_for_system(
        source_kind,
        source_profile,
        &clean_system,
        device_type,
    ) else {
        return RuntimeTarget::default();
    };

    let profile_name = runtime_profile
        .split_once('/')
        .map(|(_, value)| value.to_string())
        .unwrap_or_else(|| runtime_profile.clone());

    RuntimeTarget {
        runtime_profile: Some(runtime_profile.clone()),
        emulator_profile: Some(profile_name),
        system_profile_key: Some(system_profile_field_key(&clean_system)),
        system_profile_value: Some(runtime_profile),
    }
}

fn projection_runtime_profile_for_system(
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    system_slug: &str,
    device_type: &str,
) -> Option<String> {
    let fallback_from_kind = match source_kind {
        SourceKind::MisterFpga => "mister",
        SourceKind::RetroArch => "retroarch",
        SourceKind::Custom | SourceKind::OpenEmu | SourceKind::AnaloguePocket => "generic",
        SourceKind::Windows | SourceKind::SteamDeck => "generic",
    };

    match system_slug {
        "psx" => {
            let clean = device_type.trim().to_ascii_lowercase();
            if clean == "mister" {
                Some("psx/mister".to_string())
            } else {
                Some("psx/retroarch".to_string())
            }
        }
        "ps2" => Some("ps2/pcsx2".to_string()),
        "n64" => Some(
            match source_profile {
                EmulatorProfile::Mister => "n64/mister",
                EmulatorProfile::RetroArch => "n64/retroarch",
                EmulatorProfile::EverDrive => "n64/everdrive",
                EmulatorProfile::Project64 => "n64/project64",
                EmulatorProfile::MupenFamily => "n64/mupen-family",
                EmulatorProfile::Snes9x | EmulatorProfile::Zsnes | EmulatorProfile::Generic => {
                    match fallback_from_kind {
                        "mister" => "n64/mister",
                        "retroarch" => "n64/retroarch",
                        _ => "n64/mupen-family",
                    }
                }
            }
            .to_string(),
        ),
        "saturn" => Some(
            match source_profile {
                EmulatorProfile::Mister => "saturn/mister",
                EmulatorProfile::RetroArch => "saturn/mednafen",
                EmulatorProfile::Snes9x
                | EmulatorProfile::Zsnes
                | EmulatorProfile::EverDrive
                | EmulatorProfile::Project64
                | EmulatorProfile::MupenFamily
                | EmulatorProfile::Generic => match fallback_from_kind {
                    "retroarch" => "saturn/mednafen",
                    _ => "saturn/mister",
                },
            }
            .to_string(),
        ),
        "snes" => Some(
            match source_profile {
                EmulatorProfile::RetroArch => "snes/retroarch-snes9x",
                EmulatorProfile::Snes9x => "snes/snes9x",
                EmulatorProfile::Mister
                | EmulatorProfile::Zsnes
                | EmulatorProfile::EverDrive
                | EmulatorProfile::Project64
                | EmulatorProfile::MupenFamily
                | EmulatorProfile::Generic => match fallback_from_kind {
                    "retroarch" => "snes/retroarch-snes9x",
                    _ => "snes/snes9x",
                },
            }
            .to_string(),
        ),
        "nes" => Some(
            match source_profile {
                EmulatorProfile::RetroArch => "nes/retroarch-nestopia",
                _ => "nes/fceux",
            }
            .to_string(),
        ),
        "gba" => Some(
            match source_profile {
                EmulatorProfile::RetroArch => "gba/retroarch-mgba",
                _ => "gba/mgba",
            }
            .to_string(),
        ),
        "master-system" => Some(
            match source_profile {
                EmulatorProfile::RetroArch => "sms/retroarch-genesis-plus-gx",
                _ => "sms/genesis-plus-gx",
            }
            .to_string(),
        ),
        "genesis" => Some(
            match source_profile {
                EmulatorProfile::RetroArch => "genesis/retroarch-genesis-plus-gx",
                _ => "genesis/genesis-plus-gx",
            }
            .to_string(),
        ),
        "game-gear" => Some(
            match source_profile {
                EmulatorProfile::RetroArch => "gamegear/retroarch-genesis-plus-gx",
                _ => "gamegear/genesis-plus-gx",
            }
            .to_string(),
        ),
        "dreamcast" => Some(
            match source_profile {
                EmulatorProfile::Mister => "dreamcast/mister",
                EmulatorProfile::RetroArch => "dreamcast/retroarch-flycast",
                _ => match fallback_from_kind {
                    "mister" => "dreamcast/mister",
                    "retroarch" => "dreamcast/retroarch-flycast",
                    _ => "dreamcast/flycast",
                },
            }
            .to_string(),
        ),
        _ => None,
    }
}

fn effective_profile_for_save(
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    system_slug: &str,
    save_path: &Path,
) -> EmulatorProfile {
    match source_kind {
        SourceKind::MisterFpga => return EmulatorProfile::Mister,
        SourceKind::RetroArch => return EmulatorProfile::RetroArch,
        SourceKind::Custom
        | SourceKind::OpenEmu
        | SourceKind::AnaloguePocket
        | SourceKind::Windows
        | SourceKind::SteamDeck => {}
    }

    if !matches!(source_profile, EmulatorProfile::Generic) {
        return source_profile.clone();
    }

    infer_profile_from_path(system_slug, save_path).unwrap_or_else(|| source_profile.clone())
}

fn infer_profile_from_path(system_slug: &str, save_path: &Path) -> Option<EmulatorProfile> {
    let path_lower = save_path.to_string_lossy().to_ascii_lowercase();
    let has_token = |tokens: &[&str]| tokens.iter().any(|token| path_lower.contains(token));

    if system_slug == "n64" {
        if has_token(&["project64", "project-64", "pj64"]) {
            return Some(EmulatorProfile::Project64);
        }
        if has_token(&["mupen", "mupen64plus", "rmg", "rosalie"]) {
            return Some(EmulatorProfile::MupenFamily);
        }
        if has_token(&["everdrive", "ever-drive"]) {
            return Some(EmulatorProfile::EverDrive);
        }
    } else if system_slug == "snes" {
        if has_token(&["zsnes"]) {
            return Some(EmulatorProfile::Zsnes);
        }
        if has_token(&["snes9x"]) {
            return Some(EmulatorProfile::Snes9x);
        }
    }

    if has_token(&[
        "retroarch",
        "/retroarch/",
        "\\retroarch\\",
        "/emulation/saves/",
        "\\emulation\\saves\\",
    ]) {
        return Some(EmulatorProfile::RetroArch);
    }

    if has_token(&["/media/fat/", "\\media\\fat\\", "/mister/", "\\mister\\"]) {
        return Some(EmulatorProfile::Mister);
    }

    None
}

fn system_profile_field_key(system_slug: &str) -> String {
    if system_slug == "n64" {
        return "n64Profile".to_string();
    }
    if system_slug == "saturn" {
        return "saturnFormat".to_string();
    }

    let mut out = String::new();
    let mut uppercase_next = false;
    for ch in system_slug.chars() {
        if ch.is_ascii_alphanumeric() {
            if out.is_empty() {
                out.push(ch.to_ascii_lowercase());
                uppercase_next = false;
            } else if uppercase_next {
                out.push(ch.to_ascii_uppercase());
                uppercase_next = false;
            } else {
                out.push(ch.to_ascii_lowercase());
            }
        } else {
            uppercase_next = true;
        }
    }
    if out.is_empty() {
        "runtimeProfile".to_string()
    } else {
        out.push_str("Profile");
        out
    }
}

fn resolve_slot_name_for_sync(
    system_slug: &str,
    save_path: &Path,
    configured_slot: &str,
) -> String {
    if is_playstation_system(system_slug) {
        if let Some(slot) = parse_playstation_slot(configured_slot) {
            return slot;
        }
        return infer_playstation_slot_from_path(save_path);
    }

    if is_dreamcast_system(system_slug) {
        if let Some(slot) = parse_dreamcast_slot(configured_slot) {
            return slot;
        }
        return infer_dreamcast_slot_from_path(save_path);
    }

    if is_wii_system(system_slug) {
        return infer_wii_slot_from_path(save_path);
    }

    configured_slot.to_string()
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

fn parse_dreamcast_slot(value: &str) -> Option<String> {
    let upper = value.trim().to_ascii_uppercase();
    if upper.is_empty() || upper == "DEFAULT" {
        return None;
    }
    for bank in ['A', 'B', 'C', 'D'] {
        for slot in ['1', '2', '3', '4'] {
            let needle = format!("{bank}{slot}");
            if upper.contains(&needle) {
                return Some(needle);
            }
        }
    }
    None
}

fn infer_dreamcast_slot_from_path(path: &Path) -> String {
    let text = path.to_string_lossy().to_ascii_uppercase();
    parse_dreamcast_slot(&text).unwrap_or_else(|| "A1".to_string())
}

fn infer_wii_slot_from_path(path: &Path) -> String {
    wii_title_code_from_path(path)
        .map(|code| format!("{}/data.bin", code))
        .unwrap_or_else(|| {
            path.file_name()
                .and_then(|value| value.to_str())
                .map(ToString::to_string)
                .unwrap_or_else(|| "data.bin".to_string())
        })
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

fn dreamcast_line_key(system_slug: &str, device_type: &str, slot_name: &str) -> String {
    format!(
        "dc-line:{}:{}:{}",
        system_slug,
        device_type,
        slot_name.trim().to_ascii_lowercase()
    )
}

fn wii_line_key(title_code: Option<&str>, slot_name: &str) -> String {
    let normalized_title = title_code
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_uppercase())
        .unwrap_or_else(|| {
            slot_name
                .trim()
                .to_ascii_lowercase()
                .replace('\\', "/")
                .replace(' ', "-")
        });
    format!("wii-title:{}", normalized_title)
}

fn save_selection_key(save_path: &Path) -> String {
    if save_path
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case("data.bin"))
        .unwrap_or(false)
        && let Some(title_code) = wii_title_code_from_path(save_path)
    {
        return format!("wii:{}", title_code.to_ascii_lowercase());
    }
    filename_stem(save_path).to_ascii_lowercase()
}

fn select_preferred_save_per_stem(
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    save_files: &[PathBuf],
    rom_index: &HashMap<String, RomIndexEntry>,
) -> HashMap<String, PathBuf> {
    let mut selected: HashMap<String, (PathBuf, u8)> = HashMap::new();

    for save_path in save_files {
        let stem_key = save_selection_key(save_path);
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
            if (classification.system_slug == "n64"
                && is_native_n64_extension(extension.as_deref()))
                || extension.as_deref() == Some(preferred)
            {
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
    let current_extension = save_extension(save_path);
    if system_slug == "n64" && is_native_n64_extension(current_extension.as_deref()) {
        return save_path.to_path_buf();
    }
    let Some(preferred_extension) =
        preferred_extension_for_system(source_kind, source_profile, system_slug, canonical_size)
    else {
        return save_path.to_path_buf();
    };
    if current_extension.as_deref() == Some(preferred_extension) {
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
        | "sega-cd" | "sega-32x" | "neogeo" => preferred_extension_for_cartridge(source_profile),
        "saturn" => None,
        "n64" => preferred_extension_for_n64(source_kind, source_profile, save_size),
        "wii" => Some("bin"),
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
        | EmulatorProfile::Project64
        | EmulatorProfile::MupenFamily
        | EmulatorProfile::Generic => Some("srm"),
    }
}

fn preferred_extension_for_n64(
    _source_kind: &SourceKind,
    _source_profile: &EmulatorProfile,
    save_size: Option<u64>,
) -> Option<&'static str> {
    match save_size {
        Some(512) | Some(2_048) => Some("eep"),
        Some(32_768) => Some("sra"),
        Some(131_072) => Some("fla"),
        _ => None,
    }
}

fn save_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
}

fn is_native_n64_extension(extension: Option<&str>) -> bool {
    matches!(extension, Some("eep" | "fla" | "sra" | "mpk" | "cpk"))
}

fn upload_filename_for_sync(save_path: &Path, system_slug: &str) -> String {
    if system_slug == "wii" {
        return "data.bin".to_string();
    }
    let file_name = save_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("save.bin");
    let _ = system_slug;
    let _ = save_path;
    file_name.to_string()
}

fn is_legacy_n64_latest_mismatch(err: &anyhow::Error, system_slug: &str) -> bool {
    if system_slug != "n64" {
        return false;
    }
    let message = format!("{err:#}").to_ascii_lowercase();
    message.contains("n64 canonical payload size")
        && message.contains("does not match expected 2048")
        && message.contains("for eeprom")
}

fn is_missing_cloud_payload_reference(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();
    let status_match = message.contains("status=500 internal server error")
        || message.contains("status=404 not found");
    if !status_match {
        return false;
    }
    let payload_hint = message.contains("payload")
        || message.contains("save payload file is missing on server")
        || message.contains("missing payload");
    let missing_hint =
        message.contains("no such file or directory") || message.contains("missing on server");
    payload_hint && missing_hint
}

fn is_empty_n64_controller_pak_rejection(err: &anyhow::Error, system_slug: &str) -> bool {
    if system_slug != "n64" {
        return false;
    }
    let message = format!("{err:#}").to_ascii_lowercase();
    let status_match = message.contains("status=422 unprocessable entity")
        || message.contains("status=400 bad request");
    let controller_pak_hint = message.contains("controller-pak") || message.contains("cpk");
    let empty_hint = message.contains("does not contain any save entries")
        || message.contains("no save entries")
        || message.contains("empty controller pak");
    status_match && controller_pak_hint && empty_hint
}

fn is_playstation_projection_unavailable(err: &anyhow::Error, system_slug: &str) -> bool {
    if !is_playstation_system(system_slug) {
        return false;
    }
    let message = format!("{err:#}").to_ascii_lowercase();
    let status_match = message.contains("status=400 bad request")
        || message.contains("status=404 not found")
        || message.contains("status=422 unprocessable entity");
    let projection_hint = message.contains("playstation projection");
    let unavailable_hint =
        message.contains("not found") || message.contains("not a playstation projection");
    status_match && projection_hint && unavailable_hint
}

#[allow(clippy::too_many_arguments)]
fn upload_with_n64_mister_cpk_fallback(
    api: &ApiClient,
    filename: &str,
    bytes: &[u8],
    rom_sha1: &str,
    rom_md5: Option<&str>,
    slot_name: &str,
    device_type: &str,
    fingerprint: &str,
    app_password: Option<&str>,
    system_slug: &str,
    wii_title_id: Option<&str>,
    runtime_target: &RuntimeTarget,
    verbose: bool,
) -> Result<()> {
    let upload_result = api.upload_save(
        filename,
        bytes.to_vec(),
        rom_sha1,
        rom_md5,
        slot_name,
        device_type,
        fingerprint,
        app_password,
        Some(system_slug),
        wii_title_id,
        Some(runtime_target),
    );
    if let Err(err) = upload_result {
        if should_retry_n64_mister_cpk_as_mpk(&err, system_slug, runtime_target, filename)
            && let Some(fallback_filename) = filename_with_extension(filename, "mpk")
        {
            if verbose {
                eprintln!(
                    "Retrying N64 controller pak upload as {} (legacy backend compatibility)",
                    fallback_filename
                );
            }
            let _retry = api.upload_save(
                &fallback_filename,
                bytes.to_vec(),
                rom_sha1,
                rom_md5,
                slot_name,
                device_type,
                fingerprint,
                app_password,
                Some(system_slug),
                wii_title_id,
                Some(runtime_target),
            )?;
            return Ok(());
        }
        return Err(err);
    }
    Ok(())
}

fn should_retry_n64_mister_cpk_as_mpk(
    err: &anyhow::Error,
    system_slug: &str,
    runtime_target: &RuntimeTarget,
    filename: &str,
) -> bool {
    if system_slug != "n64" {
        return false;
    }
    if !filename.to_ascii_lowercase().ends_with(".cpk") {
        return false;
    }
    let profile = runtime_target
        .system_profile_value
        .as_deref()
        .or(runtime_target.runtime_profile.as_deref())
        .unwrap_or_default();
    if profile != "n64/mister" {
        return false;
    }
    let message = format!("{err:#}").to_ascii_lowercase();
    message.contains("status=422 unprocessable entity")
        && message.contains("unsupported n64 upload form for n64/mister")
        && message.contains(".cpk")
}

fn filename_with_extension(filename: &str, target_ext: &str) -> Option<String> {
    let trimmed_target = target_ext
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if trimmed_target.is_empty() {
        return None;
    }
    let path = Path::new(filename);
    let stem = path.file_stem()?.to_str()?;
    Some(format!("{}.{}", stem, trimmed_target))
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
    device_type: &str,
    fingerprint: &str,
    conflict: &ConflictCheckResponse,
    source_name: &str,
    source_kind: &SourceKind,
    app_password: Option<&str>,
    runtime_target: &RuntimeTarget,
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
        device_type,
        fingerprint,
        app_password,
        Some(runtime_target),
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
    fn source_system_policy_allows_only_configured_slugs() {
        let systems = vec!["snes".to_string(), "n64".to_string()];
        assert!(source_allows_system(&systems, "snes"));
        assert!(source_allows_system(&systems, "N64"));
        assert!(!source_allows_system(&systems, "wii"));
        assert!(!source_allows_system(&[], "snes"));
    }

    #[test]
    fn target_parent_policy_blocks_missing_system_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("saves");
        fs::create_dir_all(root.join("SNES")).unwrap();

        let existing_system_target = root.join("SNES/Super Mario Kart.sav");
        assert!(target_parent_allowed(
            &existing_system_target,
            std::slice::from_ref(&root),
            false
        ));

        let missing_system_target = root.join("Wii/SB4P/data.bin");
        assert!(!target_parent_allowed(
            &missing_system_target,
            std::slice::from_ref(&root),
            false
        ));
        assert!(target_parent_allowed(
            &missing_system_target,
            std::slice::from_ref(&root),
            true
        ));
    }

    fn cloud_save(
        filename: &str,
        title: &str,
        system_slug: &str,
        profile_id: &str,
        target_extension: &str,
    ) -> CloudSaveSummary {
        CloudSaveSummary {
            id: "save-cloud".to_string(),
            filename: filename.to_string(),
            display_title: title.to_string(),
            system_slug: system_slug.to_string(),
            game: None,
            sha256: Some("sha".to_string()),
            version: Some(1),
            file_size: Some(8192),
            latest_size_bytes: Some(8192),
            media_type: None,
            runtime_profile: None,
            source_artifact_profile: None,
            logical_key: None,
            card_slot: None,
            download_profiles: vec![crate::api::DownloadProfile {
                id: profile_id.to_string(),
                label: profile_id.to_string(),
                target_extension: Some(target_extension.to_string()),
                note: None,
            }],
            inspection: None,
            metadata: None,
            rom_sha1: None,
            rom_md5: None,
        }
    }

    #[test]
    fn runtime_target_uses_explicit_n64_profile_key() {
        let target = runtime_target_for_system(
            &SourceKind::MisterFpga,
            &EmulatorProfile::Mister,
            "n64",
            "mister",
        );
        assert_eq!(target.runtime_profile.as_deref(), Some("n64/mister"));
        assert_eq!(target.emulator_profile.as_deref(), Some("mister"));
        assert_eq!(target.system_profile_key.as_deref(), Some("n64Profile"));
        assert_eq!(target.system_profile_value.as_deref(), Some("n64/mister"));
    }

    #[test]
    fn runtime_target_supports_n64_project64_profile() {
        let target = runtime_target_for_system(
            &SourceKind::Custom,
            &EmulatorProfile::Project64,
            "n64",
            "custom",
        );
        assert_eq!(target.runtime_profile.as_deref(), Some("n64/project64"));
        assert_eq!(target.system_profile_key.as_deref(), Some("n64Profile"));
        assert_eq!(
            target.system_profile_value.as_deref(),
            Some("n64/project64")
        );
    }

    #[test]
    fn runtime_target_supports_n64_mupen_family_profile() {
        let target = runtime_target_for_system(
            &SourceKind::Custom,
            &EmulatorProfile::MupenFamily,
            "n64",
            "custom",
        );
        assert_eq!(target.runtime_profile.as_deref(), Some("n64/mupen-family"));
        assert_eq!(target.system_profile_key.as_deref(), Some("n64Profile"));
        assert_eq!(
            target.system_profile_value.as_deref(),
            Some("n64/mupen-family")
        );
    }

    #[test]
    fn runtime_target_uses_profile_based_runtime_for_snes() {
        let target = runtime_target_for_system(
            &SourceKind::Custom,
            &EmulatorProfile::Snes9x,
            "snes",
            "custom",
        );
        assert_eq!(target.runtime_profile.as_deref(), Some("snes/snes9x"));
        assert_eq!(target.emulator_profile.as_deref(), Some("snes9x"));
        assert_eq!(target.system_profile_key.as_deref(), Some("snesProfile"));
        assert_eq!(target.system_profile_value.as_deref(), Some("snes/snes9x"));
    }

    #[test]
    fn runtime_target_returns_empty_for_non_projection_systems() {
        let target = runtime_target_for_system(
            &SourceKind::RetroArch,
            &EmulatorProfile::RetroArch,
            "nds",
            "retroarch",
        );
        assert!(target.runtime_profile.is_none());
        assert!(target.system_profile_key.is_none());
        assert!(target.system_profile_value.is_none());
    }

    #[test]
    fn runtime_target_maps_retroarch_snes_to_backend_profile_id() {
        let target = runtime_target_for_system(
            &SourceKind::RetroArch,
            &EmulatorProfile::RetroArch,
            "snes",
            "retroarch",
        );
        assert_eq!(
            target.runtime_profile.as_deref(),
            Some("snes/retroarch-snes9x")
        );
    }

    #[test]
    fn runtime_target_maps_saturn_to_saturn_format_alias() {
        let target = runtime_target_for_system(
            &SourceKind::RetroArch,
            &EmulatorProfile::RetroArch,
            "saturn",
            "retroarch",
        );
        assert_eq!(target.runtime_profile.as_deref(), Some("saturn/mednafen"));
        assert_eq!(target.system_profile_key.as_deref(), Some("saturnFormat"));
    }

    #[test]
    fn system_profile_key_handles_hyphenated_slugs() {
        assert_eq!(system_profile_field_key("sega-cd"), "segaCdProfile");
        assert_eq!(system_profile_field_key("saturn"), "saturnFormat");
        assert_eq!(system_profile_field_key("n64"), "n64Profile");
        assert_eq!(
            system_profile_field_key("master-system"),
            "masterSystemProfile"
        );
    }

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
    fn prefers_n64_native_extension_for_non_mister_sources() {
        assert_eq!(
            preferred_extension_for_system(
                &SourceKind::RetroArch,
                &EmulatorProfile::RetroArch,
                "n64",
                Some(32_768)
            ),
            Some("sra")
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
    fn prefers_cartridge_mapping_for_sega_cd_and_32x() {
        assert_eq!(
            preferred_extension_for_system(
                &SourceKind::RetroArch,
                &EmulatorProfile::RetroArch,
                "sega-cd",
                Some(8192)
            ),
            Some("srm")
        );
        assert_eq!(
            preferred_extension_for_system(
                &SourceKind::MisterFpga,
                &EmulatorProfile::Mister,
                "sega-32x",
                Some(8192)
            ),
            Some("sav")
        );
    }

    #[test]
    fn saturn_keeps_native_extension_without_forced_rewrite() {
        assert_eq!(
            preferred_extension_for_system(
                &SourceKind::RetroArch,
                &EmulatorProfile::RetroArch,
                "saturn",
                Some(32768)
            ),
            None
        );
    }

    #[test]
    fn n64_unknown_size_has_no_forced_extension() {
        assert_eq!(
            preferred_extension_for_system(
                &SourceKind::RetroArch,
                &EmulatorProfile::RetroArch,
                "n64",
                Some(786_432)
            ),
            None
        );
    }

    #[test]
    fn keeps_native_n64_mpk_extension_on_download_path() {
        let path = PathBuf::from("/userdata/saves/n64/Mario Kart 64.mpk");
        let target = preferred_save_path(
            &path,
            &SourceKind::RetroArch,
            &EmulatorProfile::RetroArch,
            Some("n64"),
            SaveContainerFormat::Native,
            Some(32_768),
        );
        assert_eq!(target, path);
    }

    #[test]
    fn keeps_native_n64_cpk_extension_on_download_path() {
        let path = PathBuf::from("/media/fat/saves/N64/Mario Kart 64 (USA)_1.cpk");
        let target = preferred_save_path(
            &path,
            &SourceKind::MisterFpga,
            &EmulatorProfile::Mister,
            Some("n64"),
            SaveContainerFormat::Native,
            Some(32_768),
        );
        assert_eq!(target, path);
    }

    #[test]
    fn n64_cpk_upload_filename_keeps_cpk() {
        let file = PathBuf::from("/media/fat/saves/N64/Mario Kart 64 (USA)_1.cpk");
        assert_eq!(
            upload_filename_for_sync(&file, "n64"),
            "Mario Kart 64 (USA)_1.cpk"
        );
    }

    #[test]
    fn wii_slot_and_line_key_use_title_code() {
        let file = PathBuf::from("/home/deck/Emulation/saves/wii/SB4P/data.bin");
        assert_eq!(infer_wii_slot_from_path(&file), "SB4P/data.bin");
        assert_eq!(
            wii_line_key(wii_title_code_from_path(&file).as_deref(), "ignored"),
            "wii-title:SB4P"
        );
        assert_eq!(upload_filename_for_sync(&file, "wii"), "data.bin");
        assert_eq!(save_selection_key(&file), "wii:sb4p");
    }

    #[test]
    fn cloud_restore_targets_wii_title_code_directory() {
        let mut save = cloud_save(
            "data.bin",
            "Super Mario Galaxy 2",
            "wii",
            "original",
            ".bin",
        );
        save.card_slot = Some("SB4P/data.bin".to_string());

        let target = cloud_target_path(
            &save,
            &[PathBuf::from("/home/deck/Emulation/saves")],
            &SourceKind::SteamDeck,
            &EmulatorProfile::RetroArch,
            "wii",
            Some("bin"),
        );

        assert_eq!(
            target.to_string_lossy(),
            "/home/deck/Emulation/saves/wii/SB4P/data.bin"
        );
    }

    #[test]
    fn cloud_restore_reads_wii_title_code_from_backend_metadata() {
        let mut save = cloud_save(
            "data.bin",
            "Super Mario Galaxy 2",
            "wii",
            "original",
            ".bin",
        );
        save.metadata = Some(serde_json::json!({
            "rsm": {
                "wii": {
                    "titleCode": "SB4P",
                    "sourcePath": "Super Mario Galaxy 2/SB4P/data.bin"
                }
            }
        }));

        let target = cloud_target_path(
            &save,
            &[PathBuf::from("/media/fat/saves")],
            &SourceKind::MisterFpga,
            &EmulatorProfile::Mister,
            "wii",
            Some("bin"),
        );

        assert_eq!(
            target.to_string_lossy(),
            "/media/fat/saves/Wii/SB4P/data.bin"
        );
    }

    #[test]
    fn infers_n64_project64_profile_from_path() {
        let file = PathBuf::from(
            "/home/deck/Emulation/saves/project64/N64/The Legend of Zelda - Ocarina of Time.sra",
        );
        let profile = effective_profile_for_save(
            &SourceKind::Custom,
            &EmulatorProfile::Generic,
            "n64",
            &file,
        );
        assert_eq!(profile, EmulatorProfile::Project64);
    }

    #[test]
    fn detects_legacy_n64_latest_payload_mismatch() {
        let err = anyhow::Error::msg(
            "HTTP request faalde: status=400 Bad Request body={\"error\":\"Bad Request\",\"message\":\"N64 canonical payload size 512 does not match expected 2048 for eeprom\",\"statusCode\":400}",
        );
        assert!(is_legacy_n64_latest_mismatch(&err, "n64"));
        assert!(!is_legacy_n64_latest_mismatch(&err, "snes"));
    }

    #[test]
    fn detects_missing_cloud_payload_reference() {
        let err = anyhow::Error::msg(
            "HTTP request faalde: status=500 Internal Server Error body={\"error\":\"Internal Server Error\",\"message\":\"open /saves/Nintendo 64/Test/payload.mpk: no such file or directory\",\"statusCode\":500}",
        );
        assert!(is_missing_cloud_payload_reference(&err));

        let not_missing = anyhow::Error::msg(
            "HTTP request faalde: status=500 Internal Server Error body={\"error\":\"Internal Server Error\",\"message\":\"database unavailable\",\"statusCode\":500}",
        );
        assert!(!is_missing_cloud_payload_reference(&not_missing));
    }

    #[test]
    fn detects_empty_n64_controller_pak_rejection() {
        let err = anyhow::Error::msg(
            "HTTP request faalde: status=422 Unprocessable Entity body={\"error\":\"Unprocessable Entity\",\"message\":\"unsupported\",\"reason\":\"n64 controller-pak does not contain any save entries\",\"statusCode\":422}",
        );
        assert!(is_empty_n64_controller_pak_rejection(&err, "n64"));
        assert!(!is_empty_n64_controller_pak_rejection(&err, "snes"));
    }

    #[test]
    fn detects_unavailable_playstation_projection() {
        let err = anyhow::Error::msg(
            "download faalde: status=400 Bad Request body={\"message\":\"save is not a playstation projection\"}",
        );
        assert!(is_playstation_projection_unavailable(&err, "psx"));
        assert!(!is_playstation_projection_unavailable(&err, "saturn"));
    }

    #[test]
    fn retries_n64_mister_cpk_for_legacy_backend_upload_form() {
        let err = anyhow::Error::msg(
            "HTTP request faalde: status=422 Unprocessable Entity body={\"error\":\"Unprocessable Entity\",\"message\":\"unsupported N64 upload form for n64/mister: .cpk (32768 bytes)\",\"statusCode\":422}",
        );
        let target = RuntimeTarget {
            runtime_profile: Some("n64/mister".to_string()),
            emulator_profile: Some("mister".to_string()),
            system_profile_key: Some("n64Profile".to_string()),
            system_profile_value: Some("n64/mister".to_string()),
        };
        assert!(should_retry_n64_mister_cpk_as_mpk(
            &err,
            "n64",
            &target,
            "mk64_1.cpk"
        ));
        assert_eq!(
            filename_with_extension("mk64_1.cpk", "mpk").as_deref(),
            Some("mk64_1.mpk")
        );
    }

    #[test]
    fn cloud_restore_targets_mister_snes_sav_even_when_backend_profile_is_srm() {
        let save = cloud_save(
            "Super Mario Kart (USA).srm",
            "Super Mario Kart",
            "snes",
            "snes/snes9x",
            ".srm",
        );
        let extension = cloud_target_extension(&save, Some("snes/snes9x"));
        let provisional = cloud_target_path(
            &save,
            &[PathBuf::from("/media/fat/saves")],
            &SourceKind::MisterFpga,
            &EmulatorProfile::Mister,
            "snes",
            extension.as_deref(),
        );
        assert_eq!(
            provisional.to_string_lossy(),
            "/media/fat/saves/SNES/Super Mario Kart (USA).srm"
        );

        let final_path = preferred_save_path(
            &provisional,
            &SourceKind::MisterFpga,
            &EmulatorProfile::Mister,
            Some("snes"),
            SaveContainerFormat::Native,
            Some(2048),
        );
        assert_eq!(
            final_path.to_string_lossy(),
            "/media/fat/saves/SNES/Super Mario Kart (USA).sav"
        );
    }

    #[test]
    fn cloud_restore_prechecks_retroarch_preferred_extension_before_download() {
        let save = cloud_save(
            "Super Mario Bros. Deluxe.sav",
            "Super Mario Bros. Deluxe",
            "gameboy",
            "original",
            ".sav",
        );
        let extension = cloud_target_extension(&save, None);
        let provisional = cloud_target_path(
            &save,
            &[PathBuf::from("/userdata/saves")],
            &SourceKind::RetroArch,
            &EmulatorProfile::RetroArch,
            "gameboy",
            extension.as_deref(),
        );
        assert_eq!(
            provisional.to_string_lossy(),
            "/userdata/saves/gb/Super Mario Bros. Deluxe.sav"
        );

        let existing_target = cloud_restore_native_target_path(
            &provisional,
            &SourceKind::RetroArch,
            &EmulatorProfile::RetroArch,
            "gameboy",
            &save,
        );
        assert_eq!(
            existing_target.to_string_lossy(),
            "/userdata/saves/gb/Super Mario Bros. Deluxe.srm"
        );
    }

    #[test]
    fn cloud_restore_targets_mister_n64_controller_pak_cpk() {
        let mut save = cloud_save(
            "Mario Kart 64 (USA)_1.mpk",
            "Mario Kart 64",
            "n64",
            "n64/mister",
            ".cpk",
        );
        save.media_type = Some("controller-pak".to_string());
        let extension = cloud_target_extension(&save, Some("n64/mister"));
        let target = cloud_target_path(
            &save,
            &[PathBuf::from("/media/fat/saves")],
            &SourceKind::MisterFpga,
            &EmulatorProfile::Mister,
            "n64",
            extension.as_deref(),
        );
        assert_eq!(
            target.to_string_lossy(),
            "/media/fat/saves/N64/Mario Kart 64 (USA)_1.cpk"
        );
    }

    #[test]
    fn cloud_restore_prefers_mister_directory_casing_over_generic_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("saves");
        let canonical = root.join("N64");
        let generic = root.join("n64");
        fs::create_dir_all(&canonical).unwrap();
        fs::create_dir_all(&generic).unwrap();
        fs::write(canonical.join("Mario Kart 64 (USA).eep"), vec![0x01; 2048]).unwrap();
        fs::write(generic.join("Mario Kart 64 (USA).eep"), vec![0x02; 2048]).unwrap();

        let save = cloud_save(
            "Mario Kart 64 (USA).eep",
            "Mario Kart 64",
            "n64",
            "n64/mister",
            ".eep",
        );
        let target = cloud_target_path(
            &save,
            &[root],
            &SourceKind::MisterFpga,
            &EmulatorProfile::Mister,
            "n64",
            Some("eep"),
        );

        assert!(target.to_string_lossy().contains("/N64/"));
    }

    #[test]
    fn cloud_restore_keeps_single_system_emulator_root_direct() {
        let save = cloud_save(
            "Chrono Trigger (USA).srm",
            "Chrono Trigger",
            "snes",
            "snes/snes9x",
            ".srm",
        );
        let target = cloud_target_path(
            &save,
            &[PathBuf::from("/home/snes9x/save")],
            &SourceKind::Custom,
            &EmulatorProfile::Snes9x,
            "snes",
            Some("srm"),
        );
        assert_eq!(
            target.to_string_lossy(),
            "/home/snes9x/save/Chrono Trigger (USA).srm"
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

    #[test]
    fn resolves_dreamcast_slot_from_path() {
        assert_eq!(
            resolve_slot_name_for_sync(
                "dreamcast",
                Path::new("/userdata/saves/dreamcast/Sonic Adventure 2.A3.bin"),
                "default",
            ),
            "A3"
        );
        assert_eq!(
            resolve_slot_name_for_sync(
                "dreamcast",
                Path::new("/userdata/saves/dreamcast/unknown.bin"),
                "default",
            ),
            "A1"
        );
    }

    #[test]
    fn dreamcast_line_key_uses_slot_and_device() {
        assert_eq!(
            dreamcast_line_key("dreamcast", "retroarch", "A2"),
            "dc-line:dreamcast:retroarch:a2"
        );
    }
}
