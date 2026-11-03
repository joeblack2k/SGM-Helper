use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::api::{
    ApiClient, CloudSaveSummary, ConflictCheckResponse, LatestSaveContext, LatestSaveRequest,
    LatestSaveResponse, RuntimeTarget, UploadSaveResponse,
};
use crate::backend_config::sync_config_with_backend;
use crate::config::AppConfig;
use crate::scanner::{
    RomIndexEntry, SaveAdapterProfile, SaveContainerFormat, canonical_server_system_slug,
    classify_supported_save, discover_rom_index, discover_save_files, dreamcast_skip_reason,
    encode_download_for_local_container, filename_stem, is_mame_nvram_component,
    is_n64_retroarch_bundle, mame_nvram_machine, md5_file, n64_controller_pak_entry_count,
    normalize_save_bytes_for_sync, normalize_save_for_sync, ps2_skip_reason, saturn_skip_reason,
    sha1_file, sha256_bytes, sha256_file, wii_skip_reason, wii_title_code_from_path,
};
use crate::sources::{EmulatorProfile, ResolvedSource, SourceKind, prepare_sources_for_sync};
use crate::state::{
    AuthState, RememberedBackendPolicy, SyncedEntry, TerminalRejection,
    load_remembered_backend_policy, load_sync_state, now_rfc3339, save_remembered_backend_policy,
    save_sync_state, terminal_rejection_is_current,
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
    /// An arcade download whose payload is a different size from the component
    /// file it would replace. A MAME component's size is fixed by the device it
    /// belongs to, so a mismatch means the record describes a different
    /// component or machine. The local save is kept and the refusal reported.
    pub refused_size_mismatch: usize,
    /// An arcade upload refused because every component of the machine holds
    /// the same bytes -- the shape a fan-out corruption leaves behind.
    pub refused_flattened_components: usize,
    /// The server answered 2xx but returned no save, so nothing was stored.
    pub accepted_not_stored: usize,
    /// Saves the server refused permanently. Not re-attempted until the
    /// payload changes.
    pub rejected_permanent: usize,
    /// Files that look like saves but that no system profile accepts, or that
    /// travel with a parent save rather than standing alone. Counted so a
    /// silent drop cannot happen.
    pub skipped_by_policy: usize,
    /// Saves the server already held and declined to store: settled without a
    /// write, and reported rather than retried.
    pub duplicate_ignored: usize,
    /// Duplicates settled during this pass, recorded by the caller so the next
    /// pass does not ask again.
    pub duplicate_pending: Vec<(String, String)>,
    pub errors: usize,
    /// Files the pass read and then neither decided nor counted. Every path
    /// through a save is supposed to end in a counter; one that does not is a
    /// save that can vanish while the run reports success, so the caller checks
    /// this after every file and names the file when it finds one.
    pub unaccounted: usize,
    pub dry_run: bool,
}

impl SyncReport {
    /// How many per-save decisions this report holds. Compared before and after
    /// each save to catch a pass that read a file and did nothing with it.
    fn decisions(&self) -> usize {
        self.uploaded
            + self.would_upload
            + self.downloaded
            + self.would_download
            + self.in_sync
            + self.conflicts
            + self.skipped
            + self.rejected_n64_controller_pak
            + self.refused_size_mismatch
            + self.refused_flattened_components
            + self.accepted_not_stored
            + self.rejected_permanent
            + self.skipped_by_policy
            + self.duplicate_ignored
            + self.errors
            + self.unaccounted
    }
}

/// Every upload names the file it took and the record it produced. Without
/// this, one file re-uploaded every pass cannot be told from a healthy run:
/// the only per-file lines were refusals.
/// An upload the server accepts but stores in a different form leaves the
/// state entry claiming a payload the store does not hold: the next pass sees
/// the store's form, uploads again, and the record never converges. Say so
/// the first time it happens rather than letting it read as in sync.
fn report_stored_sha(save_path: &Path, sent_sha: &str, stored_sha: Option<&str>) {
    if let Some(stored) = stored_sha
        && !stored.is_empty()
        && stored != sent_sha
    {
        eprintln!(
            "Server stored a different payload for {}: sent sha256 {}, stored {}",
            save_path.display(),
            sent_sha,
            stored
        );
    }
}

fn settle_upload(
    report: &mut SyncReport,
    path: &std::path::Path,
    bytes: usize,
    record_id: Option<&str>,
    duplicate: bool,
) {
    eprintln!(
        "Uploaded {} (record {}, {} bytes)",
        path.display(),
        record_id.unwrap_or("-"),
        bytes
    );
    if duplicate {
        // The server kept its own copy and answers the same way every pass.
        // Asking again only re-arms the pass and churns both logs.
        report.duplicate_ignored += 1;
        // Remember the file's own hash: the next pass reads the file, and an
        // adapter's canonical payload is not the bytes on disk.
        let file_sha = sha256_file(path).unwrap_or_default();
        if !file_sha.is_empty() {
            report
                .duplicate_pending
                .push((path.to_string_lossy().to_string(), file_sha));
        }
        return;
    }
    report.uploaded += 1;
}

/// Marks a summary whose counters describe what would have happened, not what did.
pub fn dry_run_marker(report: &SyncReport) -> &'static str {
    if report.dry_run { " (DRY RUN)" } else { "" }
}

struct ProcessedEntry {
    state_key: String,
    entry: SyncedEntry,
}

#[derive(Debug)]
struct SyncLock {
    path: PathBuf,
}

/// Maximum lockfile age before we treat it as stale even if we can't verify
/// PID liveness (Windows fallback, or any platform where the recorded PID
/// can't be parsed). A normal sync finishes in seconds; six hours is a wide
/// safety margin so we don't yank a long-running sync, while still recovering
/// from a stuck lockfile within the same day.
const STALE_LOCK_MAX_AGE_SECS: u64 = 6 * 60 * 60;

/// Verdict on whether an existing lockfile is "stale" (safe to take over) or
/// "active" (must bail). Carries a human-readable reason for the warning log.
#[derive(Debug, PartialEq, Eq)]
enum StaleVerdict {
    /// Recorded PID is no longer alive on this system.
    DeadPid(u32),
    /// Lockfile content couldn't be parsed as our format.
    Malformed,
    /// Lockfile is older than `STALE_LOCK_MAX_AGE_SECS` (cross-platform fallback).
    TooOld { age_secs: u64 },
    /// Lockfile looks like an active sync — caller must bail.
    Active,
}

/// Parse the `pid=<N>` line out of a lockfile body.
fn parse_lock_pid(contents: &str) -> Option<u32> {
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("pid=")
            && let Ok(pid) = rest.trim().parse::<u32>()
        {
            return Some(pid);
        }
    }
    None
}

/// Best-effort check of whether `pid` corresponds to a running process on
/// this host. The windows variant has no PID-liveness primitive wired up
/// (would require windows-sys); we conservatively return `true` and let the
/// 6h-age fallback in classify_existing_lock recover stuck lockfiles. On
/// Unix builds of this crate we use kill(pid, 0).
fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: `libc::kill` with signum 0 performs only existence-check;
        // it has no side effects on the target process.
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if r == 0 {
            return true;
        }
        let errno = std::io::Error::last_os_error().raw_os_error();
        errno != Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Decide whether an existing `sync.lock` at `path` (with the given on-disk
/// contents) should be treated as stale. See `StaleVerdict` for the cases.
fn classify_existing_lock(path: &Path, contents: &str) -> StaleVerdict {
    match parse_lock_pid(contents) {
        Some(pid) => {
            if !pid_alive(pid) {
                return StaleVerdict::DeadPid(pid);
            }
        }
        None => return StaleVerdict::Malformed,
    }

    if let Ok(meta) = fs::metadata(path)
        && let Ok(modified) = meta.modified()
        && let Ok(age) = modified.elapsed()
    {
        let age_secs = age.as_secs();
        if age_secs > STALE_LOCK_MAX_AGE_SECS {
            return StaleVerdict::TooOld { age_secs };
        }
    }

    StaleVerdict::Active
}

impl SyncLock {
    fn acquire(state_dir: &Path) -> Result<Self> {
        let path = state_dir.join("sync.lock");
        let lock_content = format!("pid={}\nstarted_at={}\n", std::process::id(), now_rfc3339());
        // Single recovery retry — see helpers/mister/src/syncer.rs for the
        // detailed write-up; same logic mirrored here so the windows helper
        // gets the same stale-lock recovery behavior.
        for attempt in 0..2 {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    file.write_all(lock_content.as_bytes()).with_context(|| {
                        format!("kan lockfile niet schrijven: {}", path.display())
                    })?;
                    return Ok(Self { path });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if attempt == 1 {
                        anyhow::bail!("sync is al actief (lockfile bestaat): {}", path.display());
                    }
                    let existing = fs::read_to_string(&path).unwrap_or_default();
                    match classify_existing_lock(&path, &existing) {
                        StaleVerdict::DeadPid(pid) => {
                            eprintln!(
                                "waarschuwing: oude sync.lock van pid={} verwijderd (proces draait niet meer): {}",
                                pid,
                                path.display()
                            );
                            let _ = fs::remove_file(&path);
                        }
                        StaleVerdict::Malformed => {
                            eprintln!(
                                "waarschuwing: oude sync.lock met onleesbare inhoud verwijderd: {}",
                                path.display()
                            );
                            let _ = fs::remove_file(&path);
                        }
                        StaleVerdict::TooOld { age_secs } => {
                            eprintln!(
                                "waarschuwing: oude sync.lock ({}s oud, drempel {}s) verwijderd: {}",
                                age_secs,
                                STALE_LOCK_MAX_AGE_SECS,
                                path.display()
                            );
                            let _ = fs::remove_file(&path);
                        }
                        StaleVerdict::Active => {
                            anyhow::bail!(
                                "sync is al actief (lockfile bestaat): {}",
                                path.display()
                            );
                        }
                    }
                }
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!("kan sync lockfile niet maken: {}", path.display())
                    });
                }
            }
        }
        unreachable!("SyncLock::acquire loop should always exit via return or bail");
    }
}

impl Drop for SyncLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Give each save root to one source.
///
/// An install can end up with the auto-created default source and an explicitly
/// configured one over the same directory -- a RetroDECK device gets both -- and
/// then every save in it is processed twice, by two sources that can disagree
/// about the profile. The configured source owns the root; an auto-created
/// default yields to it and says so. Two configured sources over one root is the
/// operator's arrangement, so both stay and the overlap is reported.
fn prefer_configured_sources(sources: Vec<ResolvedSource>) -> (Vec<ResolvedSource>, Vec<String>) {
    // The helper writes "autoscan-default" and "autoscan-known" for the sources
    // it creates; a device's auto-created default carries the latter, which the
    // first cut of this rule did not match. "manual" and "legacy-migrated" are
    // the operator's own, and a migrated source is not demoted by being old.
    fn is_auto_default(source: &ResolvedSource) -> bool {
        let origin = source.origin.to_ascii_lowercase();
        origin.starts_with("autoscan") || origin == "auto" || origin == "default"
    }

    let (defaults, configured): (Vec<ResolvedSource>, Vec<ResolvedSource>) =
        sources.into_iter().partition(is_auto_default);
    let mut notices = Vec::new();

    let mut claimed: Vec<(PathBuf, String)> = Vec::new();
    for source in &configured {
        for root in &source.save_roots {
            if let Some((_, owner)) = claimed.iter().find(|(held, _)| held == root) {
                notices.push(format!(
                    "Warning: sources '{}' and '{}' both claim {}; both run, and the order decides which handles a save",
                    owner,
                    source.name,
                    root.display()
                ));
            } else {
                claimed.push((root.clone(), source.name.clone()));
            }
        }
    }

    let mut kept = configured;
    for source in defaults {
        let shared = source.save_roots.iter().find_map(|root| {
            claimed
                .iter()
                .find(|(held, _)| held == root)
                .map(|(_, owner)| (root.clone(), owner.clone()))
        });
        match shared {
            Some((root, owner)) => notices.push(format!(
                "Source '{}' shares {} with '{}'; '{}' handles it",
                source.name,
                root.display(),
                owner,
                owner
            )),
            None => {
                for root in &source.save_roots {
                    claimed.push((root.clone(), source.name.clone()));
                }
                kept.push(source);
            }
        }
    }
    (kept, notices)
}

pub fn run_sync(
    config: &AppConfig,
    auth: Option<&AuthState>,
    options: &SyncOptions,
    verbose: bool,
) -> Result<SyncReport> {
    let state_dir = config.resolved_state_dir()?;
    crate::state::ensure_directory(&state_dir)?;
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
    let (mut sources, source_notices) = prefer_configured_sources(sources);
    for notice in &source_notices {
        eprintln!("{}", notice);
    }
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
                // A backend may force a dry run; it may not turn one the
                // operator asked for into a live sync. The flag is how an
                // operator looks at a store without changing it, and a policy
                // that wins over it makes --dry-run a request to be ignored.
                effective_options.dry_run = options.dry_run || value;
            }
            // Remember what the server last told this device. A fetch that
            // fails later must not turn a server-forced dry run into a live
            // sync: that fails open on the one setting meant to stop writes.
            let remembered = RememberedBackendPolicy {
                force_upload: Some(effective_options.force_upload),
                dry_run: Some(effective_options.dry_run),
            };
            if let Err(err) = save_remembered_backend_policy(&state_dir, &remembered)
                && verbose
            {
                eprintln!("Could not remember backend policy: {:#}", err);
            }
        }
        Err(err) => {
            if verbose {
                eprintln!("Backend config sync skipped: {:#}", err);
            }
            // Apply what the server last said, and say so once. A device that
            // has never had a policy keeps its local configuration.
            if let Some(remembered) = load_remembered_backend_policy(&state_dir) {
                if let Some(value) = remembered.force_upload {
                    effective_options.force_upload = value;
                }
                if let Some(value) = remembered.dry_run {
                    effective_options.dry_run = options.dry_run || value;
                }
                eprintln!(
                    "Backend config unavailable; applying the last policy it sent (dry_run={} force_upload={})",
                    effective_options.dry_run, effective_options.force_upload
                );
            }
        }
    }
    let options = &effective_options;

    let mut report = SyncReport {
        dry_run: options.dry_run,
        ..Default::default()
    };
    let mut rom_hash_cache: HashMap<String, (String, String)> = HashMap::new();
    let mut announced_ambiguous_stems: std::collections::HashSet<String> =
        std::collections::HashSet::new();

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

        let rom_index = discover_rom_index(&source.name, &source.rom_roots, source.recursive)?;
        // How many files share each selection key. The live-directory scan only
        // runs where it can change the answer: one copy and no declared
        // directory has nothing to decide.
        let mut stem_counts: HashMap<String, usize> = HashMap::new();
        for path in &save_files {
            *stem_counts.entry(save_selection_key(path)).or_default() += 1;
        }
        // Decided before the preferred variant is chosen: a stray that won
        // that choice would skip the live copy as its duplicate and then be
        // refused itself, and neither would upload.
        let live_blocked: HashMap<PathBuf, String> = save_files
            .iter()
            .filter_map(|path| {
                let key = save_selection_key(path);
                let needs_live_check = stem_counts.get(&key).copied().unwrap_or(0) > 1
                    || !source.save_subdirs.is_empty();
                if !needs_live_check {
                    return None;
                }
                live_upload_blocked(&source, path, &key, &rom_index)
                    .map(|reason| (path.clone(), reason))
            })
            .collect();
        let candidates: Vec<PathBuf> = save_files
            .iter()
            .filter(|path| !live_blocked.contains_key(*path))
            .cloned()
            .collect();
        let preferred_save_by_stem =
            select_preferred_save_per_stem(&source.kind, &source.profile, &candidates, &rom_index);
        report.scanned += save_files.len();

        let fingerprint = hostname::get()
            .ok()
            .and_then(|value| value.into_string().ok())
            .unwrap_or_else(|| source.kind.as_str().to_string());

        for save_path in save_files {
            // A file the scanner already found but cannot open is an error, not an
            // unsupported save. The classifier reports an unreadable file as "no
            // system", so without this a permission or mount fault looks like a save
            // the helper chose to ignore.
            if let Err(err) = std::fs::File::open(&save_path) {
                report.errors += 1;
                eprintln!("Cannot read save {}: {}", save_path.display(), err.kind());
                continue;
            }
            let selection_key = save_selection_key(&save_path);
            if let Some(reason) = live_blocked.get(&save_path) {
                report.skipped += 1;
                eprintln!("{}", reason);
                continue;
            }
            if let Some(preferred_path) = preferred_save_by_stem.get(&selection_key)
                && preferred_path != &save_path
            {
                report.skipped += 1;
                // Named every time, not only under --verbose: a file the tool read
                // and then decided not to upload is exactly what a silent drop looks
                // like from the outside, whatever the reason was.
                eprintln!(
                    "Skipping duplicate save variant {} in favor of preferred {}",
                    save_path.display(),
                    preferred_path.display()
                );
                continue;
            }

            let save_key = save_path.to_string_lossy().to_string();
            let prior_rejection = sync_state
                .terminal_rejections
                .get(&save_key)
                .cloned()
                .filter(|prior| {
                    sha256_file(&save_path)
                        .map(|current| current == prior.sha256)
                        .unwrap_or(false)
                });
            // A marker recorded for a routing failure says nothing about the save, so
            // drop it and process the save again.  Devices that ran the older build
            // carry those markers in their state already.
            if let Some(prior) = prior_rejection {
                if !terminal_rejection_is_current(&prior) {
                    // The refusal was recorded by another release, or long enough
                    // ago that the server may have changed under it. Asking again
                    // costs one request; not asking is how a fixed server stayed
                    // "refusing" for two sessions.
                    eprintln!(
                        "Re-probing {}: the refusal recorded {} by helper {} is no longer current",
                        save_path.display(),
                        prior.recorded_at,
                        prior
                            .helper_version
                            .as_deref()
                            .unwrap_or("before versioning")
                    );
                    sync_state.terminal_rejections.remove(&save_key);
                } else if carries_save_scoped_refusal(&prior.reason) {
                    report.rejected_permanent += 1;
                    eprintln!(
                        "Not retrying {}: the server refused it with {} on {}{}",
                        save_path.display(),
                        prior.status,
                        prior.recorded_at,
                        if prior.reason.is_empty() {
                            String::new()
                        } else {
                            format!(": {}", prior.reason)
                        }
                    );
                    continue;
                } else {
                    sync_state.terminal_rejections.remove(&save_key);
                }
            }

            if let Some(prior_sha) = sync_state.duplicate_uploads.get(&save_key).cloned() {
                if sha256_file(&save_path)
                    .map(|current| current == prior_sha)
                    .unwrap_or(false)
                {
                    // The server kept its own copy of exactly these bytes and
                    // answers the same way every pass. Asking again only
                    // churns both logs; a changed payload is uploaded.
                    report.duplicate_ignored += 1;
                    // Named every run, not only under --verbose: this skip persists until
                    // the file changes, and a save that is never uploaded, never rejected
                    // and never mentioned reads as one the helper does not see.
                    eprintln!(
                        "Not uploading {}: the server already holds these bytes",
                        save_path.display()
                    );
                    continue;
                }
                sync_state.duplicate_uploads.remove(&save_key);
            }

            let decisions_before = report.decisions();
            let process_result = process_single_save(
                &api,
                &save_path,
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
                &rom_index,
                &mut rom_hash_cache,
                &sync_state.entries,
                app_password,
                options,
                &mut report,
                &mut announced_ambiguous_stems,
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
                    // A save that is read and then neither decided nor counted is a save
                    // that can vanish while the pass reports success. Nothing in this
                    // function is meant to end that way; if something does, name the
                    // file rather than trust the code.
                    if report.decisions() == decisions_before {
                        report.unaccounted += 1;
                        eprintln!(
                            "Unaccounted save: {} was read and produced no decision (this is a client bug)",
                            save_path.display()
                        );
                    }
                }
                Err(err) => {
                    // A refusal of this save is the save's outcome, not a failed pass.
                    // Counting it as an error made a first run report errors=3 and, in
                    // the service, look like a fault to retry. It is held for as long
                    // as the file is unchanged.
                    // Only a refusal of the upload is a refusal of the save. A lookup
                    // or a conflict check the server refuses says nothing about whether
                    // the save would be accepted, and recording it permanently froze
                    // saves whose declared profile belonged to another system until the
                    // marker expired.
                    let refused_upload = refusal_is_about_the_save(&err);
                    if let Some(status) =
                        permanent_rejection_status(&err).filter(|_| refused_upload)
                    {
                        report.rejected_permanent += 1;
                        eprintln!(
                            "Refused {} (status {}): {:#}",
                            save_path.display(),
                            status,
                            err
                        );
                        let sha = sha256_file(&save_path).unwrap_or_default();
                        sync_state.terminal_rejections.insert(
                            save_key.clone(),
                            TerminalRejection {
                                sha256: sha,
                                status,
                                reason: format!("{err:#}"),
                                recorded_at: now_rfc3339(),
                                helper_version: Some(crate::state::build_identity().to_string()),
                            },
                        );
                    } else {
                        report.errors += 1;
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
            &rom_index,
            source.create_missing_system_dirs,
            &fingerprint,
            app_password,
            options,
            &mut sync_state.entries,
            &mut report,
            verbose,
        )?;
    }

    if !report.duplicate_pending.is_empty() {
        for (path, sha) in report.duplicate_pending.drain(..) {
            sync_state.duplicate_uploads.insert(path, sha);
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

/// A 4xx says the request itself was refused, so retrying identical bytes
/// cannot succeed. 408 and 429 are the exceptions: a timeout or a rate limit
/// means "later", not "no".
fn permanent_rejection_status(err: &anyhow::Error) -> Option<u16> {
    let message = format!("{err:#}");
    let lowered = message.to_ascii_lowercase();
    let start = lowered.find("status=")? + "status=".len();
    let code: u16 = lowered[start..].split_whitespace().next()?.parse().ok()?;
    // A 404 is a route or a proxy; 401/403 an identity; 408/429 a transient
    // condition. None of them say anything about this save, and recording one
    // as terminal takes the save out of sync until its bytes change.
    if !matches!(code, 400 | 409 | 413 | 415 | 422) {
        return None;
    }
    if !carries_save_scoped_refusal(&message) {
        return None;
    }
    Some(code)
}

/// Whether a failed request carries the server's refusal of *this save*.
///
/// Only a structured body names a save-level reason.  A routing or transport
/// failure -- a proxy's 404, the bare "404 page not found" a Go mux returns for
/// an unrouted path, an empty 5xx -- says nothing about the save, and treating
/// one as terminal marks every save in a pass as permanently refused the first
/// time the server or a route in front of it is misconfigured.
/// Whether a refusal is about the save itself. Only an error from the upload
/// request qualifies; a lookup or conflict check the server refuses says
/// nothing about whether the save would be accepted. The type says so, not the
/// message -- rewording what the operator reads cannot change the decision.
fn refusal_is_about_the_save(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<crate::api::UploadRequestError>()
            .is_some()
    })
}

fn carries_save_scoped_refusal(message: &str) -> bool {
    let Some((_, body)) = message.split_once("body=") else {
        return false;
    };
    // The body is truncated when recorded, so a long one fails to parse; that
    // leaves the save retryable, which is the safe direction.
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body.trim()) else {
        return false;
    };
    ["reason", "message", "errorMessage", "error"]
        .iter()
        .any(|key| {
            value
                .get(key)
                .and_then(|entry| entry.as_str())
                .is_some_and(|entry| !entry.trim().is_empty())
        })
}

fn source_allows_system(source_systems: &[String], system_slug: &str) -> bool {
    // Both sides are compared as the server names them: a source's SYSTEMS
    // list comes from the server, and `tgfx16` there would be `pc-engine`.
    let clean = canonical_server_system_slug(system_slug);
    if clean.is_empty() {
        return false;
    }
    source_systems.iter().any(|system| {
        system == "*" || canonical_server_system_slug(system).eq_ignore_ascii_case(&clean)
    })
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
    rom_index: &HashMap<String, RomIndexEntry>,
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
                "Cloud restore scan for source '{}' saw {} save track(s) in the account                  (this source's own files are counted above, not here)",
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
                rom_index,
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
    rom_index: &HashMap<String, RomIndexEntry>,
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
                "Skipping cloud {} save '{}': {} is not in this source's SYSTEMS list",
                system_slug,
                cloud_save.display_name(),
                source_name
            );
        }
        return Ok(());
    }

    let effective_profile = effective_profile_for_cloud_restore(source_kind, source_profile);
    let device_type = helper_device_type_for_upload(source_kind, &effective_profile, &system_slug);
    let runtime_target = runtime_target_needs_dsv_profile(
        &system_slug,
        &cloud_restore_filename(cloud_save, None),
        cloud_save.latest_size_bytes.or(cloud_save.file_size),
        runtime_target_for_system(source_kind, &effective_profile, &system_slug, device_type),
    );
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
        rom_index,
    ) else {
        // Reported by the resolver: the title sits in more than one directory
        // and nothing says which one the emulator reads.
        report.skipped += 1;
        return Ok(());
    };

    if existing_local_save_is_valid(&provisional_path, &system_slug) {
        if verbose {
            eprintln!(
                "Cloud restore for {}: {} already holds this save",
                system_slug,
                provisional_path.display()
            );
        }
        return Ok(());
    }

    let native_target_path = restore_target_without_overwriting_another_system(
        cloud_restore_native_target_path(
            &provisional_path,
            source_kind,
            &effective_profile,
            &system_slug,
            cloud_save,
        ),
        &provisional_path,
        &system_slug,
        sync_entries,
    );
    if native_target_path != provisional_path
        && existing_local_save_is_valid(&native_target_path, &system_slug)
    {
        if verbose {
            eprintln!(
                "Cloud restore for {}: {} already holds this save",
                system_slug,
                native_target_path.display()
            );
        }
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
        if verbose {
            eprintln!(
                "Cloud restore for {}: {} was already restored in this pass",
                system_slug,
                provisional_path.display()
            );
        }
        return Ok(());
    }
    let native_target_key = native_target_path.to_string_lossy().to_string();
    if native_target_key != provisional_key && restored_targets.contains(&native_target_key) {
        if verbose {
            eprintln!(
                "Cloud restore for {}: {} was already restored in this pass",
                system_slug,
                native_target_path.display()
            );
        }
        return Ok(());
    }

    if options.dry_run {
        report.would_download += 1;
        if verbose {
            eprintln!("Would download {} (dry run)", cloud_save.display_name());
        }
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
    let target_path = restore_target_without_overwriting_another_system(
        preferred_save_path(
            &provisional_path,
            source_kind,
            &effective_profile,
            Some(&system_slug),
            local_container,
            Some(downloaded_bytes.len() as u64),
        ),
        &provisional_path,
        &system_slug,
        sync_entries,
    );

    if existing_local_save_is_valid(&target_path, &system_slug) {
        if verbose {
            eprintln!(
                "Cloud restore for {}: {} already holds this save",
                system_slug,
                target_path.display()
            );
        }
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
        if verbose {
            eprintln!(
                "Cloud restore for {}: {} was already restored in this pass",
                system_slug,
                target_path.display()
            );
        }
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

    report_restore_write(
        &system_slug,
        cloud_save.display_name(),
        &target_path,
        local_bytes.len(),
        runtime_profile,
    );

    Ok(())
}

/// Say that a save was written to disk.
///
/// Every restore path prints this, on every run: a restore that writes a file
/// and says nothing is indistinguishable from one that never ran, which is how
/// an N64 bundle coming back correctly was reported as "prints nothing".
fn report_restore_write(
    system_slug: &str,
    title: &str,
    path: &Path,
    bytes: usize,
    profile: Option<&str>,
) {
    eprintln!(
        "Restored {} save {:?} to {} ({} bytes, profile {})",
        system_slug,
        title,
        path.display(),
        bytes,
        profile.unwrap_or("original")
    );
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
    // Under `n64/retroarch` the core reads one combined bundle, and the record's
    // own filename may still name a component it was uploaded as. A component
    // extension is not a file that core loads, so the target is the profile's
    // `.srm`; the alternative is writing a 296,960-byte bundle over a 2 KiB
    // eeprom, which the size guard refuses every pass.
    if let Some(target) = n64_bundle_target(
        &cloud_save.system_slug,
        runtime_profile,
        extension_from_filename(&cloud_save.filename).as_deref(),
    ) {
        return Some(target.to_string());
    }
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
        &HashMap::new(),
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
    rom_index: &HashMap<String, RomIndexEntry>,
) -> Option<PathBuf> {
    let root = select_cloud_restore_root(save_roots);
    let filename = cloud_restore_filename(cloud_save, target_extension);
    let selection_key = save_selection_key(Path::new(filename.trim()));
    let declared = declared_live_subdir(
        rom_index,
        save_subdirs,
        system_slug,
        &filename_stem(Path::new(filename.trim())),
    );
    match live_target_for(root, system_slug, &selection_key, declared.as_deref()) {
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

/// The operator's `SAVE_SUBDIR_<system>` for one title, with `{content}`
/// replaced by the name of the directory the title's ROM is in. RetroArch
/// sorting saves by content directory files a Game Boy Color game's save under
/// `gbc` and a Game Boy game's under `gb`, though the server calls both
/// `gameboy`; on a device launched from frontend platform folders the same
/// option gives `Super Nintendo Entertainment System/Snes9x`. A `{content}`
/// value whose ROM isn't found, or whose stem is under two systems, declares
/// nothing, and the copies on disk decide as before.
fn declared_live_subdir(
    rom_index: &HashMap<String, RomIndexEntry>,
    save_subdirs: &BTreeMap<String, PathBuf>,
    system_slug: &str,
    stem: &str,
) -> Option<PathBuf> {
    let declared = save_subdirs
        .get(system_slug)?
        .to_string_lossy()
        .into_owned();
    if !declared.contains("{content}") {
        return Some(PathBuf::from(declared));
    }
    let entry = rom_index.get(&stem.to_ascii_lowercase())?;
    if entry.ambiguous {
        return None;
    }
    let content = entry.path.parent()?.file_name()?.to_str()?;
    Some(PathBuf::from(declared.replace("{content}", content)))
}

/// Resolve one title's live directory under a source root.
///
/// Depth is the root itself plus one level below it, stated rather than
/// assumed: a layout nested deeper is not found here and the caller's
/// disagreement line is what makes that visible. A directory belongs to a
/// system by classifying the saves inside it, not by its name, so a core
/// nobody has seen yet still works.
fn live_target_for(
    root: &Path,
    system_slug: &str,
    selection_key: &str,
    declared: Option<&Path>,
) -> LiveTarget {
    let mut copies: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        // The root itself is a candidate: a save can sit directly in it with no
        // directory at all, and leaving it out made those files invisible --
        // not followed, not reported, never synced.
        let mut directories: Vec<PathBuf> = vec![root.to_path_buf()];
        directories.extend(
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir()),
        );
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
        let directory = root.join(alias);
        let candidate = directory.join(filename);
        if candidate.exists() {
            return Some(candidate);
        }
        // The profile's extension is the server's idea of the name, and the
        // core may have written another one for the same save: a MiSTer pads a
        // Master System save to 64 KiB and keeps it as `.sav`, while the Deck
        // keeps the cart's 8188-byte `.srm`. A same-stem file counts only when
        // the state records this helper syncing it for this system -- the same
        // gate the write-back uses, so a sibling system's file is never written
        // over.
        let mut matches = same_stem_known_saves(&directory, filename, system_slug, known_saves);
        match matches.len() {
            0 => {}
            1 => {
                let existing = matches.remove(0);
                eprintln!(
                    "Restoring into {}: it already holds this {} save under another extension",
                    existing.display(),
                    system_slug
                );
                return Some(existing);
            }
            _ => {
                eprintln!(
                    "More than one file in {} holds the {} stem: {}; using a new name instead",
                    directory.display(),
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
        let matches = same_stem_known_saves(path, filename, system_slug, known_saves);
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

/// The profile's preferred extension is a name, not evidence: rewriting `.sav`
/// to `.srm` lands on whatever file already carries that name. Only a file this
/// helper synced for this system may be written blind; otherwise the record's
/// own name is kept and the other file is left alone.
fn restore_target_without_overwriting_another_system(
    candidate: PathBuf,
    provisional: &Path,
    system_slug: &str,
    known_saves: &HashMap<String, SyncedEntry>,
) -> PathBuf {
    if candidate != provisional
        && candidate.exists()
        && !known_save_for_system(&candidate, system_slug, known_saves)
    {
        eprintln!(
            "Not restoring into {}: the profile's extension names a file this helper has not synced as {}",
            candidate.display(),
            system_slug
        );
        return provisional.to_path_buf();
    }
    candidate
}

/// N64 saves come in two shapes: the combined bundle that the retroarch core
/// reads, and the components a cartridge carries (eeprom, SRAM, flash, and the
/// controller pak). One is not the other, so a bundle target must never adopt a
/// component file, whatever the record's own filename says.
fn n64_shapes_differ(candidate: &str, target: &str) -> bool {
    fn shape(name: &str) -> u8 {
        let ext = Path::new(name)
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase());
        match ext.as_deref() {
            Some("srm") => 1,
            Some("eep" | "sra" | "fla" | "mpk" | "cpk") => 2,
            _ => 0,
        }
    }
    shape(candidate) != shape(target)
}

/// Whether a same-stem file may stand in for `target` on this system.
///
/// Cross-extension is the normal case: a `.sav` and a `.srm` are one save in
/// two spellings. N64 is the exception -- its bundle and its components are
/// different files, not different spellings of one.
fn shape_compatible(system_slug: &str, candidate: &str, target: &str) -> bool {
    if system_slug == "n64" {
        return !n64_shapes_differ(candidate, target);
    }
    true
}

/// Files in `directory` that share `filename`'s stem and that the state records
/// as this system's. Sorted, so a caller that refuses on ambiguity names them in
/// a stable order.
fn same_stem_known_saves(
    directory: &Path,
    filename: &str,
    system_slug: &str,
    known_saves: &HashMap<String, SyncedEntry>,
) -> Vec<PathBuf> {
    let mut matches: Vec<PathBuf> = std::fs::read_dir(directory)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|entry| entry.is_file())
                .filter(|entry| {
                    entry
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(|name| {
                            same_save_stem(name, filename)
                                && shape_compatible(system_slug, name, filename)
                        })
                        .unwrap_or(false)
                })
                .filter(|entry| known_save_for_system(entry, system_slug, known_saves))
                .collect()
        })
        .unwrap_or_default();
    matches.sort();
    matches
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
    prior_local_sha: Option<&str>,
    fingerprint: &str,
    source_name: &str,
    source_kind: &SourceKind,
    source_profile: &EmulatorProfile,
    source_systems: &[String],
    rom_index: &HashMap<String, RomIndexEntry>,
    rom_hash_cache: &mut HashMap<String, (String, String)>,
    known_saves: &HashMap<String, SyncedEntry>,
    app_password: Option<&str>,
    options: &SyncOptions,
    report: &mut SyncReport,
    announced_ambiguous_stems: &mut std::collections::HashSet<String>,
    verbose: bool,
) -> Result<Option<ProcessedEntry>> {
    let stem = filename_stem(save_path);
    let stem_key = stem.to_ascii_lowercase();
    let rom_entry = rom_index.get(&stem_key);
    // A stem the index holds under more than one system says nothing about this
    // save: the path is one candidate among several, and letting it decide is
    // how a NES save was classified genesis because the walk reached the
    // Mega Drive ROM first. The save's own directory gets the say instead.
    let rom_hint = rom_entry
        .filter(|entry| !entry.ambiguous)
        .map(|entry| entry.path.as_path());
    let mut state_key = save_key.to_string();
    let ambiguous_systems = rom_entry
        .filter(|entry| entry.ambiguous)
        .map(|entry| entry.systems.join(", "));
    let Some(classification) = classify_supported_save(save_path, rom_hint) else {
        report.skipped += 1;
        if let Some(reason) = dreamcast_skip_reason(save_path, rom_hint) {
            eprintln!(
                "Skipping Dreamcast save {}: {}",
                save_path.display(),
                reason
            );
        } else if let Some(reason) = saturn_skip_reason(save_path, rom_hint) {
            eprintln!("Skipping Saturn save {}: {}", save_path.display(), reason);
        } else if let Some(reason) = ps2_skip_reason(save_path, rom_hint) {
            report.skipped_by_policy += 1;
            eprintln!("Skipping PS2 save {}: {}", save_path.display(), reason);
        } else if let Some(reason) = wii_skip_reason(save_path) {
            eprintln!("Skipping Wii save {}: {}", save_path.display(), reason);
        } else if matches!(
            save_extension(save_path).as_deref(),
            Some("rtc") | Some("smpc")
        ) {
            report.skipped_by_policy += 1;
            eprintln!(
                "Skipping {}: a clock sidecar travels with its save rather than syncing on its own",
                save_path.display()
            );
        } else if rom_entry.map(|entry| entry.ambiguous).unwrap_or(false) {
            report.skipped_by_policy += 1;
            eprintln!(
                "Skipping save {}: the ROM index holds this stem under more than one system and the save's own directory names none of them",
                save_path.display()
            );
        } else if let Some(ext) = save_extension(save_path)
            && crate::scanner::known_save_extensions().contains(ext.as_str())
        {
            // It carries a save extension and a save-shaped name, and no
            // system profile accepted it. Dropping it quietly is how two real
            // Sega saves went missing; name it and count it.
            report.skipped_by_policy += 1;
            eprintln!(
                "Skipping save {}: {} ({} bytes, .{})",
                save_path.display(),
                crate::scanner::save_gate_rejection(save_path)
                    .unwrap_or("no system profile accepts it"),
                std::fs::metadata(save_path)
                    .map(|meta| meta.len())
                    .unwrap_or_default(),
                ext
            );
        } else if verbose {
            eprintln!(
                "Skipping non-supported save (outside allowed console families): {}",
                save_path.display()
            );
        }
        return Ok(None);
    };
    let system_slug = classification.system_slug;
    // A MAME nvram component is named after its slot inside one machine --
    // eeprom, game, bmcu_mcu -- and every machine carries files with those
    // names. The machine names the romset, so the rom lookup uses it rather
    // than the file stem, and the upload says which machine the save belongs
    // to.
    let arcade_machine = if system_slug == "arcade" {
        mame_nvram_machine(save_path)
    } else {
        None
    };
    let rom_stem = arcade_machine.clone().unwrap_or_else(|| stem.clone());
    let rom_stem_key = rom_stem.to_ascii_lowercase();
    let rom_entry_for_rom = arcade_machine
        .as_ref()
        .and_then(|machine| rom_index.get(&machine.to_ascii_lowercase()))
        .or(rom_entry);
    let classification_evidence = classification.evidence;
    if let Some(systems) = ambiguous_systems.as_deref()
        && announced_ambiguous_stems.insert(stem.clone())
    {
        eprintln!(
            "Stem {:?} is held by {} in the ROM index; {} decided from its save directory",
            stem,
            systems,
            canonical_server_system_slug(&system_slug)
        );
    }
    if verbose && rom_entry_for_rom.is_none() {
        // Naming the fallback matters: the same save classifies differently
        // depending on whether the ROM root resolved, and an unnamed fallback
        // reads as a flapping bug rather than a missing precondition.
        eprintln!(
            "No ROM for {:?} in the index; {} classified from its save path",
            rom_stem, system_slug
        );
    }
    if !source_allows_system(source_systems, &system_slug) {
        report.skipped += 1;
        if verbose {
            eprintln!(
                "Skipping {}: {} is not in this source's SYSTEMS list ('{}')",
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
    // The server refuses a controller pak that holds no notes.  Catching it
    // here keeps the four blank templates a MiSTer writes from being uploaded
    // on every pass, where they arrived as a rejection each and buried every
    // other error in the server log.
    if is_empty_n64_controller_pak(save_path, &normalized_save.canonical_bytes, &system_slug) {
        report.rejected_n64_controller_pak += 1;
        eprintln!(
            "Skipping empty N64 controller pak {}: no save entries",
            save_path.display()
        );
        return Ok(None);
    }
    if system_slug == "arcade"
        && arcade_machine_components_are_identical(save_path, &normalized_save.canonical_bytes)
    {
        // Every component of a machine holding the same bytes is what a
        // fan-out corruption leaves behind. Uploading it would publish the
        // damage to every device that restores the machine.
        report.refused_flattened_components += 1;
        eprintln!(
            "Refusing to upload {}: every component of this machine holds the same {} bytes, which is a corruption signal, not a save",
            save_path.display(),
            normalized_save.canonical_bytes.len()
        );
        return Ok(None);
    }

    // The RTC sidecar carries a clock, not a save. Stored alone it becomes a
    // record that re-uploads and re-downloads every pass, and written to a
    // sibling name it destroys the save beside it.
    if matches!(
        save_extension(save_path).as_deref(),
        Some("rtc") | Some("smpc")
    ) {
        report.skipped_by_policy += 1;
        eprintln!(
            "Skipping {}: a clock sidecar travels with its save rather than syncing on its own",
            save_path.display()
        );
        return Ok(None);
    }

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
    if n64_component_beside_bundle(save_path, &system_slug, &effective_profile) {
        report.skipped += 1;
        eprintln!(
            "Skipping {}: a component beside the {} bundle is not this profile's save",
            save_path.display(),
            save_path.with_extension("srm").display()
        );
        return Ok(None);
    }
    let effective_slot_name =
        resolve_slot_name_for_sync(&system_slug, save_path, &options.slot_name);
    // A save's identity is its content and its game, not the name the emulator
    // chose. RetroDECK and RetroArch write one game's save under both
    // "Madden NFL 94" and "Madden NFL '94 (USA, Europe)"; uploading the second
    // name creates a second record for one game, and on the authenticated path
    // the server refuses it as a new game at an unlisted size -- which reads as
    // data loss when nothing was lost. Arcade is exempt: its components are
    // named after their device and identical bytes there are a corruption
    // signal, not a rename.
    if system_slug != "arcade"
        && let Some((prior_path, prior_entry)) = save_already_synced_under_another_name(
            known_saves,
            save_path,
            &system_slug,
            &effective_slot_name,
            &local_sha,
        )
    {
        report.duplicate_ignored += 1;
        eprintln!(
            "Not uploading {}: it is the same save as {} under another name (sha256 {})",
            save_path.display(),
            prior_path,
            local_sha
        );
        return Ok(Some(processed_entry(
            state_key.clone(),
            synced_entry(
                local_sha,
                prior_entry.rom_sha1.clone(),
                prior_entry.version,
                Some(&system_slug),
                Some(normalized_save.local_container),
                Some(normalized_save.adapter_profile),
                Some(source_kind),
                Some(source_name),
                Some(&effective_slot_name),
            ),
        )));
    }

    let device_type = helper_device_type_for_upload(source_kind, &effective_profile, &system_slug);
    let mut runtime_target =
        runtime_target_for_system(source_kind, &effective_profile, &system_slug, device_type);
    // A combined N64 save is RetroArch's own form whatever the source profile
    // says: mupen64plus-next writes one file holding EEPROM, pak templates,
    // SRAM and FlashRAM, and the server splits it only for that profile. The
    // device profile can be Generic (a SteamDeck) or MupenFamily, neither of
    // which accepts the bundle, so the file itself decides.
    if system_slug == "n64" && is_n64_retroarch_bundle(save_path) {
        runtime_target = runtime_target_for_profile(&system_slug, "n64/retroarch");
    }
    // A .dsv is DeSmuME's container. The server strips its footer only for
    // that profile, so the container decides the profile the same way the N64
    // bundle does -- otherwise the file is measured with the footer and
    // refused as an unrecognised size.
    if let Some(filename) = save_path.file_name().and_then(|value| value.to_str()) {
        runtime_target = runtime_target_needs_dsv_profile(
            &system_slug,
            filename,
            Some(normalized_save.canonical_bytes.len() as u64),
            runtime_target,
        );
    }

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
        let lookup = api.lookup_rom(&rom_stem).ok();
        rom_sha1 = lookup
            .as_ref()
            .and_then(|value| value.rom.as_ref())
            .and_then(|value| value.sha1.clone());
        rom_md5 = lookup
            .as_ref()
            .and_then(|value| value.rom.as_ref())
            .and_then(|value| value.md5.clone());

        // The lookup endpoint derives its sha1 from the filename stem and never
        // names a system, so "the record carries no system" says nothing about
        // whether the game is known -- warning on it fired for every save and
        // buried the cases that matter. What is worth naming is an answer with
        // no game at all, which is a response this helper cannot use.
        let lookup_game = lookup
            .as_ref()
            .and_then(|value| value.rom.as_ref())
            .and_then(|rom| rom.game.as_ref());
        if verbose && lookup.is_some() && lookup_game.is_none() {
            eprintln!(
                "The server returned no game for {:?}; the save is keyed on what this helper hashes itself",
                rom_stem
            );
        }

        if rom_sha1.is_none()
            && let Some(rom_entry) = rom_entry_for_rom
        {
            if let Some((cached_sha1, cached_md5)) = rom_hash_cache.get(&rom_stem_key).cloned() {
                rom_sha1 = Some(cached_sha1);
                rom_md5 = Some(cached_md5);
            } else {
                let local_rom_sha1 = sha1_file(&rom_entry.path)?;
                let local_rom_md5 = md5_file(&rom_entry.path)?;
                rom_hash_cache.insert(
                    rom_stem_key.clone(),
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
            eprintln!("No ROM mapping found for save {}", rom_stem);
        }
        return Ok(None);
    };

    if verbose {
        // Which ROM the save was mapped to, and what was hashed for it. The
        // server treats a missing rom_sha1 as a different upload shape, so a
        // derivation that silently yields nothing changes server behaviour.
        eprintln!(
            "ROM mapping for {}: stem {:?}, index entry {}, rom_sha1 {}",
            save_path.display(),
            rom_stem,
            if rom_entry_for_rom.is_some() {
                "found"
            } else {
                "none"
            },
            match &active_rom_sha1 {
                value if value.is_empty() => "EMPTY".to_string(),
                value => value.clone(),
            }
        );
    }

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
            report.would_upload += 1;
            if verbose {
                eprintln!("Would upload {} (dry run)", save_path.display());
            }
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
            arcade_machine.as_deref(),
            &runtime_target,
            verbose,
        );
        let upload_id: Option<String>;
        let duplicate_uploaded: bool;
        let upload_version = match upload_result {
            Ok(ref response) => match response
                .save
                .as_ref()
                .filter(|save| save.id.is_some() || save.sha256.is_some())
            {
                Some(save) => {
                    duplicate_uploaded = response.declined_duplicate();
                    report_stored_sha(save_path, &local_sha, save.sha256.as_deref());
                    upload_id = save.id.clone();
                    save.version
                }
                None => {
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
            },
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

        settle_upload(
            report,
            save_path,
            normalized_save.canonical_bytes.len(),
            upload_id.as_deref(),
            duplicate_uploaded,
        );
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

    if options.force_upload {
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
            arcade_machine.as_deref(),
            &runtime_target,
            verbose,
        );
        let upload_id: Option<String>;
        let duplicate_uploaded: bool;
        let upload_version = match upload_result {
            Ok(ref response) => match response
                .save
                .as_ref()
                .filter(|save| save.id.is_some() || save.sha256.is_some())
            {
                Some(save) => {
                    duplicate_uploaded = response.declined_duplicate();
                    report_stored_sha(save_path, &local_sha, save.sha256.as_deref());
                    upload_id = save.id.clone();
                    save.version
                }
                None => {
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
            },
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
        settle_upload(
            report,
            save_path,
            normalized_save.canonical_bytes.len(),
            upload_id.as_deref(),
            duplicate_uploaded,
        );
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
        if verbose {
            eprintln!(
                "In sync with the server: {} (version {})",
                save_path.display(),
                latest.version.unwrap_or_default()
            );
        }
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
            let response = api.upload_save(
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
                arcade_machine.as_deref(),
                Some(&runtime_target),
            )?;
            report_stored_sha(
                save_path,
                &local_sha,
                response
                    .save
                    .as_ref()
                    .and_then(|save| save.sha256.as_deref()),
            );
            settle_upload(
                report,
                save_path,
                normalized_save.canonical_bytes.len(),
                None,
                response.declined_duplicate(),
            );
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
                let response = api.upload_save(
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
                    arcade_machine.as_deref(),
                    Some(&runtime_target),
                )?;
                report_stored_sha(
                    save_path,
                    &local_sha,
                    response
                        .save
                        .as_ref()
                        .and_then(|save| save.sha256.as_deref()),
                );
                settle_upload(
                    report,
                    save_path,
                    normalized_save.canonical_bytes.len(),
                    None,
                    response.declined_duplicate(),
                );
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

    // The local bytes differ from the record. If this device had the record's
    // copy at the last sync, the difference is a local edit and belongs on the
    // server as a new version -- downloading over it would destroy the only
    // copy of that edit. Without a last-sync record the helper cannot tell
    // which side moved, and a device with no history is the restore case: the
    // cloud copy is written, guarded by the size check.
    let local_edited_since_sync = prior_local_sha
        .map(|prior| prior != local_sha.as_str())
        .unwrap_or(false);
    if local_edited_since_sync {
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
        let response = api.upload_save(
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
            arcade_machine.as_deref(),
            Some(&runtime_target),
        )?;
        report_stored_sha(
            save_path,
            &local_sha,
            response
                .save
                .as_ref()
                .and_then(|save| save.sha256.as_deref()),
        );
        settle_upload(
            report,
            save_path,
            normalized_save.canonical_bytes.len(),
            None,
            response.declined_duplicate(),
        );
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
                let response = api.upload_save(
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
                    arcade_machine.as_deref(),
                    Some(&runtime_target),
                )?;
                report_stored_sha(
                    save_path,
                    &local_sha,
                    response
                        .save
                        .as_ref()
                        .and_then(|save| save.sha256.as_deref()),
                );
                settle_upload(
                    report,
                    save_path,
                    normalized_save.canonical_bytes.len(),
                    None,
                    response.declined_duplicate(),
                );
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
        if target_save_path == save_path
            && let Ok(existing) = std::fs::metadata(&target_save_path)
            && existing.is_file()
            && existing.len() != local_bytes.len() as u64
        {
            // Padding is not a difference: the same save written by a core that
            // pads and one that does not must not deadlock two devices. Read the
            // file only on the mismatch, and only to compare the bytes that
            // carry the save.
            if let Ok(on_disk) = std::fs::read(&target_save_path) {
                let padded = if on_disk.len() >= local_bytes.len() {
                    same_save_with_padding(&local_bytes, &on_disk)
                } else {
                    same_save_with_padding(&on_disk, &local_bytes)
                };
                if padded {
                    report.in_sync += 1;
                    eprintln!(
                        "Keeping {} ({} bytes): record {} is the same save at {} bytes, the difference is 0xFF padding",
                        target_save_path.display(),
                        on_disk.len(),
                        save_id,
                        local_bytes.len()
                    );
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
            }
            // A component's size is a property of its device. A payload of a
            // different size is a different component (or another machine's
            // save), and writing it would replace real data with the wrong
            // save at the wrong length.
            report.refused_size_mismatch += 1;
            eprintln!(
                "Refusing to write record {} ({} bytes) over {} ({} bytes): a save keeps its size, so this payload is a different save or a canonical form the game never wrote",
                save_id,
                local_bytes.len(),
                target_save_path.display(),
                existing.len()
            );
            return Ok(None);
        }
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
        report_restore_write(
            &system_slug,
            &filename_stem(save_path),
            &target_save_path,
            local_bytes.len(),
            runtime_target
                .system_profile_value
                .as_deref()
                .or(runtime_target.runtime_profile.as_deref()),
        );
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
                "Downloaded canonical save for {} and wrote local container {} (record {}, {} bytes)",
                target_save_path.display(),
                normalized_save.local_container.as_str(),
                save_id,
                local_bytes.len()
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
/// The target extension a bundle-profile source gives an N64 save.
///
/// `n64/retroarch` loads one combined bundle per game, and the record or the
/// state can still name a component the core never reads (`.eep`, `.sra`,
/// `.fla`, and the controller pak containers). Both restore paths ask this one
/// function, so they cannot disagree about what the target is.
/// Under a bundle profile the core reads one combined file per game, so a
/// component beside it is a leftover of an older restore. The server's payload
/// for that game arrives as the bundle, the size guard refuses it over the
/// component, and the refusal repeats every pass while nothing uploads.
fn n64_component_beside_bundle(
    save_path: &Path,
    system_slug: &str,
    profile: &EmulatorProfile,
) -> bool {
    if !system_slug.eq_ignore_ascii_case("n64") || !matches!(profile, EmulatorProfile::RetroArch) {
        return false;
    }
    let extension = save_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    if !matches!(extension.as_deref(), Some("eep" | "sra" | "fla")) {
        return false;
    }
    save_path.with_extension("srm").is_file()
}

fn n64_bundle_target(
    system_slug: &str,
    runtime_profile: Option<&str>,
    extension: Option<&str>,
) -> Option<&'static str> {
    if !system_slug.eq_ignore_ascii_case("n64") {
        return None;
    }
    if !matches!(
        runtime_profile
            .map(|profile| profile.to_ascii_lowercase())
            .as_deref(),
        Some("n64/retroarch")
    ) {
        return None;
    }
    match extension.map(|value| value.to_ascii_lowercase()).as_deref() {
        Some("eep" | "sra" | "fla" | "mpk" | "cpk") => Some("srm"),
        _ => None,
    }
}

/// Where a missing save is restored for this system.
///
/// The state records the path a save was last synced from, and that path can be
/// a component the core never reads: an `n64/retroarch` device loads one combined
/// bundle per game, so restoring a recorded `.eep` recreates a stray and leaves
/// the bundle untouched. Under the bundle profile the target is `<stem>.srm`;
/// everything else keeps its recorded path.
fn missing_save_restore_path(
    save_path: &Path,
    system_slug: &str,
    runtime_profile: Option<&str>,
) -> PathBuf {
    let extension = save_path.extension().and_then(|value| value.to_str());
    match n64_bundle_target(system_slug, runtime_profile, extension) {
        Some(target) => save_path.with_extension(target),
        None => save_path.to_path_buf(),
    }
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
                "Skipping missing {} save restore for {}: not in this source's SYSTEMS list ('{}')",
                system_slug,
                save_path.display(),
                source_name
            );
        }
        return Ok(None);
    }
    let effective_profile =
        effective_profile_for_save(source_kind, source_profile, system_slug, save_path);
    let device_type = helper_device_type_for_upload(source_kind, &effective_profile, system_slug);
    let runtime_target =
        runtime_target_for_system(source_kind, &effective_profile, system_slug, device_type);
    // A recorded component is not a file this core loads. Restoring it recreates
    // the stray that the bundle rule exists to prevent, and the state entry
    // never moves; the target is the bundle for the same stem instead, and the
    // entry is re-keyed to it by the caller.
    let restore_path = missing_save_restore_path(
        save_path,
        system_slug,
        runtime_target
            .system_profile_value
            .as_deref()
            .or(runtime_target.runtime_profile.as_deref()),
    );
    let save_path = restore_path.as_path();

    // The recorded component is not a file this core loads. If the bundle for
    // the same stem is already here, the save is present and the entry is what
    // is stale: re-key it to the bundle and leave both files alone.
    if restore_path != save_path && existing_local_save_is_valid(&restore_path, system_slug) {
        if verbose {
            eprintln!(
                "Missing save {} is the bundle {}; re-keying the entry",
                save_path.display(),
                restore_path.display()
            );
        }
        return Ok(Some(processed_entry(
            restore_path.to_string_lossy().to_string(),
            synced_entry(
                existing_entry.sha256.clone(),
                Some(rom_sha1.to_string()),
                existing_entry.version,
                Some(system_slug),
                existing_entry.local_container,
                existing_entry.adapter_profile,
                Some(source_kind),
                Some(source_name),
                existing_entry.slot_name.as_deref(),
            ),
        )));
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
    let runtime_target = runtime_target_needs_dsv_profile(
        system_slug,
        &save_path.to_string_lossy(),
        std::fs::metadata(save_path).ok().map(|meta| meta.len()),
        runtime_target,
    );

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
        if verbose {
            eprintln!("Would download {} (dry run)", save_path.display());
        }
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
    // The same plausibility rule the download branch applies, before anything is
    // written: a payload that is not a save of this system at this target's
    // extension is refused rather than restored. An 8-byte clock state over a
    // `.srm` is the case that found it.
    let target_extension = save_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if !crate::scanner::is_plausible_save_for_system(
        &target_extension,
        canonical_bytes.len() as u64,
        system_slug,
    ) {
        report.refused_size_mismatch += 1;
        eprintln!(
            "Refusing to restore missing save {} ({} bytes): that is not a plausible {} save for .{}",
            save_path.display(),
            canonical_bytes.len(),
            system_slug,
            target_extension
        );
        return Ok(None);
    }
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
    report_restore_write(
        system_slug,
        &filename_stem(save_path),
        save_path,
        local_bytes.len(),
        runtime_target
            .system_profile_value
            .as_deref()
            .or(runtime_target.runtime_profile.as_deref()),
    );

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
    // The system is named the way the server names it: a MiSTer core is
    // `tgfx16` locally and `pc-engine` on the wire, and the profile table is
    // keyed on the server's name. Matching the local name sent no profile at
    // all, and the server refuses a projection-capable system without one.
    let clean_system = canonical_server_system_slug(system_slug);
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

    runtime_target_for_profile(&clean_system, &runtime_profile)
}

/// A `.dsv` is DeSmuME's container. The server needs that profile to strip the
/// footer, on upload and on download alike; a request without it is refused
/// with "runtimeProfile is required", which is what stopped NDS restores.
fn runtime_target_needs_dsv_profile(
    system_slug: &str,
    filename: &str,
    payload_size: Option<u64>,
    target: RuntimeTarget,
) -> RuntimeTarget {
    if system_slug != "nds" {
        return target;
    }
    let ext_is_dsv = std::path::Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("dsv"))
        .unwrap_or(false);
    // A `.sav` holding the backup plus DeSmuME's 122-byte footer is the same
    // container under another name, and the server strips the footer by
    // profile. A size that is a power of two plus the footer says so.
    let wrapped = payload_size
        .map(|size| {
            let backup = size.saturating_sub(122);
            !size.is_power_of_two() && backup.is_power_of_two() && backup >= 512
        })
        .unwrap_or(false);
    if ext_is_dsv || wrapped {
        return runtime_target_for_profile(system_slug, "nds/desmume");
    }
    target
}

fn runtime_target_for_profile(system_slug: &str, runtime_profile: &str) -> RuntimeTarget {
    let profile_name = runtime_profile
        .split_once('/')
        .map(|(_, value)| value.to_string())
        .unwrap_or_else(|| runtime_profile.to_string());
    RuntimeTarget {
        runtime_profile: Some(runtime_profile.to_string()),
        emulator_profile: Some(profile_name),
        system_profile_key: Some(system_profile_field_key(system_slug)),
        system_profile_value: Some(runtime_profile.to_string()),
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

    // The table is keyed by the server's name for a system. A caller holding
    // the local core name (`tgfx16`) gets the same answer as one holding
    // `pc-engine`.
    let system_slug = canonical_server_system_slug(system_slug);
    match system_slug.as_str() {
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
        // The server's system rows name one profile per system; a request
        // without it is refused the way an NDS restore was.
        "ngp" => Some("ngp/mednafen".to_string()),
        "virtualboy" => Some("virtualboy/mednafen".to_string()),
        "neogeo-cd" => Some("neogeo-cd/retroarch".to_string()),
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
    // A MAME nvram component's *file name* is its slot inside one machine --
    // eeprom, game, bmcu_mcu -- and the same three names appear under every
    // machine. Keying on the stem alone collapsed all 34 machines' eeproms into
    // one "eeprom" entry and kept a twelfth of the collection.
    if is_mame_nvram_component(save_path) {
        let machine = save_path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let file = save_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        return format!("arcade:{machine}:{file}");
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
    rom_index: &HashMap<String, RomIndexEntry>,
) -> Option<String> {
    let root = source.save_roots.first()?;
    let classification = classify_supported_save(save_path, None)?;
    let system_slug = classification.system_slug;
    let declared = declared_live_subdir(
        rom_index,
        &source.save_subdirs,
        &system_slug,
        &filename_stem(save_path),
    );
    match live_target_for(root, &system_slug, selection_key, declared.as_deref()) {
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
        let rom_path = rom_index
            .get(&stem_key)
            .filter(|entry| !entry.ambiguous)
            .map(|entry| entry.path.as_path());
        let Some(classification) = classify_supported_save(save_path, rom_path) else {
            continue;
        };

        let extension = save_extension(save_path);
        let save_size = save_path.metadata().ok().map(|meta| meta.len());
        // A bundle profile reads one combined file per game, so that file is the
        // save and a component beside it is a leftover. Without this the native
        // -component bonus ties with the bundle and the tie is decided by the
        // order the directory was walked -- which is how a device keeps a
        // component it will then refuse the bundle against forever.
        let effective_profile = effective_profile_for_save(
            source_kind,
            source_profile,
            &classification.system_slug,
            save_path,
        );
        let bundle_profile = classification.system_slug == "n64"
            && matches!(effective_profile, EmulatorProfile::RetroArch);
        let score = if bundle_profile {
            if extension.as_deref() == Some("srm") {
                2
            } else {
                1
            }
        } else {
            preferred_extension_for_system(
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
            .unwrap_or(1)
        };

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

/// Whether one payload is the other's bytes with `0xFF` padding on the end.
///
/// A RetroArch device pads a cartridge save to whatever size its core was set up
/// with -- a 32 KiB GBA save becomes 128 KiB, an 8 KiB Master System save
/// becomes 64 KiB -- and fills the rest with `0xFF`. The bytes that carry the
/// save are identical, so a size difference with padding on one side is not a
/// conflict, and refusing it leaves the padded device unable to ever adopt the
/// server's copy.
fn same_save_with_padding(shorter: &[u8], longer: &[u8]) -> bool {
    longer.len() >= shorter.len()
        && longer.starts_with(shorter)
        && longer[shorter.len()..].iter().all(|byte| *byte == 0xFF)
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

/// True when every component file of the machine holding `save_path` carries
/// the same bytes as `bytes`. MAME components are independent devices; when
/// they all read identically, one payload was written over the set.
fn arcade_machine_components_are_identical(save_path: &Path, bytes: &[u8]) -> bool {
    let Some(parent) = save_path.parent() else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return false;
    };
    let mut total = 0usize;
    let mut identical = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        total += 1;
        if path == save_path
            || std::fs::read(&path)
                .map(|other| other == bytes)
                .unwrap_or(false)
        {
            identical += 1;
        }
    }
    total > 1 && identical == total
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

fn is_empty_n64_controller_pak(save_path: &Path, bytes: &[u8], system_slug: &str) -> bool {
    if system_slug != "n64" {
        return false;
    }
    let Some(extension) = save_path.extension().and_then(|value| value.to_str()) else {
        return false;
    };
    if !extension.eq_ignore_ascii_case("cpk") && !extension.eq_ignore_ascii_case("mpk") {
        return false;
    }
    n64_controller_pak_entry_count(bytes) == Some(0)
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
    machine: Option<&str>,
    runtime_target: &RuntimeTarget,
    verbose: bool,
) -> Result<UploadSaveResponse> {
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
        machine,
        Some(runtime_target),
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
                    machine,
                    Some(runtime_target),
                )?
            } else {
                return Err(err);
            }
        }
    };
    // A response that names a save is a stored save. The server's success body
    // carries `id` and `sha256` and, on some paths, no `version` at all; reading
    // that as "nothing was stored" reported every such upload as accepted and
    // discarded while the record was on disk.
    Ok(upload_response)
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

/// The name a save's game is known by, with the parts that vary between
/// emulators removed: case, punctuation, and any parenthesised or bracketed
/// group (region, revision, dump tags). "Madden NFL '94 (USA, Europe)" and
/// "Madden NFL 94" both reduce to `maddennfl94`.
fn save_name_identity(path: &std::path::Path) -> String {
    let stem = filename_stem(path).to_ascii_lowercase();
    let mut out = String::with_capacity(stem.len());
    let mut depth = 0usize;
    for ch in stem.chars() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 && ch.is_ascii_alphanumeric() => out.push(ch),
            _ => {}
        }
    }
    out
}

/// A save this helper has already synced, with the same content, for the same
/// game, in the same slot, under a name that differs only in the way emulators
/// spell names. The name check is what keeps two different games that both
/// wrote a blank save from being collapsed into one.
fn save_already_synced_under_another_name<'a>(
    known_saves: &'a HashMap<String, SyncedEntry>,
    save_path: &std::path::Path,
    system_slug: &str,
    slot_name: &str,
    sha256: &str,
) -> Option<(&'a str, &'a SyncedEntry)> {
    let identity = save_name_identity(save_path);
    let current = save_path.to_string_lossy();
    known_saves.iter().find_map(|(path, entry)| {
        if path == current.as_ref() {
            return None;
        }
        if entry.sha256 != sha256
            || entry.system_slug.as_deref() != Some(system_slug)
            || entry.slot_name.as_deref().unwrap_or("default") != slot_name
        {
            return None;
        }
        if save_name_identity(std::path::Path::new(path)) != identity {
            return None;
        }
        Some((path.as_str(), entry))
    })
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

    // A RetroArch device pads a cartridge save with 0xFF; another core writes it
    // at its true length. Same save, and the size difference must not deadlock
    // the two devices.
    #[test]
    fn a_padded_payload_is_the_same_save() {
        let mut shorter = vec![0xA5u8; 32768];
        shorter[0] = 0x01;
        let mut longer = shorter.clone();
        longer.extend(vec![0xFFu8; 98304]);

        assert!(same_save_with_padding(&shorter, &longer));
        assert!(same_save_with_padding(&longer[..shorter.len()], &longer));
    }

    #[test]
    fn a_difference_inside_the_save_is_not_padding() {
        let shorter = vec![0xA5u8; 32768];
        let mut longer = shorter.clone();
        longer.extend(vec![0xFFu8; 98304]);
        longer[100] = 0x00;

        assert!(!same_save_with_padding(&shorter, &longer));
    }

    #[test]
    fn padding_must_be_0xff_to_count_as_padding() {
        let shorter = vec![0xA5u8; 16];
        let mut longer = shorter.clone();
        longer.extend(vec![0x00u8; 16]);

        assert!(!same_save_with_padding(&shorter, &longer));
    }

    fn resolved_source(
        name: &str,
        kind: SourceKind,
        profile: EmulatorProfile,
        root: &str,
        origin: &str,
    ) -> ResolvedSource {
        ResolvedSource {
            id: name.to_string(),
            name: name.to_string(),
            kind,
            profile,
            save_roots: vec![PathBuf::from(root)],
            rom_roots: vec![],
            recursive: true,
            systems: vec![],
            save_subdirs: Default::default(),
            create_missing_system_dirs: false,
            managed: false,
            origin: origin.to_string(),
        }
    }

    // A RetroDECK install gets an auto-created default source as well as the
    // configured one, both over the same saves directory. Every save then runs
    // twice, and the second run asked the server for a different N64 profile.
    #[test]
    fn an_auto_default_source_yields_its_root_to_a_configured_one() {
        let configured = resolved_source(
            "retrodeck",
            SourceKind::RetroArch,
            EmulatorProfile::RetroArch,
            "/home/deck/retrodeck/saves",
            "manual",
        );
        let auto = resolved_source(
            "steamdeck_default",
            SourceKind::SteamDeck,
            EmulatorProfile::Generic,
            "/home/deck/retrodeck/saves",
            "autoscan-known",
        );

        let (kept, notices) = prefer_configured_sources(vec![auto, configured]);

        assert_eq!(kept.len(), 1, "the auto source was kept");
        assert_eq!(kept[0].name, "retrodeck");
        assert_eq!(notices.len(), 1);
        assert!(
            notices[0].contains("steamdeck_default") && notices[0].contains("retrodeck"),
            "the skip does not name both sources: {}",
            notices[0]
        );
    }

    // Two configured sources over one root is the operator's arrangement: both
    // run as before, and the overlap is reported rather than resolved.
    #[test]
    fn two_configured_sources_over_one_root_are_reported_not_removed() {
        let first = resolved_source(
            "retrodeck",
            SourceKind::RetroArch,
            EmulatorProfile::RetroArch,
            "/home/deck/retrodeck/saves",
            "manual",
        );
        let second = resolved_source(
            "second",
            SourceKind::SteamDeck,
            EmulatorProfile::Generic,
            "/home/deck/retrodeck/saves",
            "manual",
        );

        let (kept, notices) = prefer_configured_sources(vec![first, second]);

        assert_eq!(kept.len(), 2);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("both claim"), "{}", notices[0]);
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

    // ------------------------------------------------------------------
    // Stale sync.lock recovery tests (mirrors helpers/mister/src/syncer.rs).
    // ------------------------------------------------------------------

    #[test]
    fn parse_lock_pid_reads_pid_line() {
        let body = "pid=19199\nstarted_at=2026-06-06T13:29:25Z\n";
        assert_eq!(parse_lock_pid(body), Some(19199));
    }

    #[test]
    fn parse_lock_pid_handles_missing_or_garbage() {
        assert_eq!(parse_lock_pid(""), None);
        assert_eq!(parse_lock_pid("started_at=2026-06-06T13:29:25Z\n"), None);
        assert_eq!(parse_lock_pid("pid=not-a-number\n"), None);
        assert_eq!(parse_lock_pid("pid=42  \nstarted_at=x\n"), Some(42));
    }

    #[test]
    fn classify_existing_lock_malformed_treated_as_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sync.lock");
        let body = "this is not what we ever write";
        fs::write(&path, body).unwrap();
        assert_eq!(classify_existing_lock(&path, body), StaleVerdict::Malformed);
    }

    #[test]
    fn classify_existing_lock_live_pid_active() {
        let pid = std::process::id();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sync.lock");
        let body = format!("pid={}\nstarted_at=2026-06-06T13:29:25Z\n", pid);
        fs::write(&path, &body).unwrap();
        assert_eq!(classify_existing_lock(&path, &body), StaleVerdict::Active);
    }

    // Note: the dead-PID and stale-takeover tests live in the mister + steamdeck
    // suites. They rely on pid_alive() returning false for synthetic PIDs, which
    // is only true on Unix targets (where kill(pid,0) checks /proc). On Windows
    // pid_alive() conservatively returns true and the 6h-age fallback is what
    // recovers stuck locks; testing that path deterministically would require
    // mocking SystemTime::elapsed which is over-engineered for the value.

    #[test]
    fn acquire_refuses_when_lock_is_actively_held_by_live_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let _held = SyncLock::acquire(state_dir).expect("first acquire");
        let err = SyncLock::acquire(state_dir).expect_err("second acquire should bail");
        assert!(format!("{err}").contains("sync is al actief"));
    }

    #[cfg(unix)]
    #[test]
    fn acquire_takes_over_dead_pid_lock_and_cleans_up_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let lock_path = state_dir.join("sync.lock");

        let dead_pid: u32 = (i32::MAX as u32) - 7;
        assert!(!pid_alive(dead_pid));
        fs::write(
            &lock_path,
            format!("pid={}\nstarted_at=2026-06-06T13:29:25Z\n", dead_pid),
        )
        .unwrap();
        assert!(lock_path.exists());

        let lock = SyncLock::acquire(state_dir).expect("should take over stale lock");

        let contents = fs::read_to_string(&lock_path).unwrap();
        assert_eq!(parse_lock_pid(&contents), Some(std::process::id()));

        drop(lock);
        assert!(!lock_path.exists());
    }

    #[test]
    fn a_refusal_is_about_the_save_only_when_the_upload_returned_it() {
        let upload = anyhow::Error::new(crate::api::UploadRequestError(anyhow::anyhow!(
            "status=400 and the save was refused"
        )));
        assert!(refusal_is_about_the_save(&upload));

        let lookup = anyhow::anyhow!("GET /save/latest: HTTP request faalde: status=400");
        assert!(!refusal_is_about_the_save(&lookup));
    }

    #[test]
    fn a_routing_failure_is_not_a_permanent_rejection() {
        let err =
            anyhow::anyhow!("HTTP request faalde: status=404 Not Found body=404 page not found");
        assert_eq!(permanent_rejection_status(&err), None);
    }

    #[test]
    fn a_reasoned_refusal_is_still_a_permanent_rejection() {
        let err = anyhow::anyhow!(
            r#"HTTP request faalde: status=422 Unprocessable Entity body={{"success":false,"reason":"snes raw save payload is blank"}}"#
        );
        assert_eq!(permanent_rejection_status(&err), Some(422));
    }

    #[test]
    fn a_client_error_without_a_body_is_retryable() {
        let err = anyhow::anyhow!("HTTP request faalde: status=400 Bad Request body=");
        assert_eq!(permanent_rejection_status(&err), None);
    }

    #[test]
    fn a_client_error_with_an_unstructured_body_is_retryable() {
        let err = anyhow::anyhow!("HTTP request faalde: status=412 Precondition Failed body=no");
        assert_eq!(permanent_rejection_status(&err), None);
    }

    #[test]
    fn a_server_error_is_retryable() {
        let err = anyhow::anyhow!(
            r#"HTTP request faalde: status=503 Service Unavailable body={{"reason":"down"}}"#
        );
        assert_eq!(permanent_rejection_status(&err), None);
    }

    #[test]
    fn a_source_admits_only_the_systems_it_lists() {
        // The gate that dropped 60 classified MAME files: the source's own
        // SYSTEMS list. It compares the canonical slug, so the config and the
        // classifier have to agree on the name.
        let systems = vec!["snes".to_string(), "genesis".to_string()];
        assert!(source_allows_system(&systems, "snes"));
        assert!(source_allows_system(&systems, "SNES"));
        assert!(!source_allows_system(&systems, "arcade"));
        assert!(!source_allows_system(&systems, ""));
    }

    #[test]
    fn a_source_listing_a_wildcard_admits_every_system() {
        let systems = vec!["*".to_string()];
        assert!(source_allows_system(&systems, "arcade"));
        assert!(source_allows_system(&systems, "anything-at-all"));
    }

    #[test]
    fn two_machines_each_keeping_an_eeprom_are_not_variants_of_each_other() {
        // Every machine writes a file called `eeprom`; keying on the file name
        // alone made 34 of them one entry and kept a twelfth of the collection.
        let first = normalize_selection_key("/m/nvram/akatana/eeprom");
        let second = normalize_selection_key("/m/nvram/acedrvrw/eeprom");
        assert_ne!(first, second);
    }

    #[test]
    fn one_machines_eeprom_and_game_are_distinct_components() {
        let eeprom = normalize_selection_key("/m/nvram/akatana/eeprom");
        let game = normalize_selection_key("/m/nvram/akatana/game");
        assert_ne!(eeprom, game);
    }

    #[test]
    fn the_same_nvram_component_is_still_deduped() {
        // Same machine, same slot, found through two roots: one key, so the
        // dedup still collapses it.
        let first = normalize_selection_key("/a/nvram/sfa3/eeprom");
        let second = normalize_selection_key("/b/nvram/SFA3/EEPROM");
        assert_eq!(first, second);
    }

    fn normalize_selection_key(path: &str) -> String {
        save_selection_key(Path::new(path))
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
            // The slug the classifier actually produces for a MiSTer PC Engine
            // save; the table is keyed on the server's name, so both have to
            // answer the same way.
            (
                "tgfx16",
                EmulatorProfile::RetroArch,
                "pc-engine/retroarch-beetle-pce",
            ),
            ("tgfx16", EmulatorProfile::Mister, "pc-engine/mister"),
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
