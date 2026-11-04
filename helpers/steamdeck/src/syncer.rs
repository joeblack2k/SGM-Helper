use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::api::{
    ApiClient, CloudSaveSummary, ConflictCheckResponse, LatestSaveContext, LatestSaveRequest,
    LatestSaveResponse, PortUploadMetadata, RuntimeTarget,
};
use crate::backend_config::sync_config_with_backend;
use crate::config::AppConfig;
use crate::ports::{PortSaveMatch, discover_port_save_matches};
use crate::scanner::{
    PSP_SAVEDATA_SUBPATHS, RomIndexEntry, SaveAdapterProfile, SaveContainerFormat,
    classify_supported_save, discover_rom_index, discover_save_files,
    discover_save_files_and_psp_bundles, dreamcast_skip_reason,
    encode_download_for_local_container, filename_stem, is_psp_savedata_parent, md5_file,
    normalize_save_bytes_for_sync, normalize_save_for_sync, pack_psp_savedata,
    parse_param_sfo_savedir, saturn_skip_reason, sha1_file, sha256_bytes, sha256_file,
    unpack_psp_savedata, wii_skip_reason, wii_title_code_from_path,
};
use crate::sources::{EmulatorProfile, ResolvedSource, SourceKind, prepare_sources_for_sync};
use crate::state::{
    AuthState, SyncedEntry, TerminalRejection, load_sync_state, now_rfc3339, save_sync_state,
};

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
    pub would_upload: usize,
    pub downloaded: usize,
    pub would_download: usize,
    pub in_sync: usize,
    pub conflicts: usize,
    pub skipped: usize,
    /// The server refused an N64 controller-pak upload. Counted apart from
    /// `skipped`, which also holds saves with nothing to do, and apart from
    /// `errors`, which holds refusals and failures on every other system.
    pub rejected_n64_controller_pak: usize,
    /// The server answered 2xx but returned no save, so nothing was stored.
    pub accepted_not_stored: usize,
    /// Saves the server refused permanently. Not re-attempted until the
    /// payload changes.
    pub rejected_permanent: usize,
    pub errors: usize,
    pub dry_run: bool,
}

/// Marks a summary whose counters describe what would have happened, not what did.
pub fn dry_run_marker(report: &SyncReport) -> &'static str {
    if report.dry_run { " (DRY RUN)" } else { "" }
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
                // A sync that died leaves its lock behind, and every later sync would stop here for
                // good. The lock names its owner; one whose process is gone is taken over, once.
                let existing = fs::read_to_string(&path).unwrap_or_default();
                match lock_owner(&existing) {
                    Some(pid) if !pid_alive(pid) => Self::take_over(&path, pid, &lock_content),
                    _ => anyhow::bail!("sync is al actief (lockfile bestaat): {}", path.display()),
                }
            }
            Err(err) => Err(err)
                .with_context(|| format!("kan sync lockfile niet maken: {}", path.display())),
        }
    }
}

impl SyncLock {
    /// Take over a lock whose owner `dead` is gone. Two syncs can find the same dead lock: a rename
    /// to a name of this process's own moves the file once, so only one of them gets it, and a
    /// remove by path could delete the lock the other has just created. The renamed file must still
    /// name `dead`; one naming anyone else is a live lock, put back.
    fn take_over(path: &Path, dead: u32, lock_content: &str) -> Result<Self> {
        let busy = || anyhow::anyhow!("sync is al actief (lockfile bestaat): {}", path.display());
        let stale = path.with_extension(format!("lock.stale.{}", std::process::id()));
        if fs::rename(path, &stale).is_err() {
            return Err(busy());
        }
        if lock_owner(&fs::read_to_string(&stale).unwrap_or_default()) != Some(dead) {
            let _ = fs::rename(&stale, path);
            return Err(busy());
        }
        let _ = fs::remove_file(&stale);
        eprintln!(
            "oude sync.lock van pid={} verwijderd (proces draait niet meer): {}",
            dead,
            path.display()
        );
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|_| busy())?;
        use std::io::Write;
        file.write_all(lock_content.as_bytes())
            .with_context(|| format!("kan lockfile niet schrijven: {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }
}

/// The pid a `sync.lock` names on its `pid=` line.
fn lock_owner(contents: &str) -> Option<u32> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix("pid="))
        .and_then(|pid| pid.trim().parse().ok())
}

/// Whether a process with this pid is running. A process that exists but this user may not
/// signal or open (another user's) counts as running, so its lock is never taken.
fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks that the process exists; nothing is sent.
        if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{
            CloseHandle, ERROR_ACCESS_DENIED, GetLastError, STILL_ACTIVE,
        };
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: OpenProcess only reads the pid; the handle is closed below.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            // SAFETY: reads this thread's last error, set by OpenProcess.
            return unsafe { GetLastError() } == ERROR_ACCESS_DENIED;
        }
        let mut code = 0u32;
        // SAFETY: the handle is open and valid; code is a valid out pointer.
        let ok = unsafe { GetExitCodeProcess(handle, &mut code) } != 0;
        // SAFETY: closes the handle opened above, once.
        unsafe { CloseHandle(handle) };
        !ok || code == STILL_ACTIVE as u32
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
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

    let mut report = SyncReport {
        dry_run: options.dry_run,
        ..Default::default()
    };
    let mut rom_hash_cache: HashMap<String, (String, String)> = HashMap::new();

    for source in sources {
        let fingerprint = hostname::get()
            .ok()
            .and_then(|value| value.into_string().ok())
            .unwrap_or_else(|| source.kind.as_str().to_string());

        if matches!(source.kind, SourceKind::Ports) {
            let port_saves = discover_port_save_matches(&source.save_roots)?;
            if verbose {
                eprintln!(
                    "Source '{}' ({}) discovered {} port progress save file(s)",
                    source.name,
                    source.kind.as_str(),
                    port_saves.len()
                );
            }
            report.scanned += port_saves.len();

            for port_save in port_saves {
                let save_key = port_save.path.to_string_lossy().to_string();
                let process_result = process_port_save(
                    &api,
                    &port_save,
                    &save_key,
                    &fingerprint,
                    &source.name,
                    &source.kind,
                    &source.systems,
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
                        eprintln!("Port sync error for {}: {}", port_save.path.display(), err);
                    }
                }
            }
        } else {
            // With PSP_SAVEDATA_BUNDLES on, a PPSSPP SAVEDATA folder is one save and its files
            // are not discovered on their own.
            let (save_files, psp_bundles) = if config.psp_savedata_bundles {
                discover_save_files_and_psp_bundles(&source.save_roots, source.recursive)?
            } else {
                (
                    discover_save_files(&source.save_roots, source.recursive)?,
                    Vec::new(),
                )
            };
            if verbose {
                eprintln!(
                    "Source '{}' ({}) discovered {} save file(s)",
                    source.name,
                    source.kind.as_str(),
                    save_files.len()
                );
                if config.psp_savedata_bundles {
                    eprintln!(
                        "Source '{}' ({}) discovered {} PPSSPP SAVEDATA folder(s)",
                        source.name,
                        source.kind.as_str(),
                        psp_bundles.len()
                    );
                }
            }
            report.scanned += psp_bundles.len();
            for bundle_dir in psp_bundles {
                let save_key = bundle_dir.to_string_lossy().to_string();
                let result = process_psp_savedata_dir(
                    &api,
                    &bundle_dir,
                    &save_key,
                    sync_state
                        .entries
                        .get(&save_key)
                        .map(|entry| entry.sha256.as_str()),
                    &fingerprint,
                    &source.name,
                    &source.kind,
                    &source.profile,
                    &source.systems,
                    app_password,
                    options,
                    &mut report,
                    verbose,
                );
                match result {
                    Ok(processed) => {
                        if let Some(processed) = processed {
                            sync_state
                                .entries
                                .insert(processed.state_key, processed.entry);
                        }
                    }
                    Err(err) => {
                        report.errors += 1;
                        if verbose {
                            eprintln!("Sync error for {}: {}", bundle_dir.display(), err);
                        }
                    }
                }
            }

            let rom_index = discover_rom_index(&source.name, &source.rom_roots, source.recursive)?;
            let preferred_save_by_stem = select_preferred_save_per_stem(
                &source.kind,
                &source.profile,
                &save_files,
                &rom_index,
            );
            // How many files share each selection key. The live-directory scan only
            // runs where it can change the answer: one copy and no declared
            // directory has nothing to decide.
            let mut stem_counts: HashMap<String, usize> = HashMap::new();
            for path in &save_files {
                *stem_counts.entry(save_selection_key(path)).or_default() += 1;
            }
            report.scanned += save_files.len();

            for save_path in save_files {
                let selection_key = save_selection_key(&save_path);
                let needs_live_check = stem_counts.get(&selection_key).copied().unwrap_or(0) > 1
                    || !source.save_subdirs.is_empty();
                if needs_live_check
                    && let Some(reason) = live_upload_blocked(&source, &save_path, &selection_key)
                {
                    report.skipped += 1;
                    eprintln!("{}", reason);
                    continue;
                }
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
                if let Some(prior) = sync_state.terminal_rejections.get(&save_key)
                    && sha256_file(&save_path)
                        .map(|current| current == prior.sha256)
                        .unwrap_or(false)
                {
                    report.rejected_permanent += 1;
                    eprintln!(
                        "Not retrying {}: the server refused it with {}{}",
                        save_path.display(),
                        prior.status,
                        if prior.reason.is_empty() {
                            String::new()
                        } else {
                            format!(": {}", prior.reason)
                        }
                    );
                    continue;
                }

                let process_result = process_single_save(
                    &api,
                    &save_path,
                    &save_key,
                    sync_state.entries.get(&save_key),
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
                        eprintln!("Sync error for {}: {}", save_path.display(), err);
                        if let Some(status) = permanent_rejection_status(&err) {
                            let sha = sha256_file(&save_path).unwrap_or_default();
                            sync_state.terminal_rejections.insert(
                                save_key.clone(),
                                TerminalRejection {
                                    sha256: sha,
                                    status,
                                    reason: format!("{err:#}"),
                                    recorded_at: now_rfc3339(),
                                    helper_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                                },
                            );
                        }
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
                // A SAVEDATA folder is a bundle, not a file: the cloud-only pass restores it.
                if entry
                    .rom_sha1
                    .as_deref()
                    .map(|key| key.starts_with("psp-savedir:"))
                    .unwrap_or(false)
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
                    eprintln!("Restore error for {}: {}", save_path.display(), err);
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
            &source.save_subdirs,
            source.create_missing_system_dirs,
            config.psp_savedata_bundles,
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

/// A 4xx says the request itself was refused, so retrying identical bytes
/// cannot succeed. 408 and 429 are the exceptions: a timeout or a rate limit
/// means "later", not "no".
fn permanent_rejection_status(err: &anyhow::Error) -> Option<u16> {
    let message = format!("{err:#}").to_ascii_lowercase();
    let start = message.find("status=")? + "status=".len();
    let code: u16 = message[start..].split_whitespace().next()?.parse().ok()?;
    if (400..500).contains(&code) && code != 408 && code != 429 {
        Some(code)
    } else {
        None
    }
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
    save_subdirs: &BTreeMap<String, PathBuf>,
    create_missing_system_dirs: bool,
    psp_savedata_bundles: bool,
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
                // A restore that never ran is the failure mode nobody notices:
                // the run reports success and the device simply does not
                // restore. Say it every time, not only under --verbose.
                eprintln!(
                    "Cloud restore skipped for source '{}': /saves unavailable ({})",
                    source_name, err
                );
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
                save_subdirs,
                create_missing_system_dirs,
                psp_savedata_bundles,
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
                    let label = cloud_save.display_name();
                    eprintln!("Cloud restore error for '{}': {}", label, err);
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
    save_subdirs: &BTreeMap<String, PathBuf>,
    create_missing_system_dirs: bool,
    psp_savedata_bundles: bool,
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
        if verbose {
            eprintln!(
                "Skipping cloud {} save '{}': this helper does not restore {} saves",
                system_slug,
                cloud_save.display_name(),
                system_slug
            );
        }
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

    if let Some(reason) = psp_restore_refusal(cloud_save, &system_slug) {
        report.skipped += 1;
        eprintln!(
            "Not restoring cloud psp save '{}': {}",
            cloud_save.display_name(),
            reason
        );
        return Ok(());
    }
    if system_slug == "psp" {
        return restore_cloud_psp_bundle(
            api,
            cloud_save,
            source_name,
            source_kind,
            source_profile,
            save_roots,
            psp_savedata_bundles,
            fingerprint,
            app_password,
            options,
            sync_entries,
            restored_targets,
            report,
            verbose,
        );
    }

    let effective_profile = effective_profile_for_cloud_restore(source_kind, source_profile);
    let device_type = helper_device_type_for_upload(source_kind, &effective_profile, &system_slug);
    let runtime_target =
        runtime_target_for_system(source_kind, &effective_profile, &system_slug, device_type);
    if verbose {
        eprintln!(
            "Runtime profile on the wire for {}: {:?} (source kind {}, profile {})",
            system_slug,
            runtime_target.runtime_profile,
            source_kind.as_str(),
            effective_profile.as_str()
        );
    }
    let runtime_profile = runtime_target
        .system_profile_value
        .as_deref()
        .or(runtime_target.runtime_profile.as_deref());
    // The cloud track's own format-class (primary vs sidecar), from its stored
    // filename: it gates the "already present" checks below, so a local file of
    // the other class can never satisfy this track's restore.
    let cloud_is_sidecar = std::path::Path::new(&cloud_save.filename)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| crate::scanner::is_sidecar(&value.to_ascii_lowercase()))
        .unwrap_or(false);
    let target_extension = ps1_per_game_card_extension(cloud_save, source_profile)
        .or_else(|| cloud_target_extension(cloud_save, runtime_profile));
    let Some(provisional_path) = cloud_target_path_with_known(
        cloud_save,
        save_roots,
        source_kind,
        &effective_profile,
        &system_slug,
        target_extension.as_deref(),
        sync_entries,
        save_subdirs,
    ) else {
        // Reported by the resolver: the title sits in more than one directory
        // and nothing says which one the emulator reads.
        report.skipped += 1;
        return Ok(());
    };

    if existing_local_save_is_valid(&provisional_path, &system_slug, cloud_is_sidecar) {
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
        && existing_local_save_is_valid(&native_target_path, &system_slug, cloud_is_sidecar)
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
        report.would_download += 1;
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

    if existing_local_save_is_valid(&target_path, &system_slug, cloud_is_sidecar) {
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

/// Restore a PPSSPP SAVEDATA bundle the device does not hold yet.
///
/// A folder that already exists is the upload pass's to compare, so it is left alone here, as an
/// existing single-file save is. The SAVEDATA directory has to exist already: a device without
/// PPSSPP set up has nowhere a game would read the folder from.
#[allow(clippy::too_many_arguments)]
fn restore_cloud_psp_bundle(
    api: &ApiClient,
    cloud_save: &CloudSaveSummary,
    source_name: &str,
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    save_roots: &[PathBuf],
    psp_savedata_bundles: bool,
    fingerprint: &str,
    app_password: Option<&str>,
    options: &SyncOptions,
    sync_entries: &mut HashMap<String, SyncedEntry>,
    restored_targets: &mut HashSet<String>,
    report: &mut SyncReport,
    verbose: bool,
) -> Result<()> {
    if !psp_savedata_bundles {
        report.skipped += 1;
        if verbose {
            eprintln!(
                "Skipping cloud psp save '{}': PSP_SAVEDATA_BUNDLES is off",
                cloud_save.display_name()
            );
        }
        return Ok(());
    }
    let Some(save_dir) = cloud_psp_save_dir(cloud_save) else {
        report.skipped += 1;
        return Ok(());
    };
    let root = select_cloud_restore_root(save_roots);
    let target = psp_savedata_root(root).join(&save_dir);
    let target_key = target.to_string_lossy().to_string();
    if restored_targets.contains(&target_key) {
        return Ok(());
    }
    if fs::symlink_metadata(&target).is_ok() {
        if verbose {
            eprintln!(
                "Cloud restore for psp: {} already holds this save",
                target.display()
            );
        }
        return Ok(());
    }
    let savedata_root = target.parent().map(Path::to_path_buf).unwrap_or_default();
    if !savedata_root.is_dir() {
        report.skipped += 1;
        if verbose {
            eprintln!(
                "Skipping cloud psp save '{}': no PPSSPP SAVEDATA directory at {} for source '{}'",
                cloud_save.display_name(),
                savedata_root.display(),
                source_name
            );
        }
        return Ok(());
    }
    if options.dry_run {
        report.downloaded += 1;
        return Ok(());
    }

    let effective_profile = effective_profile_for_cloud_restore(source_kind, source_profile);
    let device_type = helper_device_type_for_upload(source_kind, &effective_profile, "psp");
    let bundle = api.download_save(&cloud_save.id, device_type, fingerprint, app_password, None)?;
    let files = restore_psp_savedata_bundle(&target, &bundle)?;
    sync_entries.insert(
        target_key.clone(),
        synced_entry(
            sha256_bytes(&bundle),
            Some(psp_line_key(&save_dir)),
            cloud_save.version,
            Some("psp"),
            Some(SaveContainerFormat::Native),
            Some(SaveAdapterProfile::Identity),
            Some(source_kind),
            Some(source_name),
            Some(PSP_BUNDLE_SLOT),
        ),
    );
    restored_targets.insert(target_key);
    report.downloaded += 1;

    if verbose {
        eprintln!(
            "Restored cloud-only psp save '{}' to {} ({} files)",
            cloud_save.display_name(),
            target.display(),
            files
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
            | "ports"
            | "3do"
            | "atari-jaguar"
            | "neogeo-cd"
            | "ngp"
            | "pc-engine"
            | "virtualboy"
    )
}

fn effective_profile_for_cloud_restore(
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
) -> EmulatorProfile {
    match source_kind {
        SourceKind::MisterFpga => EmulatorProfile::Mister,
        SourceKind::RetroArch => EmulatorProfile::RetroArch,
        SourceKind::Ports => EmulatorProfile::Generic,
        _ => source_profile.clone(),
    }
}

fn cloud_target_extension(
    cloud_save: &CloudSaveSummary,
    runtime_profile: Option<&str>,
) -> Option<String> {
    let profile_extension = profile_target_extension(cloud_save, runtime_profile);
    // A sidecar must never land on a primary target: an `.rtc` mapped onto a
    // `.srm` overwrites the real cartridge save with seconds of clock state.
    // The cloud record carries no format field, so sidecar-ness comes from the
    // filename it stored. Sidecar-to-sidecar is a container conversion the
    // profile may legitimately ask for (a MiSTer controller pak writes `.cpk`
    // for a record stored as `.mpk`), so only a sidecar heading for a
    // non-sidecar target is redirected.
    if let (Some(ext), Some(target)) = (
        extension_from_filename(&cloud_save.filename),
        profile_extension.as_deref(),
    ) && crate::scanner::is_sidecar(&ext)
        && !crate::scanner::is_sidecar(target)
    {
        return Some(ext);
    }
    profile_extension
}

fn profile_target_extension(
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

/// The restore target without any record of what has been synced; used by the
/// tests, which have no state to consult. A real restore goes through
/// `cloud_target_path_with_known`.
#[cfg(test)]
fn cloud_target_path(
    cloud_save: &CloudSaveSummary,
    save_roots: &[PathBuf],
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    system_slug: &str,
    target_extension: Option<&str>,
) -> PathBuf {
    cloud_target_path_with_known(
        cloud_save,
        save_roots,
        source_kind,
        source_profile,
        system_slug,
        target_extension,
        &HashMap::new(),
        &BTreeMap::new(),
    )
    .unwrap_or_else(|| {
        // These tests ask about the shape of a path, not about whether the
        // device has decided where its saves live; a fixture with the same
        // title in two directories still has to return a path.
        cloud_target_path_inner(
            cloud_save,
            save_roots,
            source_kind,
            source_profile,
            system_slug,
            target_extension,
            &HashMap::new(),
        )
    })
}

/// Resolve the restore target, letting the directory the emulator actually
/// writes decide when a copy of this title already exists.
///
/// `None` means the save must not be restored: two copies exist and the
/// operator has not declared which one is live. Guessing there is exactly the
/// defect this rule removes, and a save that waits is better than one written
/// where the game cannot see it.
#[allow(clippy::too_many_arguments)]
fn cloud_target_path_with_known(
    cloud_save: &CloudSaveSummary,
    save_roots: &[PathBuf],
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    system_slug: &str,
    target_extension: Option<&str>,
    known_saves: &HashMap<String, SyncedEntry>,
    save_subdirs: &BTreeMap<String, PathBuf>,
) -> Option<PathBuf> {
    let root = select_cloud_restore_root(save_roots);
    let filename = cloud_restore_filename(cloud_save, target_extension);
    let selection_key = save_selection_key(Path::new(filename.trim()));
    let declared = save_subdirs.get(system_slug).map(|path| path.as_path());
    match live_target_for(root, system_slug, &selection_key, declared) {
        LiveTarget::Path { live, strays } => {
            for stray in strays {
                eprintln!(
                    "Stray copy {}: {} is the live directory for {}",
                    stray.join(&filename).display(),
                    live.display(),
                    system_slug
                );
            }
            return Some(live.join(&filename));
        }
        LiveTarget::Ambiguous(directories) => {
            let named: Vec<String> = directories
                .iter()
                .map(|directory| directory.display().to_string())
                .collect();
            eprintln!(
                "Not restoring {} ({}): it exists in {}, and no live directory is declared; set SAVE_SUBDIR_{} on this source",
                filename,
                system_slug,
                named.join(", "),
                system_slug.to_ascii_uppercase()
            );
            return None;
        }
        LiveTarget::FallThrough => {}
    }
    Some(cloud_target_path_inner(
        cloud_save,
        save_roots,
        source_kind,
        source_profile,
        system_slug,
        target_extension,
        known_saves,
    ))
}

fn cloud_target_path_inner(
    cloud_save: &CloudSaveSummary,
    save_roots: &[PathBuf],
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    system_slug: &str,
    target_extension: Option<&str>,
    known_saves: &HashMap<String, SyncedEntry>,
) -> PathBuf {
    let root = select_cloud_restore_root(save_roots);
    // A PPSSPP SAVEDATA bundle is a folder, and PPSSPP decides where it lives.
    if system_slug == "psp"
        && let Some(save_dir) = cloud_psp_save_dir(cloud_save)
    {
        return psp_savedata_root(root).join(save_dir);
    }
    if system_slug == "ports"
        && let Some(root_relative_path) =
            cloud_json_string(&cloud_save.metadata, "/rsm/nativePort/rootRelativePath")
        && safe_cloud_relative_path(&root_relative_path)
    {
        return root.join(root_relative_path);
    }
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
            known_saves,
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
    target_dir
        .join(opera_per_game_subdir(system_slug, &filename))
        .join(filename)
}

/// Opera (3DO) saves a game's NVRAM as `<save dir>/opera/per_game/<game>.<version>.srm` and reads
/// `<save dir>/<game>.srm` only as an unversioned fallback (opera-libretro `opera_lr_nvram.c`), so a
/// versioned 3DO save goes in `opera/per_game` and an unversioned one stays in the save directory.
fn opera_per_game_subdir(system_slug: &str, filename: &str) -> PathBuf {
    let versioned = system_slug == "3do"
        && Path::new(filename)
            .file_stem()
            .and_then(|stem| Path::new(stem).extension())
            .and_then(|version| version.to_str())
            .is_some_and(|version| {
                !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit())
            });
    if versioned {
        PathBuf::from("opera").join("per_game")
    } else {
        PathBuf::new()
    }
}

/// Why a PSP save can't be restored, if it can't. PPSSPP keeps a game's save as a folder,
/// `PSP/SAVEDATA/<dir>/` (the data file, PARAM.SFO, ICON0.PNG), and reads it from nowhere else.
/// Only a record holding that whole folder, and naming it, can put it back; a record of one
/// loose file restores nothing a game can read, whatever ids it carries.
fn psp_restore_refusal(cloud_save: &CloudSaveSummary, system_slug: &str) -> Option<&'static str> {
    if system_slug != "psp" {
        return None;
    }
    if cloud_psp_save_dir(cloud_save).is_some() {
        None
    } else {
        Some(
            "PPSSPP reads a save only as a SAVEDATA/<dir>/ folder, and this record is not a SAVEDATA bundle naming one",
        )
    }
}

/// The SAVEDATA folder a PSP bundle record restores into: `/rsm/psp/saveDir`, present only
/// when `/rsm/psp/bundle` says the payload is the whole folder, and only when it is a plain
/// folder name.
fn cloud_psp_save_dir(cloud_save: &CloudSaveSummary) -> Option<String> {
    let bundle = cloud_json_string(&cloud_save.metadata, "/rsm/psp/bundle")?;
    if bundle != PSP_SAVEDATA_BUNDLE_KIND {
        return None;
    }
    cloud_json_string(&cloud_save.metadata, "/rsm/psp/saveDir")
        .filter(|save_dir| psp_flat_name(save_dir))
}

/// What `/rsm/psp/bundle` holds for a zipped SAVEDATA folder.
const PSP_SAVEDATA_BUNDLE_KIND: &str = "savedata-zip";

/// Where a SAVEDATA folder goes when no SAVEDATA directory exists yet. A restore never creates
/// it, so this only names the path in the log.
const PSP_DEFAULT_SAVEDATA_SUBPATH: &str = "PSP/PPSSPP-SA";

/// The slot a PPSSPP SAVEDATA bundle is stored under. The folder name is the save's identity,
/// so one slot holds it.
const PSP_BUNDLE_SLOT: &str = "default";

/// A name that stays inside the directory it is joined to: no separators, no `..`, no drive
/// or stream syntax.
fn psp_flat_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.trim() == name
        && !name.contains("..")
        && !name.chars().any(|ch| matches!(ch, '/' | '\\' | ':' | '\0'))
}

/// The SAVEDATA directory under a source root: the root itself when it is one, otherwise the
/// first of PPSSPP's known places that exists.
fn psp_savedata_root(root: &Path) -> PathBuf {
    if is_psp_savedata_parent(root) {
        return root.to_path_buf();
    }
    PSP_SAVEDATA_SUBPATHS
        .iter()
        .map(|subpath| root.join(subpath))
        .find(|path| path.is_dir())
        .unwrap_or_else(|| root.join(PSP_DEFAULT_SAVEDATA_SUBPATH))
}

fn psp_line_key(save_dir: &str) -> String {
    format!("psp-savedir:{}", save_dir.to_ascii_uppercase())
}

fn psp_bundle_filename(save_dir: &str) -> String {
    // Never `.zip`: the server explodes a zip into one record per file.
    format!("{save_dir}.pspsave")
}

/// Replace a PPSSPP SAVEDATA folder with the files a bundle holds.
///
/// Everything is checked before anything is written: every entry is a flat file name, and the
/// bundle's PARAM.SFO names the folder it is going into. The files are written to
/// `<dir>.rsm-tmp` beside the folder, the old folder is removed, and the new one is renamed into
/// place. The old folder is not kept: a save is allowed to be overwritten. The SAVEDATA
/// directory itself must already exist; a restore never creates it.
fn restore_psp_savedata_bundle(target_dir: &Path, bundle: &[u8]) -> Result<usize> {
    let save_dir = target_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| psp_flat_name(name))
        .with_context(|| format!("geen geldige PSP SAVEDATA map: {}", target_dir.display()))?;
    let parent = target_dir
        .parent()
        .with_context(|| format!("geen geldige PSP SAVEDATA map: {}", target_dir.display()))?;
    if !parent.is_dir() || !is_psp_savedata_parent(parent) {
        anyhow::bail!(
            "no PPSSPP SAVEDATA directory at {}; a restore does not create one",
            parent.display()
        );
    }

    let entries = unpack_psp_savedata(bundle)?;
    let mut seen = HashSet::new();
    for (name, _) in &entries {
        if !psp_flat_name(name) {
            anyhow::bail!(
                "PSP bundle entry {:?} is not a plain file name; nothing was written",
                name
            );
        }
        if !seen.insert(name.to_ascii_lowercase()) {
            anyhow::bail!("PSP bundle holds {:?} twice; nothing was written", name);
        }
    }
    let param = entries
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("PARAM.SFO"))
        .context("PSP bundle holds no PARAM.SFO; nothing was written")?;
    match parse_param_sfo_savedir(&param.1) {
        Some(named) if named == save_dir => {}
        named => anyhow::bail!(
            "PSP bundle's PARAM.SFO names {:?}, not {:?}; nothing was written",
            named,
            save_dir
        ),
    }

    let staging = parent.join(format!("{save_dir}.rsm-tmp"));
    remove_path_if_present(&staging)?;
    fs::create_dir(&staging)
        .with_context(|| format!("kan map niet maken: {}", staging.display()))?;
    let written = entries.iter().try_for_each(|(name, bytes)| {
        fs::write(staging.join(name), bytes).with_context(|| {
            format!(
                "kan bestand niet schrijven: {}",
                staging.join(name).display()
            )
        })
    });
    if let Err(err) = written {
        let _ = fs::remove_dir_all(&staging);
        return Err(err);
    }
    // No copy of the old folder is kept: a crash between this removal and the rename leaves the
    // folder absent, and the next sync restores it from the server, which still holds the save.
    remove_path_if_present(target_dir)?;
    fs::rename(&staging, target_dir).with_context(|| {
        format!(
            "kan {} niet naar {} hernoemen",
            staging.display(),
            target_dir.display()
        )
    })?;
    Ok(entries.len())
}

fn remove_path_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path)
            .with_context(|| format!("kan map niet verwijderen: {}", path.display())),
        Ok(_) => fs::remove_file(path)
            .with_context(|| format!("kan bestand niet verwijderen: {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("kan pad niet lezen: {}", path.display())),
    }
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

fn safe_cloud_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !path.is_absolute()
        && path.components().all(|component| {
            matches!(component, std::path::Component::Normal(part) if part.to_str().map(|value| !value.trim().is_empty()).unwrap_or(false))
        })
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

fn existing_local_save_is_valid(path: &Path, system_slug: &str, expect_sidecar: bool) -> bool {
    if system_slug == "ports" {
        return path.is_file() && path.metadata().map(|meta| meta.len() > 0).unwrap_or(false);
    }
    // A local file satisfies a cloud track's "already present" check only when
    // it is the same format-class: a kept `.rtc` must not stand in for a deleted
    // `.srm` (which would then never be restored), and a `.srm` must not stand
    // in for a `.rtc`.
    let local_is_sidecar = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| crate::scanner::is_sidecar(&value.to_ascii_lowercase()))
        .unwrap_or(false);
    if local_is_sidecar != expect_sidecar {
        return false;
    }
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

/// Directories a core-sorted emulator keeps another system's saves in.
///
/// Consulted only when looking for a file that already exists, never when
/// choosing where a new save goes: on most devices the system's own directory
/// is the right one. RetroArch with `sort_savefiles_enable` files saves by the
/// core that reads them, and `genesis_plus_gx` serves Genesis and Game Gear
/// alike, so a Game Gear save lives under `genesis/` and the core reads it
/// there. This list is deliberately separate from `system_directory_aliases`,
/// which also decides where new saves are written.
fn writeback_directory_aliases(system_slug: &str) -> &'static [&'static str] {
    match system_slug {
        "game-gear" => &["genesis", "megadrive", "md"],
        _ => &[],
    }
}

/// Which directory a system's saves actually live in.
///
/// A device that sorts saves by core writes them under a directory named after
/// the core (`Mupen64Plus-Next OpenGL`) while the helper's own system
/// directory is `n64`. Both can hold a copy of one title, and the helper
/// cannot infer which one the emulator reads: mtime is useless because the
/// helper's own restores refresh the stale copy, and a table of core names
/// goes stale. So the evidence is the disk, plus one key the operator may set.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LiveTarget {
    /// No copy of this title anywhere under the root: keep the existing rules.
    FallThrough,
    /// The directory the emulator uses, and the other copies, which are strays.
    Path { live: PathBuf, strays: Vec<PathBuf> },
    /// More than one copy and nothing declared: report it and do nothing.
    Ambiguous(Vec<PathBuf>),
}

/// Resolve one title's live directory under a source root.
///
/// Depth is one level below the root, stated rather than assumed: a layout
/// nested deeper is not found here and the caller's disagreement line is what
/// makes that visible. A directory belongs to a system by classifying the
/// saves inside it, not by its name, so a core nobody has seen yet still works.
fn live_target_for(
    root: &Path,
    system_slug: &str,
    selection_key: &str,
    declared: Option<&Path>,
) -> LiveTarget {
    let mut copies: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        let mut directories: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        directories.sort();
        for directory in directories {
            let Ok(files) = std::fs::read_dir(&directory) else {
                continue;
            };
            // Count per directory, not per file: one title's .eep, .sra and
            // .srm sitting side by side are one copy, not three.
            let holds_copy = files.flatten().any(|file| {
                let path = file.path();
                path.is_file()
                    && save_selection_key(&path) == selection_key
                    && classify_supported_save(&path, None)
                        .map(|classification| classification.system_slug == system_slug)
                        .unwrap_or(false)
            });
            if holds_copy {
                copies.push(directory);
            }
        }
    }

    if let Some(subdir) = declared {
        let live = root.join(subdir);
        let strays: Vec<PathBuf> = copies
            .into_iter()
            .filter(|directory| *directory != live)
            .collect();
        return LiveTarget::Path { live, strays };
    }

    match copies.len() {
        0 => LiveTarget::FallThrough,
        1 => LiveTarget::Path {
            live: copies.remove(0),
            strays: Vec::new(),
        },
        _ => LiveTarget::Ambiguous(copies),
    }
}

fn existing_cloud_target_for_alias(
    root: &Path,
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    system_slug: &str,
    filename: &str,
    known_saves: &HashMap<String, SyncedEntry>,
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

    let declared = writeback_directory_aliases(system_slug);
    let directories: Vec<(String, PathBuf)> = std::fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
                .filter_map(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .map(|name| (name.to_string(), entry.path()))
                })
                .collect()
        })
        .unwrap_or_default();

    // A directory the core actually reads from, named by the operator or by the
    // core itself ("Genesis Plus GX"). The directory says which core reads the
    // file, not which system it belongs to: Sonic is a Genesis game and a Game
    // Gear game, and a same-named file already sitting there may be the other
    // one. Only a file this helper has synced for this system is this save.
    for (name, path) in &directories {
        let token = normalize_path_token(name);
        if !declared
            .iter()
            .any(|alias| normalize_path_token(alias) == token)
        {
            continue;
        }
        let exact = path.join(filename);
        if known_save_for_system(&exact, system_slug, known_saves) {
            eprintln!(
                "Restoring into {}: it already holds this {} save",
                exact.display(),
                system_slug
            );
            return Some(exact);
        }
        if exact.exists() {
            eprintln!(
                "Not restoring into {}: it holds a file this helper has not synced as {}",
                exact.display(),
                system_slug
            );
        }

        // The record's filename carries the extension its profile declares, and
        // that is not always the one the core writes: genesis-plus-gx is listed
        // with ".sav" while the device holds ".srm". Matching the stem keeps a
        // profile that disagrees with the core from silently defeating the
        // write-back; the state still decides which file is this system's, and
        // two known files on one stem is a guess this helper does not make.
        let mut matches: Vec<PathBuf> = std::fs::read_dir(path)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|entry| entry.is_file())
                    .filter(|entry| {
                        entry
                            .file_name()
                            .and_then(|name| name.to_str())
                            .map(|name| same_save_stem(name, filename))
                            .unwrap_or(false)
                    })
                    .filter(|entry| known_save_for_system(entry, system_slug, known_saves))
                    .collect()
            })
            .unwrap_or_default();
        matches.sort();
        match matches.len() {
            0 => {}
            1 => {
                eprintln!(
                    "Restoring into {}: it already holds this {} save under another extension",
                    matches[0].display(),
                    system_slug
                );
                return Some(matches[0].clone());
            }
            _ => {
                eprintln!(
                    "More than one file in {} holds the {} stem: {}; using the system directory instead",
                    path.display(),
                    save_stem(filename),
                    matches
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }
    None
}

/// Whether the helper's own state records `path` as a save of `system_slug`.
fn known_save_for_system(
    path: &Path,
    system_slug: &str,
    known_saves: &HashMap<String, SyncedEntry>,
) -> bool {
    known_saves
        .get(path.to_string_lossy().as_ref())
        .and_then(|entry| entry.system_slug.as_deref())
        .map(|slug| slug == system_slug)
        .unwrap_or(false)
}

/// The filename without its extension: "Sonic (USA).srm" and "Sonic (USA).sav"
/// are the same save under two profiles' spellings of it.
fn save_stem(filename: &str) -> &str {
    Path::new(filename)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(filename)
}

fn same_save_stem(candidate: &str, reference: &str) -> bool {
    Path::new(candidate).extension().is_some() && save_stem(candidate) == save_stem(reference)
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
        "3do" => "3do",
        "atari-jaguar" => "atarijaguar",
        "neogeo-cd" => "neogeocd",
        "ngp" => "ngp",
        "pc-engine" => "pcengine",
        "virtualboy" => "virtualboy",
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
        "3do" => &["3do", "panasonic 3do"],
        "atari-jaguar" => &["atarijaguar", "jaguar", "atari jaguar"],
        "neogeo-cd" => &["neogeocd", "neo geo cd", "neogeo cd"],
        "ngp" => &["ngp", "ngpc", "neo geo pocket", "neogeo pocket"],
        "pc-engine" => &["pcengine", "pce", "tg16", "turbografx", "pc engine"],
        "virtualboy" => &["virtualboy", "virtual boy", "vb"],
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
fn process_port_save(
    api: &ApiClient,
    port_save: &PortSaveMatch,
    save_key: &str,
    fingerprint: &str,
    source_name: &str,
    source_kind: &SourceKind,
    source_systems: &[String],
    app_password: Option<&str>,
    options: &SyncOptions,
    report: &mut SyncReport,
    verbose: bool,
) -> Result<Option<ProcessedEntry>> {
    const PORT_SYSTEM_SLUG: &str = "ports";

    if !source_allows_system(source_systems, PORT_SYSTEM_SLUG) {
        report.skipped += 1;
        if verbose {
            eprintln!(
                "Skipping {}: ports are disabled for source '{}'",
                port_save.path.display(),
                source_name
            );
        }
        return Ok(None);
    }

    let bytes = fs::read(&port_save.path)
        .with_context(|| format!("kan port save niet lezen: {}", port_save.path.display()))?;
    if bytes.is_empty() {
        report.skipped += 1;
        return Ok(None);
    }

    let local_sha = sha256_bytes(&bytes);
    let state_key = save_key.to_string();
    let rom_key = format!("port:{}", port_save.port_id);
    let slot_name = port_save.slot_id.as_str();
    let device_type = SourceKind::SteamDeck.helper_device_type();
    let runtime_target = RuntimeTarget {
        runtime_profile: Some(port_save.runtime_profile.clone()),
        emulator_profile: Some(port_save.port_id.clone()),
        system_profile_key: Some("portsProfile".to_string()),
        system_profile_value: Some(port_save.runtime_profile.clone()),
    };
    let upload_metadata = PortUploadMetadata {
        port_id: port_save.port_id.clone(),
        port_name: port_save.port_name.clone(),
        origin_system_slug: port_save.origin_system_slug.clone(),
        port_save_kind: "progress".to_string(),
        relative_path: port_save.relative_path.clone(),
        root_relative_path: port_save.root_relative_path.clone(),
        slot_id: port_save.slot_id.clone(),
        display_title: port_save.display_title.clone(),
    };
    let latest_context = LatestSaveContext {
        filename: port_save
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .map(ToString::to_string)
            .unwrap_or_else(|| "port-save.sav".to_string()),
        system_slug: PORT_SYSTEM_SLUG.to_string(),
        display_title: port_save.display_title.clone(),
        region_code: None,
    };

    if verbose {
        eprintln!(
            "Detected native port save {} [{}:{}]",
            port_save.path.display(),
            port_save.port_id,
            port_save.slot_id
        );
    }

    let latest = api.latest_save(LatestSaveRequest {
        rom_sha1: &rom_key,
        slot_name,
        device_type,
        fingerprint,
        app_password,
        runtime_target: Some(&runtime_target),
        context: Some(&latest_context),
    })?;

    let upload_port_save = |report: &mut SyncReport| -> Result<Option<ProcessedEntry>> {
        if options.dry_run {
            report.would_upload += 1;
            return Ok(Some(processed_entry(
                state_key.clone(),
                synced_entry(
                    local_sha.clone(),
                    Some(rom_key.clone()),
                    latest.version,
                    Some(PORT_SYSTEM_SLUG),
                    Some(SaveContainerFormat::Native),
                    Some(SaveAdapterProfile::Identity),
                    Some(source_kind),
                    Some(source_name),
                    Some(slot_name),
                ),
            )));
        }

        let filename = port_save
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("port-save.sav");
        let response = api.upload_save(
            filename,
            bytes.clone(),
            &rom_key,
            None,
            slot_name,
            device_type,
            fingerprint,
            app_password,
            Some(PORT_SYSTEM_SLUG),
            None,
            Some(&runtime_target),
            Some(&upload_metadata),
            None,
        )?;
        report.uploaded += 1;
        Ok(Some(processed_entry(
            state_key.clone(),
            synced_entry(
                local_sha.clone(),
                Some(rom_key.clone()),
                response
                    .save
                    .and_then(|save| save.version)
                    .or(latest.version),
                Some(PORT_SYSTEM_SLUG),
                Some(SaveContainerFormat::Native),
                Some(SaveAdapterProfile::Identity),
                Some(source_kind),
                Some(source_name),
                Some(slot_name),
            ),
        )))
    };

    if !latest.exists || options.force_upload {
        return upload_port_save(report);
    }

    if latest.sha256.as_deref() == Some(local_sha.as_str()) {
        report.in_sync += 1;
        return Ok(Some(processed_entry(
            state_key.clone(),
            synced_entry(
                local_sha,
                Some(rom_key),
                latest.version,
                Some(PORT_SYSTEM_SLUG),
                Some(SaveContainerFormat::Native),
                Some(SaveAdapterProfile::Identity),
                Some(source_kind),
                Some(source_name),
                Some(slot_name),
            ),
        )));
    }

    let conflict = api.conflict_check(
        &rom_key,
        slot_name,
        device_type,
        fingerprint,
        app_password,
        Some(&runtime_target),
    )?;
    if conflict.exists {
        if !options.dry_run {
            let cloud_sha = conflict
                .cloud_sha256
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            let device_name = format!("{} ({})", source_kind.as_str(), source_name);
            let filename = port_save
                .path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("port-save.sav");
            let _ = api.conflict_report(
                filename,
                bytes,
                &rom_key,
                slot_name,
                &local_sha,
                &cloud_sha,
                &device_name,
                device_type,
                fingerprint,
                app_password,
                Some(&runtime_target),
            )?;
        }
        report.conflicts += 1;
        return Ok(Some(processed_entry(
            state_key.clone(),
            synced_entry(
                local_sha,
                Some(rom_key),
                latest.version,
                Some(PORT_SYSTEM_SLUG),
                Some(SaveContainerFormat::Native),
                Some(SaveAdapterProfile::Identity),
                Some(source_kind),
                Some(source_name),
                Some(slot_name),
            ),
        )));
    }

    let Some(save_id) = latest.id.as_deref() else {
        report.skipped += 1;
        return Ok(None);
    };
    if options.dry_run {
        report.would_download += 1;
        return Ok(Some(processed_entry(
            state_key.clone(),
            synced_entry(
                local_sha,
                Some(rom_key),
                latest.version,
                Some(PORT_SYSTEM_SLUG),
                Some(SaveContainerFormat::Native),
                Some(SaveAdapterProfile::Identity),
                Some(source_kind),
                Some(source_name),
                Some(slot_name),
            ),
        )));
    }

    let cloud_bytes = api.download_save(
        save_id,
        device_type,
        fingerprint,
        app_password,
        Some(&runtime_target),
    )?;
    fs::write(&port_save.path, &cloud_bytes).with_context(|| {
        format!(
            "kan port save niet overschrijven: {}",
            port_save.path.display()
        )
    })?;
    report.downloaded += 1;
    Ok(Some(processed_entry(
        state_key,
        synced_entry(
            sha256_bytes(&cloud_bytes),
            Some(rom_key),
            latest.version,
            Some(PORT_SYSTEM_SLUG),
            Some(SaveContainerFormat::Native),
            Some(SaveAdapterProfile::Identity),
            Some(source_kind),
            Some(source_name),
            Some(slot_name),
        ),
    )))
}

/// Sync one PPSSPP `SAVEDATA/<dir>/` folder as a single bundle.
///
/// The folder is packed the same way every time, so its sha256 is compared with the server's
/// latest for the folder's line, as a port save is. A local change since the last sync uploads;
/// a server change over an unchanged folder replaces the folder; a conflict is reported the way a
/// single-file save's is.
#[allow(clippy::too_many_arguments)]
fn process_psp_savedata_dir(
    api: &ApiClient,
    dir: &Path,
    save_key: &str,
    prior_local_sha: Option<&str>,
    fingerprint: &str,
    source_name: &str,
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    source_systems: &[String],
    app_password: Option<&str>,
    options: &SyncOptions,
    report: &mut SyncReport,
    verbose: bool,
) -> Result<Option<ProcessedEntry>> {
    const PSP_SYSTEM_SLUG: &str = "psp";

    if !source_allows_system(source_systems, PSP_SYSTEM_SLUG) {
        report.skipped += 1;
        if verbose {
            eprintln!(
                "Skipping {}: psp is disabled for source '{}'",
                dir.display(),
                source_name
            );
        }
        return Ok(None);
    }
    let save_dir = dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| psp_flat_name(name))
        .map(ToString::to_string)
        .with_context(|| format!("geen geldige PSP SAVEDATA map: {}", dir.display()))?;

    let (bytes, local_sha) = pack_psp_savedata(dir)?;
    let state_key = save_key.to_string();
    let rom_key = psp_line_key(&save_dir);
    let filename = psp_bundle_filename(&save_dir);
    let slot_name = PSP_BUNDLE_SLOT;
    let effective_profile = effective_profile_for_cloud_restore(source_kind, source_profile);
    let device_type =
        helper_device_type_for_upload(source_kind, &effective_profile, PSP_SYSTEM_SLUG);
    let entry_for = |sha: String, version: Option<i64>| {
        processed_entry(
            state_key.clone(),
            synced_entry(
                sha,
                Some(rom_key.clone()),
                version,
                Some(PSP_SYSTEM_SLUG),
                Some(SaveContainerFormat::Native),
                Some(SaveAdapterProfile::Identity),
                Some(source_kind),
                Some(source_name),
                Some(slot_name),
            ),
        )
    };

    if verbose {
        eprintln!(
            "Detected PPSSPP SAVEDATA folder {} ({} bytes packed, line {})",
            dir.display(),
            bytes.len(),
            rom_key
        );
    }

    let latest_context = LatestSaveContext {
        filename: filename.clone(),
        system_slug: PSP_SYSTEM_SLUG.to_string(),
        display_title: save_dir.clone(),
        region_code: None,
    };
    let latest = api.latest_save(LatestSaveRequest {
        rom_sha1: &rom_key,
        slot_name,
        device_type,
        fingerprint,
        app_password,
        runtime_target: None,
        context: Some(&latest_context),
    })?;

    let upload = |report: &mut SyncReport| -> Result<Option<ProcessedEntry>> {
        if options.dry_run {
            report.uploaded += 1;
            return Ok(Some(entry_for(local_sha.clone(), latest.version)));
        }
        let response = api.upload_save(
            &filename,
            bytes.clone(),
            &rom_key,
            None,
            slot_name,
            device_type,
            fingerprint,
            app_password,
            Some(PSP_SYSTEM_SLUG),
            None,
            None,
            None,
            Some(&save_dir),
        )?;
        report.uploaded += 1;
        Ok(Some(entry_for(
            local_sha.clone(),
            response
                .save
                .and_then(|save| save.version)
                .or(latest.version),
        )))
    };

    if !latest.exists || options.force_upload {
        return upload(report);
    }
    if latest.sha256.as_deref() == Some(local_sha.as_str()) {
        report.in_sync += 1;
        return Ok(Some(entry_for(local_sha.clone(), latest.version)));
    }

    let conflict = api.conflict_check(
        &rom_key,
        slot_name,
        device_type,
        fingerprint,
        app_password,
        None,
    )?;
    if conflict.exists {
        if !options.dry_run {
            let cloud_sha = conflict
                .cloud_sha256
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            let device_name = format!("{} ({})", source_kind.as_str(), source_name);
            let _ = api.conflict_report(
                &filename,
                bytes.clone(),
                &rom_key,
                slot_name,
                &local_sha,
                &cloud_sha,
                &device_name,
                device_type,
                fingerprint,
                app_password,
                None,
            )?;
        }
        report.conflicts += 1;
        return Ok(Some(entry_for(local_sha.clone(), latest.version)));
    }

    // The same rule as a single-file save: a folder that changed since this device last synced
    // it is a local edit and uploads; otherwise the server's copy replaces it.
    let local_edited_since_sync = prior_local_sha
        .map(|prior| prior != local_sha.as_str())
        .unwrap_or(false);
    if local_edited_since_sync {
        return upload(report);
    }

    let Some(save_id) = latest.id.as_deref() else {
        report.skipped += 1;
        return Ok(None);
    };
    if options.dry_run {
        report.downloaded += 1;
        return Ok(Some(entry_for(local_sha.clone(), latest.version)));
    }
    let cloud_bytes = api.download_save(save_id, device_type, fingerprint, app_password, None)?;
    let files = restore_psp_savedata_bundle(dir, &cloud_bytes)?;
    report.downloaded += 1;
    if verbose {
        eprintln!(
            "Replaced {} with record {} ({} files)",
            dir.display(),
            save_id,
            files
        );
    }
    // The downloaded bundle's hash, not a repack: if another packer made it, the next pass sees
    // the folder as a local edit and stores this helper's canonical form once.
    Ok(Some(entry_for(sha256_bytes(&cloud_bytes), latest.version)))
}

/// Whether a ROM index can say a PS1 ROM is missing: it holds at least one ROM
/// under a PlayStation folder. A source whose ROM root is its save root indexes
/// an emulator's own `.bin` files, so its index is not empty yet holds no PS1
/// ROM, and a card's ROM missing from it says nothing.
fn rom_index_holds_ps1_roms(rom_index: &HashMap<String, RomIndexEntry>) -> bool {
    rom_index.values().any(|entry| {
        entry.path.components().any(|component| {
            let name = component.as_os_str().to_string_lossy().to_ascii_lowercase();
            name == "psx"
                || name == "ps1"
                || (name.contains("playstation")
                    && !name.contains("playstation 2")
                    && !name.contains("portable")
                    && !name.contains("vita"))
        })
    })
}

#[allow(clippy::too_many_arguments)]
fn process_single_save(
    api: &ApiClient,
    save_path: &std::path::Path,
    save_key: &str,
    prior_entry: Option<&SyncedEntry>,
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
    // A PS1 card is named after its game, and the server keeps one line per
    // card. A card whose name matches no ROM here (a title-named card a game
    // or a converter wrote) opens a line of its own on the server, which then
    // moves the game's save onto it and away from the game's card. Only an
    // index that loaded can say a ROM is missing; with no ROM root the card
    // syncs as before.
    if rom_entry.is_none()
        && rom_index_holds_ps1_roms(rom_index)
        && system_slug == "psx"
        && infer_playstation_slot_from_path(save_path).starts_with(PS1_PER_GAME_SLOT_PREFIX)
    {
        report.skipped += 1;
        eprintln!(
            "Not uploading {}: a PS1 card named after no ROM in the index would open its own line on the server",
            save_path.display()
        );
        return Ok(None);
    }
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
    } else if system_slug == "sega-cd" && crate::scanner::is_sega_cd_shared_bram(save_path) {
        crate::scanner::sega_cd_bram_line_key(device_type, save_path)
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

    let latest_context = latest_save_context(save_path, &system_slug);
    let latest = match api.latest_save(LatestSaveRequest {
        rom_sha1: &active_rom_sha1,
        slot_name: &effective_slot_name,
        device_type,
        fingerprint,
        app_password,
        runtime_target: Some(&runtime_target),
        context: Some(&latest_context),
    }) {
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
                card_slot: None,
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
                card_slot: None,
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
                card_slot: None,
            }
        }
        Err(err) => return Err(err),
    };

    // The server holding these bytes means the save is in sync only if this
    // device last synced it under the card it is offered under now. A save
    // that moved cards (a per-game card newly holding it) is a different offer
    // of the same bytes: the server moves the save onto the line the new card
    // names, so it is uploaded rather than treated as in sync or restored.
    let moved_to_another_card = prior_entry
        .and_then(|entry| entry.slot_name.as_deref())
        .map(str::trim)
        .filter(|slot| !slot.is_empty())
        .map(|slot| !slot.eq_ignore_ascii_case(effective_slot_name.trim()))
        .unwrap_or(false)
        // The device's own state is not the only witness to a move: the server
        // can re-home a save (another device, a manual repair) while this device
        // still records the card it last synced. The card the server reports is
        // compared as well, so an unchanged-bytes offer still uploads and the
        // save lands on the card this device names.
        || latest
            .card_slot
            .as_deref()
            .map(str::trim)
            .filter(|slot| !slot.is_empty())
            .map(|slot| !slot.eq_ignore_ascii_case(effective_slot_name.trim()))
            .unwrap_or(false);
    if !latest.exists || moved_to_another_card {
        if options.dry_run {
            report.would_upload += 1;
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
        let upload_version = match upload_result {
            Ok(Some(version)) => Some(version),
            Ok(None) => {
                // 2xx with no `save`: the server accepted the request but stored
                // nothing.  Counting this as uploaded is how a client reports a
                // hundred successful uploads while the store never grows.
                report.accepted_not_stored += 1;
                eprintln!(
                    "Server accepted {} but returned no save; nothing was stored",
                    save_path.display()
                );
                return Ok(None);
            }
            Err(err) => {
                if is_empty_n64_controller_pak_rejection(&err, &system_slug) {
                    report.rejected_n64_controller_pak += 1;
                    eprintln!(
                        "Rejected empty N64 controller pak {}: {}",
                        save_path.display(),
                        err
                    );
                    return Ok(None);
                }
                return Err(err);
            }
        };

        report.uploaded += 1;
        return Ok(Some(processed_entry(
            state_key.clone(),
            synced_entry(
                local_sha,
                Some(active_rom_sha1.clone()),
                upload_version.or(latest.version),
                Some(&system_slug),
                Some(normalized_save.local_container),
                Some(normalized_save.adapter_profile),
                Some(source_kind),
                Some(source_name),
                Some(&effective_slot_name),
            ),
        )));
    }

    // What did both sides last agree on?  Comparing local against remote alone
    // cannot tell "the user just saved" from "the server has something new", so a
    // divergence was resolved by asking the server, which can only see its own side.
    let (local_moved, remote_moved) = match prior_entry {
        Some(prior) => {
            let last = prior.sha256.as_str();
            let remote = latest.sha256.as_deref().unwrap_or("");
            (last != local_sha.as_str(), last != remote)
        }
        // Nothing agreed yet (first sync): not a conflict, fall through.
        None => (false, false),
    };

    if local_moved && remote_moved {
        // Both sides moved, and neither is more real.  Touch neither.
        let remote_sha = latest.sha256.as_deref().unwrap_or("");
        report.conflicts += 1;
        eprintln!(
            "Sync conflict for {}: local {} and server {} both changed since the last sync; leaving both untouched",
            save_path.display(),
            &local_sha[..local_sha.len().min(8)],
            &remote_sha[..remote_sha.len().min(8)]
        );
        return Ok(Some(processed_entry(
            state_key.clone(),
            prior_entry
                .expect("conflict requires a prior entry")
                .clone(),
        )));
    }

    // Only the local side moved: the ordinary "player just saved" case.  Upload it.
    if options.force_upload || local_moved {
        if options.dry_run {
            report.would_upload += 1;
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
        let upload_version = match upload_result {
            Ok(Some(version)) => Some(version),
            Ok(None) => {
                // 2xx with no `save`: the server accepted the request but stored
                // nothing.  Counting this as uploaded is how a client reports a
                // hundred successful uploads while the store never grows.
                report.accepted_not_stored += 1;
                eprintln!(
                    "Server accepted {} but returned no save; nothing was stored",
                    save_path.display()
                );
                return Ok(None);
            }
            Err(err) => {
                if is_empty_n64_controller_pak_rejection(&err, &system_slug) {
                    report.rejected_n64_controller_pak += 1;
                    eprintln!(
                        "Rejected empty N64 controller pak {}: {}",
                        save_path.display(),
                        err
                    );
                    return Ok(None);
                }
                return Err(err);
            }
        };
        report.uploaded += 1;
        return Ok(Some(processed_entry(
            state_key.clone(),
            synced_entry(
                local_sha,
                Some(active_rom_sha1.clone()),
                upload_version.or(latest.version),
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
                report.would_upload += 1;
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
                None,
                None,
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
                    None,
                    None,
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
            report.would_download += 1;
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
                    None,
                    None,
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

    let latest = match api.latest_save(LatestSaveRequest {
        rom_sha1,
        slot_name: &effective_slot_name,
        device_type,
        fingerprint,
        app_password,
        runtime_target: Some(&runtime_target),
        context: None,
    }) {
        Ok(value) => value,
        Err(err) if is_missing_cloud_payload_reference(&err) => LatestSaveResponse {
            exists: false,
            sha256: None,
            version: None,
            id: None,
            card_slot: None,
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
        report.would_download += 1;
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

fn latest_save_context(save_path: &Path, system_slug: &str) -> LatestSaveContext {
    LatestSaveContext {
        filename: save_path
            .file_name()
            .and_then(|value| value.to_str())
            .map(ToString::to_string)
            .unwrap_or_else(|| upload_filename_for_sync(save_path, system_slug)),
        system_slug: system_slug.to_string(),
        display_title: filename_stem(save_path),
        region_code: None,
    }
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
        SourceKind::Ports => SourceKind::SteamDeck.helper_device_type(),
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
        SourceKind::Ports => "ports",
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
        // The profile is required by the server for every projection-capable
        // system, on the latest check, the upload and the download alike. A
        // system with no arm here sent no profile and was refused with
        // "runtimeProfile is required" -- game boy and DS saves never synced
        // for that reason alone.
        "gameboy" => Some(
            match source_profile {
                EmulatorProfile::Mister => "gameboy/mister",
                EmulatorProfile::RetroArch => "gameboy/retroarch-gambatte",
                _ => "gameboy/gambatte",
            }
            .to_string(),
        ),
        "nds" => Some(
            match source_profile {
                EmulatorProfile::RetroArch => "nds/retroarch-melonds",
                _ => "nds/melonds",
            }
            .to_string(),
        ),
        "pc-engine" => Some(
            match source_profile {
                EmulatorProfile::Mister => "pc-engine/mister",
                EmulatorProfile::RetroArch => "pc-engine/retroarch-beetle-pce",
                _ => "pc-engine/mednafen",
            }
            .to_string(),
        ),
        "3do" => Some(
            match source_profile {
                EmulatorProfile::RetroArch => "3do/retroarch-opera",
                _ => "3do/opera",
            }
            .to_string(),
        ),
        "atari-jaguar" => Some(
            match source_profile {
                EmulatorProfile::RetroArch => "atari-jaguar/retroarch-virtual-jaguar",
                _ => "atari-jaguar/virtual-jaguar",
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
        "sega-cd" => Some(
            match source_profile {
                EmulatorProfile::RetroArch => "sega-cd/retroarch-genesis-plus-gx",
                _ => "sega-cd/genesis-plus-gx",
            }
            .to_string(),
        ),
        "sega-32x" => Some(
            match source_profile {
                EmulatorProfile::RetroArch => "sega-32x/retroarch-genesis-plus-gx",
                _ => "sega-32x/genesis-plus-gx",
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
        | SourceKind::Ports
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

/// A PS1 card the emulator keeps per game (RetroArch's Beetle PSX without
/// shared cards, the MiSTer) is named after that game and holds only its saves.
/// The server keeps one line per card slot, so such a card gets its own slot,
/// "Game: <name>"; sharing "Memory Card 1" made each game's card read as a
/// deletion of every other game's save. A numbered card (memory_card_1, mcd001)
/// is a shared card and keeps its number, and so does a PS2 card.
const PS1_PER_GAME_SLOT_PREFIX: &str = "Game: ";

fn infer_playstation_slot_from_path(path: &Path) -> String {
    let text = path.to_string_lossy().to_ascii_lowercase();
    if let Some(slot) = parse_playstation_slot(&text) {
        return slot;
    }
    let is_ps1_card = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| !value.eq_ignore_ascii_case("ps2"))
        .unwrap_or(false);
    match path.file_stem().and_then(|value| value.to_str()) {
        Some(stem) if is_ps1_card && !stem.trim().is_empty() => {
            format!("{PS1_PER_GAME_SLOT_PREFIX}{}", stem.trim())
        }
        _ => "Memory Card 1".to_string(),
    }
}

/// The extension a per-game PS1 card has on this device: the server names the
/// card after its game but knows only the `.mcr` container, while RetroArch
/// reads `<game>.srm` and the MiSTer `<game>.sav`.
fn ps1_per_game_card_extension(
    cloud_save: &CloudSaveSummary,
    source_profile: &EmulatorProfile,
) -> Option<String> {
    let slot = cloud_save.card_slot.as_deref()?.trim();
    if !slot.starts_with(PS1_PER_GAME_SLOT_PREFIX)
        || !cloud_save.system_slug.eq_ignore_ascii_case("psx")
    {
        return None;
    }
    Some(
        match source_profile {
            EmulatorProfile::Mister => "sav",
            _ => "srm",
        }
        .to_string(),
    )
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
    let stem = filename_stem(save_path).to_ascii_lowercase();
    // Additive save extensions are NOT alternative formats of the main save —
    // they carry independent state that must NOT be deduplicated against the
    // primary battery save. Examples:
    //   .rtc — GBC real-time clock state (Pokemon Crystal/Gold/Silver,
    //          Harvest Moon GBC, etc). The .srm is the SRAM; the .rtc is
    //          the day/night clock. Dropping .rtc loses time-based events
    //          like Pokemon Crystal evolutions and the in-game day cycle.
    //   .mpk / .cpk — N64 controller pak / memory pak. Independent of the
    //          main .eep / .fla / .sra battery save.
    // Give these their own selection key so select_preferred_save_per_stem
    // doesn't pick one variant and silently drop the others.
    if let Some(ext) = save_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        && matches!(ext.as_str(), "rtc" | "mpk" | "cpk")
    {
        return format!("{}:{}", stem, ext);
    }
    stem
}

/// Whether this copy may upload, and the reason when it may not.
///
/// The restore rule and this one are the same rule: the directory the emulator
/// actually writes is the one that syncs. Declared wins; two copies with
/// nothing declared is a conflict the helper refuses to resolve, because a
/// wrong upload replaces a good record rather than merely landing in a place
/// the game cannot see.
fn live_upload_blocked(
    source: &ResolvedSource,
    save_path: &Path,
    selection_key: &str,
) -> Option<String> {
    let root = source.save_roots.first()?;
    let classification = classify_supported_save(save_path, None)?;
    let system_slug = classification.system_slug;
    let declared = source
        .save_subdirs
        .get(&system_slug)
        .map(|path| path.as_path());
    match live_target_for(root, &system_slug, selection_key, declared) {
        LiveTarget::FallThrough => None,
        LiveTarget::Path { live, .. } => {
            if save_path.parent() == Some(live.as_path()) {
                return None;
            }
            Some(format!(
                "Not uploading {}: it is a stray copy, and {} is the live directory for {} in source '{}'",
                save_path.display(),
                live.display(),
                system_slug,
                source.name
            ))
        }
        LiveTarget::Ambiguous(directories) => {
            let named: Vec<String> = directories
                .iter()
                .map(|directory| directory.display().to_string())
                .collect();
            Some(format!(
                "Not uploading {}: it exists in {}, and no live directory is declared; set SAVE_SUBDIR_{} on source '{}'",
                save_path.display(),
                named.join(", "),
                system_slug.to_ascii_uppercase(),
                source.name
            ))
        }
    }
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
    // A Sega CD backup RAM is read by its own name (scd_U.brm, cart.brm); the
    // cartridge .srm rule would write a file Genesis Plus GX never opens.
    if system_slug == "sega-cd" && current_extension.as_deref() == Some("brm") {
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
) -> Result<Option<i64>> {
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
        None,
        None,
    );
    let upload_response = match upload_result {
        Ok(response) => response,
        Err(err) => {
            if should_retry_n64_mister_cpk_as_mpk(&err, system_slug, runtime_target, filename)
                && let Some(fallback_filename) = filename_with_extension(filename, "mpk")
            {
                if verbose {
                    eprintln!(
                        "Retrying N64 controller pak upload as {} (legacy backend compatibility)",
                        fallback_filename
                    );
                }
                api.upload_save(
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
                    None,
                    None,
                )?
            } else {
                return Err(err);
            }
        }
    };
    Ok(upload_response.save.and_then(|save| save.version))
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

    // The server's error text is `… status=<code> <reason> body=…`; the parse
    // has to take the code and decide on it, not on the reason phrase.
    #[test]
    fn permanent_rejection_status_reads_the_status_code() {
        let refused = anyhow::anyhow!(
            "POST /saves (multipart upload): HTTP request faalde: status=422 Unprocessable Entity body={{}}"
        );
        assert_eq!(permanent_rejection_status(&refused), Some(422));
        let bad = anyhow::anyhow!("HTTP request faalde: status=400 Bad Request body={{}}");
        assert_eq!(permanent_rejection_status(&bad), Some(400));
        for later in [
            "HTTP request faalde: status=408 Request Timeout body={}",
            "HTTP request faalde: status=429 Too Many Requests body={}",
            "HTTP request faalde: status=500 Internal Server Error body={}",
            "HTTP request faalde: status=503 Service Unavailable body={}",
            "connection refused",
        ] {
            assert_eq!(
                permanent_rejection_status(&anyhow::anyhow!("{later}")),
                None,
                "{later}"
            );
        }
    }
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
    fn save_selection_key_separates_additive_extensions() {
        // Pokemon Crystal saves both .srm (battery SRAM) and .rtc (real-time
        // clock state). They are NOT alternative formats of each other —
        // dropping .rtc loses time-based events (Pokemon evolutions, day/night
        // cycle). They must get DIFFERENT selection keys so they don't dedup
        // against each other in select_preferred_save_per_stem.
        let srm = PathBuf::from(
            "/run/media/x/saves/gbc/Pokemon - Crystal Version (USA, Europe) (Rev 1).srm",
        );
        let rtc = PathBuf::from(
            "/run/media/x/saves/gbc/Pokemon - Crystal Version (USA, Europe) (Rev 1).rtc",
        );
        let srm_key = save_selection_key(&srm);
        let rtc_key = save_selection_key(&rtc);
        assert_ne!(
            srm_key, rtc_key,
            ".srm and .rtc must have distinct selection keys (issue: Pokemon Crystal clock state was being dropped)"
        );
        // Sanity: same stem still produces deterministic per-extension keys.
        assert_eq!(
            rtc_key,
            "pokemon - crystal version (usa, europe) (rev 1):rtc"
        );
    }

    #[test]
    fn save_selection_key_separates_n64_controller_pak_from_battery_save() {
        // N64 .mpk / .cpk = controller pak (separate save device, used for
        // games like Mario Kart 64 ghost data, F-Zero X custom courses).
        // .eep / .fla / .sra = main battery save. Must not dedup.
        let eep = PathBuf::from("/saves/n64/Mario Kart 64 (USA).eep");
        let mpk = PathBuf::from("/saves/n64/Mario Kart 64 (USA).mpk");
        let cpk = PathBuf::from("/saves/n64/Mario Kart 64 (USA).cpk");
        let eep_key = save_selection_key(&eep);
        let mpk_key = save_selection_key(&mpk);
        let cpk_key = save_selection_key(&cpk);
        assert_ne!(eep_key, mpk_key, ".eep and .mpk must have distinct keys");
        assert_ne!(eep_key, cpk_key, ".eep and .cpk must have distinct keys");
        assert_ne!(
            mpk_key, cpk_key,
            ".mpk and .cpk must have distinct keys (different controller-pak slots)"
        );
    }

    #[test]
    fn save_selection_key_dedups_real_alternative_formats() {
        // Regression guard: ALTERNATIVE formats of the same save (e.g. a
        // user with both .sav and .srm versions of the same game) MUST
        // still collide so dedup picks the preferred one.
        let sav = PathBuf::from("/saves/snes/Super Mario World.sav");
        let srm = PathBuf::from("/saves/snes/Super Mario World.srm");
        assert_eq!(
            save_selection_key(&sav),
            save_selection_key(&srm),
            "true alternative formats (.sav vs .srm of same game) must still dedup"
        );
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

    // GBC-RTC clobber: a profile that targets the primary extension must not
    // pull a sidecar onto it. An 8-byte clock state written over a 32 KiB
    // cartridge save destroys the save.
    #[test]
    fn a_sidecar_restores_to_its_own_extension() {
        let save = cloud_save(
            "Pokemon - Crystal Version (USA, Europe) (Rev 1).rtc",
            "Pokemon - Crystal Version",
            "gameboy",
            "gameboy/retroarch-gambatte",
            ".srm",
        );
        assert_eq!(
            cloud_target_extension(&save, Some("gameboy/retroarch-gambatte")).as_deref(),
            Some("rtc"),
            "the sidecar was mapped onto the profile's primary extension"
        );
    }

    #[test]
    fn a_primary_still_takes_the_profiles_extension() {
        let save = cloud_save(
            "Pokemon - Crystal Version (USA, Europe) (Rev 1).srm",
            "Pokemon - Crystal Version",
            "gameboy",
            "gameboy/retroarch-gambatte",
            ".srm",
        );
        assert_eq!(
            cloud_target_extension(&save, Some("gameboy/retroarch-gambatte")).as_deref(),
            Some("srm")
        );
    }

    #[test]
    fn existing_local_save_class_gate() {
        // Controller paks are the format class that classifies on this tree --
        // an `.rtc` is deliberately never a standalone save -- so they are what
        // the gate can be proven on. Real files, because a missing path returns
        // false for the wrong reason and proves nothing.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("saves/n64");
        fs::create_dir_all(&dir).unwrap();
        let mpk = dir.join("Mario Kart 64 (USA)_1.mpk");
        let cpk = dir.join("Mario Kart 64 (USA)_1.cpk");
        fs::write(&mpk, vec![0xA5u8; 32768]).unwrap();
        fs::write(&cpk, vec![0xA5u8; 32768]).unwrap();

        // Same class is accepted: a present pak does not make the device fetch
        // its own copy again. cpk is what the profile writes; mpk is the record.
        assert!(
            existing_local_save_is_valid(&cpk, "n64", true),
            "a present sidecar was not accepted for a sidecar track"
        );
        assert!(
            existing_local_save_is_valid(&mpk, "n64", true),
            "a present controller pak of the record's own container was not accepted"
        );

        // A sidecar never satisfies a primary track: before the gate, any file
        // the classifier accepted counted as "already present", so a pak stood
        // in for a cartridge save that had been deleted.
        assert!(
            !existing_local_save_is_valid(&mpk, "n64", false),
            "a controller pak satisfied a primary track's presence check"
        );
        assert!(
            !existing_local_save_is_valid(&cpk, "n64", false),
            "a controller pak satisfied a primary track's presence check"
        );
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
    fn classifies_permanent_rejections() {
        let err = anyhow::Error::msg(
            "HTTP request faalde: status=422 Unprocessable Entity body={\"reason\":\"unsupported\"}",
        );
        assert_eq!(permanent_rejection_status(&err), Some(422));
    }

    #[test]
    fn rate_limit_and_timeout_are_not_permanent() {
        for code in [408, 429] {
            let err = anyhow::Error::msg(format!("status={code} Too Many Requests"));
            assert_eq!(permanent_rejection_status(&err), None, "{code} must retry");
        }
    }

    #[test]
    fn server_errors_and_transport_failures_are_not_permanent() {
        let err = anyhow::Error::msg("status=503 Service Unavailable");
        assert_eq!(permanent_rejection_status(&err), None);
        let err = anyhow::Error::msg("connection refused");
        assert_eq!(permanent_rejection_status(&err), None);
    }

    #[test]
    fn runtime_target_returns_empty_for_non_projection_systems() {
        // A system the server publishes no projection for stays empty. DS used
        // to be in this test, which is how the missing profile became a silent
        // 400 on every DS save: the assertion encoded the defect.
        let target = runtime_target_for_system(
            &SourceKind::RetroArch,
            &EmulatorProfile::RetroArch,
            "neogeo",
            "retroarch",
        );
        assert!(target.runtime_profile.is_none());
        assert!(target.system_profile_key.is_none());
        assert!(target.system_profile_value.is_none());

        let nds = runtime_target_for_system(
            &SourceKind::RetroArch,
            &EmulatorProfile::RetroArch,
            "nds",
            "retroarch",
        );
        assert_eq!(
            nds.runtime_profile.as_deref(),
            Some("nds/retroarch-melonds")
        );
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
    fn cloud_restore_accepts_every_system_the_server_holds() {
        // A save another helper uploaded for one of these systems has to come
        // back down; a system missing here is dropped before any target is chosen.
        for (slug, filename, folder) in [
            ("3do", "Star Control II (USA).srm", "3do"),
            ("atari-jaguar", "Doom (World).srm", "atarijaguar"),
            ("neogeo-cd", "Samurai Shodown RPG (Japan).srm", "neogeocd"),
            (
                "ngp",
                "Baseball Stars - Pocket Sports Series (Japan, Europe) (En,Ja).flash",
                "ngp",
            ),
            ("pc-engine", "Neutopia (USA).srm", "pcengine"),
            ("virtualboy", "3-D Tetris (USA).srm", "virtualboy"),
        ] {
            assert!(
                supports_cloud_restore_system(slug),
                "{slug} is not restored"
            );
            let save = cloud_save(filename, "Game", slug, "original", ".srm");
            let target = cloud_target_path(
                &save,
                &[PathBuf::from("/run/media/deck/retrodeck/saves")],
                &SourceKind::RetroArch,
                &EmulatorProfile::RetroArch,
                slug,
                Path::new(filename)
                    .extension()
                    .and_then(|value| value.to_str()),
            );
            assert_eq!(
                target,
                PathBuf::from("/run/media/deck/retrodeck/saves")
                    .join(folder)
                    .join(filename),
                "{slug}"
            );
        }
    }

    #[test]
    fn cloud_restore_uses_an_existing_alias_folder_for_ngp() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("saves");
        fs::create_dir_all(root.join("ngpc")).unwrap();
        let filename = "Sonic the Hedgehog - Pocket Adventure (World).flash";
        let save = cloud_save(filename, "Sonic", "ngp", "original", ".flash");
        let target = cloud_target_path(
            &save,
            std::slice::from_ref(&root),
            &SourceKind::RetroArch,
            &EmulatorProfile::RetroArch,
            "ngp",
            Some("flash"),
        );
        assert_eq!(target, root.join("ngpc").join(filename));
    }

    fn psp_bundle_save(save_dir: &str) -> CloudSaveSummary {
        let mut save = cloud_save(
            &format!("{save_dir}.pspsave"),
            "DISSIDIA FINAL FANTASY",
            "psp",
            "original",
            ".pspsave",
        );
        save.metadata = Some(serde_json::json!({
            "rsm": {"psp": {
                "saveDir": save_dir,
                "gameId": "ULUS10437",
                "bundle": "savedata-zip",
                "format": 1,
                "files": ["DISSIDIA.BIN", "ICON0.PNG", "PARAM.SFO"]
            }}
        }));
        save
    }

    #[test]
    fn psp_restore_needs_a_savedata_bundle_record() {
        // PPSSPP reads a game's save only from SAVEDATA/<dir>/, as a folder. A
        // record holding one loose file restores none of it, whatever ids it names.
        let loose = cloud_save("DISSIDIA.BIN", "DISSIDIA", "psp", "original", ".bin");
        assert!(psp_restore_refusal(&loose, "psp").is_some());
        assert_eq!(psp_restore_refusal(&loose, "snes"), None);

        let mut game_id_only = loose.clone();
        game_id_only.metadata = Some(serde_json::json!({"rsm": {"psp": {"gameId": "ULUS10437"}}}));
        game_id_only.inspection =
            Some(serde_json::json!({"semanticFields": {"gameId": "ULUS10437"}}));
        assert!(psp_restore_refusal(&game_id_only, "psp").is_some());

        let mut dir_but_not_bundle = loose.clone();
        dir_but_not_bundle.metadata =
            Some(serde_json::json!({"rsm": {"psp": {"saveDir": "ULUS10437GameData00"}}}));
        assert!(psp_restore_refusal(&dir_but_not_bundle, "psp").is_some());

        let mut escaping_dir = psp_bundle_save("ULUS10437GameData00");
        escaping_dir.metadata = Some(serde_json::json!({
            "rsm": {"psp": {"saveDir": "../ULUS10437GameData00", "bundle": "savedata-zip"}}
        }));
        assert!(psp_restore_refusal(&escaping_dir, "psp").is_some());

        assert_eq!(
            psp_restore_refusal(&psp_bundle_save("ULUS10437GameData00"), "psp"),
            None
        );
    }

    #[test]
    fn a_psp_bundle_restores_into_the_retrodeck_ppsspp_folder() {
        // RetroDECK keeps PPSSPP's SAVEDATA at saves/PSP/PPSSPP-SA and links the
        // emulator's own directory to it.
        let save = psp_bundle_save("ULUS10437GameData00");
        let root = PathBuf::from("/run/media/deck/retrodeck/saves");
        let target = cloud_target_path(
            &save,
            std::slice::from_ref(&root),
            &SourceKind::RetroArch,
            &EmulatorProfile::RetroArch,
            "psp",
            Some("pspsave"),
        );
        assert_eq!(target, root.join("PSP/PPSSPP-SA/ULUS10437GameData00"));

        // A folder that already exists under another spelling is the one used.
        let tmp = tempfile::tempdir().unwrap();
        let saves = tmp.path().join("saves");
        fs::create_dir_all(saves.join("PSP/SAVEDATA")).unwrap();
        let target = cloud_target_path(
            &save,
            std::slice::from_ref(&saves),
            &SourceKind::SteamDeck,
            &EmulatorProfile::Generic,
            "psp",
            Some("pspsave"),
        );
        assert_eq!(target, saves.join("PSP/SAVEDATA/ULUS10437GameData00"));
    }

    fn psp_bundle_bytes(save_dir: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
        let mut entries: Vec<(String, Vec<u8>)> = vec![
            ("DISSIDIA.BIN".to_string(), vec![0x5Au8; 2048]),
            ("ICON0.PNG".to_string(), b"icon".to_vec()),
            (
                "PARAM.SFO".to_string(),
                crate::scanner::param_sfo_fixture(save_dir),
            ),
        ];
        for (name, bytes) in extra {
            entries.push(((*name).to_string(), bytes.to_vec()));
        }
        crate::scanner::build_stored_zip(&entries).unwrap()
    }

    #[test]
    fn a_psp_bundle_restore_writes_every_file_and_replaces_the_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let savedata = tmp.path().join("PSP/PPSSPP-SA");
        let target = savedata.join("ULUS10437GameData00");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("DISSIDIA.BIN"), b"old").unwrap();
        fs::write(target.join("OLD-ONLY.DAT"), b"gone after restore").unwrap();

        let written =
            restore_psp_savedata_bundle(&target, &psp_bundle_bytes("ULUS10437GameData00", &[]))
                .unwrap();
        assert_eq!(written, 3);
        assert_eq!(
            fs::read(target.join("DISSIDIA.BIN")).unwrap(),
            vec![0x5Au8; 2048]
        );
        assert_eq!(fs::read(target.join("ICON0.PNG")).unwrap(), b"icon");
        assert!(target.join("PARAM.SFO").is_file());
        assert!(
            !target.join("OLD-ONLY.DAT").exists(),
            "the old folder was kept"
        );
        // No temp folder and no copy of the old one is left beside it.
        let left: Vec<String> = fs::read_dir(&savedata)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(left, vec!["ULUS10437GameData00".to_string()]);
    }

    #[test]
    fn a_psp_bundle_restore_refuses_an_entry_that_leaves_the_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let savedata = tmp.path().join("PSP/PPSSPP-SA");
        fs::create_dir_all(&savedata).unwrap();
        let target = savedata.join("ULUS10437GameData00");

        let escaping = psp_bundle_bytes("ULUS10437GameData00", &[("../x", b"outside")]);
        assert!(restore_psp_savedata_bundle(&target, &escaping).is_err());
        assert!(!savedata.join("x").exists());
        assert!(!tmp.path().join("PSP/x").exists());
        assert!(!target.exists(), "a refused bundle wrote its folder");
        assert_eq!(
            fs::read_dir(&savedata).unwrap().count(),
            0,
            "temp folder left behind"
        );

        // PARAM.SFO has to name the folder the bundle is restored into.
        let wrong_dir = psp_bundle_bytes("ULUS10437GameData01", &[]);
        assert!(restore_psp_savedata_bundle(&target, &wrong_dir).is_err());
        assert!(!target.exists());

        // The SAVEDATA root is never created by a restore.
        let missing_root = tmp.path().join("nowhere/SAVEDATA/ULUS10437GameData00");
        assert!(
            restore_psp_savedata_bundle(
                &missing_root,
                &psp_bundle_bytes("ULUS10437GameData00", &[])
            )
            .is_err()
        );
        assert!(!tmp.path().join("nowhere").exists());
    }

    #[test]
    fn cloud_restore_puts_a_versioned_three_do_save_where_opera_reads_it() {
        // Opera loads <save dir>/opera/per_game/<game>.<version>.srm first, and
        // falls back to <save dir>/<game>.srm with no version. A
        // "<game>.0.srm" written beside the content is read by neither.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("saves");
        fs::create_dir_all(root.join("3do")).unwrap();
        let versioned = "20th Century Video Almanac (USA).0.srm";
        let save = cloud_save(versioned, "Almanac", "3do", "3do/retroarch-opera", ".srm");
        let target = cloud_target_path(
            &save,
            std::slice::from_ref(&root),
            &SourceKind::RetroArch,
            &EmulatorProfile::RetroArch,
            "3do",
            Some("srm"),
        );
        assert_eq!(
            target,
            root.join("3do")
                .join("opera")
                .join("per_game")
                .join(versioned)
        );

        // The unversioned name is the one Opera's fallback reads from the save
        // directory itself, so it stays there.
        let plain = "Star Control II (USA).srm";
        let save = cloud_save(
            plain,
            "Star Control II",
            "3do",
            "3do/retroarch-opera",
            ".srm",
        );
        let target = cloud_target_path(
            &save,
            std::slice::from_ref(&root),
            &SourceKind::RetroArch,
            &EmulatorProfile::RetroArch,
            "3do",
            Some("srm"),
        );
        assert_eq!(target, root.join("3do").join(plain));
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
    fn a_ps1_card_named_after_its_game_is_its_own_slot() {
        assert_eq!(
            infer_playstation_slot_from_path(Path::new("/saves/psx/Tekken 3 (USA).srm")),
            "Game: Tekken 3 (USA)".to_string()
        );
        assert_eq!(
            infer_playstation_slot_from_path(Path::new("/saves/PSX/Tekken 3 (USA).sav")),
            "Game: Tekken 3 (USA)".to_string()
        );
        // Numbered cards are shared and keep their number.
        assert_eq!(
            infer_playstation_slot_from_path(Path::new("/saves/psx/memory_card_1.mcr")),
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

#[cfg(test)]
mod lock_tests {
    #[test]
    fn a_lock_names_its_owner() {
        assert_eq!(super::lock_owner("pid=1234\nstarted_at=x\n"), Some(1234));
        assert_eq!(super::lock_owner("started_at=x\n"), None);
        assert_eq!(super::lock_owner("pid=abc\n"), None);
    }

    #[test]
    fn of_two_takeovers_of_one_dead_lock_exactly_one_succeeds() {
        let dir = std::env::temp_dir().join(format!("sgm-lock-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sync.lock");
        std::fs::write(&path, "pid=4194303\nstarted_at=x\n").unwrap();
        let first = super::SyncLock::take_over(&path, 4_194_303, "pid=1\n");
        let second = super::SyncLock::take_over(&path, 4_194_303, "pid=2\n");
        assert!(first.is_ok(), "the first takeover of a dead lock failed");
        assert!(
            second.is_err(),
            "a second takeover deleted the first one's live lock"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "pid=1\n",
            "the first sync's lock was replaced"
        );
        drop(first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pid_alive_tells_a_running_process_from_a_missing_one() {
        assert!(
            super::pid_alive(std::process::id()),
            "this test's own process reads as dead"
        );
        assert!(
            !super::pid_alive(4_194_303),
            "a pid naming no process reads as alive"
        );
    }
}

#[cfg(test)]
mod runtime_profile_map {
    use super::*;

    // Every system the server treats as projection-capable needs a profile on
    // the wire: the latest check, the upload and the download are all refused
    // without one ("runtimeProfile is required"), which is how game boy and DS
    // saves silently never synced. This table is the audit: a system with no
    // arm, or an arm naming a profile the server does not publish, fails here
    // rather than at a 400 on a device.
    #[test]
    fn every_supported_system_maps_to_a_published_profile() {
        let cases = [
            ("psx", EmulatorProfile::RetroArch, "psx/retroarch"),
            ("ps2", EmulatorProfile::RetroArch, "ps2/pcsx2"),
            ("n64", EmulatorProfile::Mister, "n64/mister"),
            ("snes", EmulatorProfile::RetroArch, "snes/retroarch-snes9x"),
            ("nes", EmulatorProfile::RetroArch, "nes/retroarch-nestopia"),
            (
                "gameboy",
                EmulatorProfile::RetroArch,
                "gameboy/retroarch-gambatte",
            ),
            ("gba", EmulatorProfile::RetroArch, "gba/retroarch-mgba"),
            ("nds", EmulatorProfile::RetroArch, "nds/retroarch-melonds"),
            (
                "master-system",
                EmulatorProfile::RetroArch,
                "sms/retroarch-genesis-plus-gx",
            ),
            (
                "genesis",
                EmulatorProfile::RetroArch,
                "genesis/retroarch-genesis-plus-gx",
            ),
            (
                "game-gear",
                EmulatorProfile::RetroArch,
                "gamegear/retroarch-genesis-plus-gx",
            ),
            (
                "sega-cd",
                EmulatorProfile::RetroArch,
                "sega-cd/retroarch-genesis-plus-gx",
            ),
            ("saturn", EmulatorProfile::RetroArch, "saturn/mednafen"),
            (
                "dreamcast",
                EmulatorProfile::RetroArch,
                "dreamcast/retroarch-flycast",
            ),
            (
                "pc-engine",
                EmulatorProfile::RetroArch,
                "pc-engine/retroarch-beetle-pce",
            ),
            (
                "atari-jaguar",
                EmulatorProfile::RetroArch,
                "atari-jaguar/retroarch-virtual-jaguar",
            ),
            ("3do", EmulatorProfile::RetroArch, "3do/retroarch-opera"),
        ];
        for (system, profile, expected) in cases {
            let got = projection_runtime_profile_for_system(
                &SourceKind::RetroArch,
                &profile,
                system,
                "retroarch",
            );
            assert_eq!(
                got.as_deref(),
                Some(expected),
                "{system} has no profile on the wire"
            );
        }
    }
}
