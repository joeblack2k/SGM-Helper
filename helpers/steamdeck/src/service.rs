use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::api::ApiClient;
use crate::config::AppConfig;
use crate::sources::{SourceKind, load_source_store, resolved_sources_or_default};
use crate::state::{AuthState, now_rfc3339};
use crate::syncer::{SyncOptions, SyncReport, run_sync};

#[derive(Debug, Clone)]
pub struct ServiceRunOptions {
    pub heartbeat_interval_secs: u64,
    pub reconcile_interval_secs: u64,
    pub force_upload: bool,
    pub dry_run: bool,
    pub scan: bool,
    pub deep_scan: bool,
    pub apply_scan: bool,
    pub slot_name: String,
    pub default_source_kind: SourceKind,
    pub max_cycles: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceBackend {
    LinuxSystemdUser,
    LinuxSystemdSystem,
    LinuxCron,
    WindowsTask,
}

#[derive(Debug, Clone)]
pub struct ServiceStatus {
    pub installed: bool,
    pub details: String,
}

#[derive(Debug, Clone)]
struct ServiceRuntimeState {
    started_at: String,
    last_sync_started_at: Option<String>,
    last_sync_finished_at: Option<String>,
    last_sync_ok: Option<bool>,
    last_sync: Option<SyncReport>,
    last_error: Option<String>,
    last_event: Option<String>,
    cycles: u32,
}

impl ServiceRuntimeState {
    fn new(started_at: String) -> Self {
        Self {
            started_at,
            last_sync_started_at: None,
            last_sync_finished_at: None,
            last_sync_ok: None,
            last_sync: None,
            last_error: None,
            last_event: None,
            cycles: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct ServiceAction {
    reason: String,
    scan: bool,
    deep_scan: bool,
    apply_scan: bool,
}

#[derive(Debug)]
enum ServiceSignal {
    SyncRequested(String),
    ScanRequested(String),
    DeepScanRequested(String),
    ConfigChanged(String),
    SaveChanged(String),
}

pub fn run_service(
    config: &AppConfig,
    auth: Option<&AuthState>,
    options: ServiceRunOptions,
    verbose: bool,
    quiet: bool,
) -> Result<()> {
    if options.heartbeat_interval_secs == 0 {
        bail!("--heartbeat-interval moet >= 1 zijn");
    }
    if options.reconcile_interval_secs == 0 {
        bail!("--reconcile-interval moet >= 1 zijn");
    }

    let token = auth.map(|value| value.token.clone());
    let api = ApiClient::new(config.base_url(), config.route_prefix.clone(), token)?;
    let app_password = if config.app_password.trim().is_empty() {
        None
    } else {
        Some(config.app_password.trim())
    };
    let binary_path = env::current_exe().context("kan executable pad niet bepalen")?;
    let started_instant = Instant::now();
    let mut runtime = ServiceRuntimeState::new(now_rfc3339());
    let stop_flag = Arc::new(AtomicBool::new(false));

    {
        let stop_flag = Arc::clone(&stop_flag);
        ctrlc::set_handler(move || {
            stop_flag.store(true, Ordering::SeqCst);
        })?;
    }

    let (tx, rx) = mpsc::channel::<ServiceSignal>();
    spawn_service_sse_listener(
        api.clone(),
        tx,
        Arc::clone(&stop_flag),
        verbose,
        app_password.map(str::to_string),
    );

    send_heartbeat(
        &api,
        config,
        auth,
        &options,
        &runtime,
        "starting",
        started_instant.elapsed().as_secs(),
        app_password,
        &binary_path,
        verbose,
    );

    let heartbeat_interval = Duration::from_secs(options.heartbeat_interval_secs);
    let reconcile_interval = Duration::from_secs(options.reconcile_interval_secs);
    let mut next_heartbeat = Instant::now() + heartbeat_interval;
    let mut next_reconcile = Instant::now();
    let mut pending_action = Some(ServiceAction {
        reason: "startup".to_string(),
        scan: options.scan,
        deep_scan: options.deep_scan,
        apply_scan: options.apply_scan,
    });

    while !stop_flag.load(Ordering::SeqCst) {
        if let Some(action) = pending_action.take() {
            if !quiet {
                println!("Service sync triggered: {}", action.reason);
            }

            runtime.last_event = Some(action.reason.clone());
            runtime.last_sync_started_at = Some(now_rfc3339());
            send_heartbeat(
                &api,
                config,
                auth,
                &options,
                &runtime,
                "syncing",
                started_instant.elapsed().as_secs(),
                app_password,
                &binary_path,
                verbose,
            );

            let sync_result = run_sync(
                config,
                auth,
                &SyncOptions {
                    force_upload: options.force_upload,
                    dry_run: options.dry_run,
                    scan: action.scan,
                    deep_scan: action.deep_scan,
                    apply_scan: action.apply_scan,
                    slot_name: options.slot_name.clone(),
                    default_source_kind: options.default_source_kind.clone(),
                },
                verbose,
            );

            runtime.last_sync_finished_at = Some(now_rfc3339());
            runtime.cycles = runtime.cycles.saturating_add(1);
            match sync_result {
                Ok(report) => {
                    if !quiet {
                        println!(
                            "Service sync complete: scanned={} uploaded={} downloaded={} in_sync={} conflicts={} skipped={} errors={}",
                            report.scanned,
                            report.uploaded,
                            report.downloaded,
                            report.in_sync,
                            report.conflicts,
                            report.skipped,
                            report.errors
                        );
                    }
                    runtime.last_sync_ok = Some(report.errors == 0);
                    runtime.last_error = None;
                    runtime.last_sync = Some(report);
                }
                Err(err) => {
                    let message = err.to_string();
                    if !quiet || verbose {
                        eprintln!("Service sync failed: {}", message);
                    }
                    runtime.last_sync_ok = Some(false);
                    runtime.last_error = Some(message);
                }
            }

            let status = if runtime.last_error.is_some() {
                "backoff"
            } else {
                "idle"
            };
            send_heartbeat(
                &api,
                config,
                auth,
                &options,
                &runtime,
                status,
                started_instant.elapsed().as_secs(),
                app_password,
                &binary_path,
                verbose,
            );

            next_heartbeat = Instant::now() + heartbeat_interval;
            next_reconcile = Instant::now() + reconcile_interval;

            if let Some(max_cycles) = options.max_cycles
                && runtime.cycles >= max_cycles
            {
                break;
            }
            continue;
        }

        let now = Instant::now();
        if now >= next_reconcile {
            pending_action = Some(ServiceAction {
                reason: "periodic.reconcile".to_string(),
                scan: false,
                deep_scan: false,
                apply_scan: false,
            });
            continue;
        }

        if now >= next_heartbeat {
            send_heartbeat(
                &api,
                config,
                auth,
                &options,
                &runtime,
                "idle",
                started_instant.elapsed().as_secs(),
                app_password,
                &binary_path,
                verbose,
            );
            next_heartbeat = Instant::now() + heartbeat_interval;
            continue;
        }

        let wait_for = next_heartbeat
            .min(next_reconcile)
            .saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait_for) {
            Ok(ServiceSignal::SyncRequested(reason))
            | Ok(ServiceSignal::ConfigChanged(reason))
            | Ok(ServiceSignal::SaveChanged(reason)) => {
                pending_action = Some(ServiceAction {
                    reason,
                    scan: false,
                    deep_scan: false,
                    apply_scan: false,
                });
            }
            Ok(ServiceSignal::ScanRequested(reason)) => {
                pending_action = Some(ServiceAction {
                    reason,
                    scan: true,
                    deep_scan: false,
                    apply_scan: false,
                });
            }
            Ok(ServiceSignal::DeepScanRequested(reason)) => {
                pending_action = Some(ServiceAction {
                    reason,
                    scan: false,
                    deep_scan: true,
                    apply_scan: options.apply_scan,
                });
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }
    }

    send_heartbeat(
        &api,
        config,
        auth,
        &options,
        &runtime,
        "stopping",
        started_instant.elapsed().as_secs(),
        app_password,
        &binary_path,
        verbose,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn send_heartbeat(
    api: &ApiClient,
    config: &AppConfig,
    auth: Option<&AuthState>,
    options: &ServiceRunOptions,
    runtime: &ServiceRuntimeState,
    status: &str,
    uptime_secs: u64,
    app_password: Option<&str>,
    binary_path: &Path,
    verbose: bool,
) {
    let payload = match build_heartbeat_payload(
        config,
        auth,
        &options.default_source_kind,
        options,
        runtime,
        status,
        uptime_secs,
        binary_path,
    ) {
        Ok(payload) => payload,
        Err(err) => {
            if verbose {
                eprintln!("Heartbeat payload build skipped: {}", err);
            }
            return;
        }
    };

    match api.helper_heartbeat(&payload, app_password) {
        Ok(response) => {
            if verbose
                && response
                    .get("unsupported")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                eprintln!("Backend does not support /helpers/heartbeat yet; service continues.");
            }
        }
        Err(err) => {
            if verbose {
                eprintln!("Heartbeat skipped: {}", err);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_heartbeat_payload(
    config: &AppConfig,
    auth: Option<&AuthState>,
    default_source_kind: &SourceKind,
    options: &ServiceRunOptions,
    runtime: &ServiceRuntimeState,
    status: &str,
    uptime_secs: u64,
    binary_path: &Path,
) -> Result<Value> {
    let hostname = hostname::get()
        .ok()
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| default_source_kind.as_str().to_string());
    let state_dir = config.resolved_state_dir()?;
    let source_snapshot = collect_source_snapshot(config, default_source_kind);

    let redacted_config = json!({
        "url": config.url,
        "port": config.port,
        "baseUrl": config.base_url(),
        "email": config.email,
        "appPasswordConfigured": !config.app_password.trim().is_empty(),
        "root": config.root,
        "stateDir": config.state_dir,
        "watch": config.watch,
        "watchInterval": config.watch_interval,
        "forceUpload": config.force_upload,
        "dryRun": config.dry_run,
        "routePrefix": config.route_prefix,
        "sources": source_snapshot.sources,
    });
    let config_hash = sha256_json(&redacted_config)?;

    Ok(json!({
        "schemaVersion": 1,
        "helper": {
            "name": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
            "deviceType": default_source_kind.helper_device_type(),
            "defaultKind": default_source_kind.as_str(),
            "hostname": hostname,
            "platform": env::consts::OS,
            "arch": env::consts::ARCH,
            "pid": std::process::id(),
            "startedAt": runtime.started_at,
            "uptimeSeconds": uptime_secs,
            "binaryPath": binary_path,
            "binaryDir": config.binary_dir,
            "configPath": config.config_path,
            "stateDir": state_dir,
        },
        "service": {
            "mode": "daemon",
            "status": status,
            "loop": "sse-plus-periodic-reconcile",
            "heartbeatInterval": options.heartbeat_interval_secs,
            "reconcileInterval": options.reconcile_interval_secs,
            "controlChannel": "GET /events",
            "lastSyncStartedAt": runtime.last_sync_started_at,
            "lastSyncFinishedAt": runtime.last_sync_finished_at,
            "lastSyncOk": runtime.last_sync_ok,
            "lastError": runtime.last_error,
            "lastEvent": runtime.last_event,
            "syncCycles": runtime.cycles,
        },
        "sensors": {
            "online": status != "stopping",
            "authenticated": auth.is_some(),
            "configHash": config_hash,
            "configReadable": source_snapshot.error.is_none(),
            "configError": source_snapshot.error,
            "sourceCount": source_snapshot.source_count,
            "savePathCount": source_snapshot.save_path_count,
            "romPathCount": source_snapshot.rom_path_count,
            "configuredSystems": source_snapshot.configured_systems,
            "supportedSystems": crate::sources::default_systems_for_kind(default_source_kind),
            "syncLockPresent": state_dir.join("sync.lock").exists(),
            "lastSync": runtime.last_sync.as_ref().map(sync_report_json),
        },
        "config": redacted_config,
        "capabilities": {
            "serviceRun": true,
            "serviceInstall": true,
            "heartbeatEndpoint": "POST /helpers/heartbeat",
            "configSyncEndpoint": "POST /helpers/config/sync",
            "controlEvents": [
                "sync.requested",
                "scan.requested",
                "deep_scan.requested",
                "config.changed",
                "save.changed",
                "save_created",
                "save_parsed",
                "save_deleted",
                "conflict_created",
                "conflict_resolved"
            ],
            "schedulerFallback": true,
            "backendPolicyWins": true
        }
    }))
}

#[derive(Debug)]
struct SourceSnapshot {
    sources: Vec<Value>,
    source_count: usize,
    save_path_count: usize,
    rom_path_count: usize,
    configured_systems: Vec<String>,
    error: Option<String>,
}

fn collect_source_snapshot(config: &AppConfig, default_source_kind: &SourceKind) -> SourceSnapshot {
    let store = match load_source_store(&config.config_path) {
        Ok(store) => store,
        Err(err) => {
            return SourceSnapshot {
                sources: Vec::new(),
                source_count: 0,
                save_path_count: 0,
                rom_path_count: 0,
                configured_systems: Vec::new(),
                error: Some(err.to_string()),
            };
        }
    };
    let sources = match resolved_sources_or_default(&store, config, default_source_kind.clone()) {
        Ok(sources) => sources,
        Err(err) => {
            return SourceSnapshot {
                sources: Vec::new(),
                source_count: 0,
                save_path_count: 0,
                rom_path_count: 0,
                configured_systems: Vec::new(),
                error: Some(err.to_string()),
            };
        }
    };

    let mut systems = BTreeSet::new();
    let mut save_path_count = 0;
    let mut rom_path_count = 0;
    let mut source_values = Vec::new();
    for source in sources {
        save_path_count += source.save_roots.len();
        rom_path_count += source.rom_roots.len();
        for system in &source.systems {
            systems.insert(system.clone());
        }
        source_values.push(json!({
            "id": source.id,
            "label": source.name,
            "kind": source.kind.as_str(),
            "profile": source.profile.as_str(),
            "savePaths": source.save_roots,
            "romPaths": source.rom_roots,
            "recursive": source.recursive,
            "systems": source.systems,
            "createMissingSystemDirs": source.create_missing_system_dirs,
            "managed": source.managed,
            "origin": source.origin,
        }));
    }

    SourceSnapshot {
        source_count: source_values.len(),
        sources: source_values,
        save_path_count,
        rom_path_count,
        configured_systems: systems.into_iter().collect(),
        error: None,
    }
}

fn sync_report_json(report: &SyncReport) -> Value {
    json!({
        "scanned": report.scanned,
        "uploaded": report.uploaded,
        "downloaded": report.downloaded,
        "inSync": report.in_sync,
        "conflicts": report.conflicts,
        "skipped": report.skipped,
        "errors": report.errors,
    })
}

fn sha256_json(value: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(value).context("kan config snapshot niet hashen")?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn spawn_service_sse_listener(
    api: ApiClient,
    tx: mpsc::Sender<ServiceSignal>,
    stop_flag: Arc<AtomicBool>,
    verbose: bool,
    _app_password: Option<String>,
) {
    thread::spawn(move || {
        while !stop_flag.load(Ordering::SeqCst) {
            let response = match api.open_events() {
                Ok(response) => response,
                Err(err) => {
                    if verbose {
                        eprintln!(
                            "Service SSE unavailable, polling/reconcile remains active: {}",
                            err
                        );
                    }
                    thread::sleep(Duration::from_secs(5));
                    continue;
                }
            };

            let mut current_event: Option<String> = None;
            let mut current_data = String::new();
            let reader = BufReader::new(response);
            for line in reader.lines() {
                if stop_flag.load(Ordering::SeqCst) {
                    break;
                }

                let Ok(line) = line else {
                    break;
                };
                let trimmed = line.trim();
                if trimmed.starts_with("event:") {
                    current_event = Some(trimmed.trim_start_matches("event:").trim().to_string());
                    continue;
                }
                if trimmed.starts_with("data:") {
                    if !current_data.is_empty() {
                        current_data.push('\n');
                    }
                    current_data.push_str(trimmed.trim_start_matches("data:").trim());
                    continue;
                }
                if trimmed.is_empty() {
                    if let Some(event_name) = current_event.take()
                        && let Some(signal) = service_signal_from_event(&event_name, &current_data)
                    {
                        let _ = tx.send(signal);
                    }
                    current_data.clear();
                }
            }

            thread::sleep(Duration::from_secs(1));
        }
    });
}

fn service_signal_from_event(event_name: &str, data: &str) -> Option<ServiceSignal> {
    let normalized = event_name.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "sync.requested" | "helper.sync.requested" => {
            Some(ServiceSignal::SyncRequested(event_name.to_string()))
        }
        "scan.requested" | "helper.scan.requested" => {
            Some(ServiceSignal::ScanRequested(event_name.to_string()))
        }
        "deep_scan.requested" | "deep-scan.requested" | "helper.deep_scan.requested" => {
            Some(ServiceSignal::DeepScanRequested(event_name.to_string()))
        }
        "config.changed" | "helper.config.changed" => {
            Some(ServiceSignal::ConfigChanged(event_name.to_string()))
        }
        "save.changed" | "save_created" | "save_parsed" | "save_deleted" | "conflict_created"
        | "conflict_resolved" => Some(ServiceSignal::SaveChanged(event_name.to_string())),
        _ => service_signal_from_json_data(data),
    }
}

fn service_signal_from_json_data(data: &str) -> Option<ServiceSignal> {
    let value: Value = serde_json::from_str(data).ok()?;
    let action = value
        .get("action")
        .or_else(|| value.get("command"))
        .and_then(Value::as_str)?
        .trim()
        .to_ascii_lowercase();
    match action.as_str() {
        "sync" => Some(ServiceSignal::SyncRequested("data.action:sync".to_string())),
        "scan" => Some(ServiceSignal::ScanRequested("data.action:scan".to_string())),
        "deep_scan" | "deep-scan" => Some(ServiceSignal::DeepScanRequested(
            "data.action:deep_scan".to_string(),
        )),
        "config.changed" | "reload_config" => Some(ServiceSignal::ConfigChanged(
            "data.action:config".to_string(),
        )),
        _ => None,
    }
}

pub fn detect_service_backend() -> ServiceBackend {
    if cfg!(windows) {
        return ServiceBackend::WindowsTask;
    }
    if systemctl_available() {
        if is_root_user() {
            ServiceBackend::LinuxSystemdSystem
        } else {
            ServiceBackend::LinuxSystemdUser
        }
    } else {
        ServiceBackend::LinuxCron
    }
}

pub fn install_service(
    backend: ServiceBackend,
    service_name: &str,
    binary_path: &Path,
    config_path: &Path,
    heartbeat_interval_secs: u64,
    reconcile_interval_secs: u64,
) -> Result<String> {
    if heartbeat_interval_secs == 0 {
        bail!("--heartbeat-interval moet >= 1 zijn");
    }
    if reconcile_interval_secs == 0 {
        bail!("--reconcile-interval moet >= 1 zijn");
    }

    let command = build_service_run_command(
        binary_path,
        config_path,
        heartbeat_interval_secs,
        reconcile_interval_secs,
    );
    match backend {
        ServiceBackend::LinuxSystemdUser | ServiceBackend::LinuxSystemdSystem => {
            install_systemd_service(backend, service_name, binary_path, &command)
        }
        ServiceBackend::LinuxCron => install_cron_service(service_name, &command),
        ServiceBackend::WindowsTask => install_windows_service_task(service_name, &command),
    }
}

pub fn service_status(
    backend: ServiceBackend,
    service_name: &str,
    binary_path: &Path,
    config_path: &Path,
) -> Result<ServiceStatus> {
    let command = build_service_run_command(binary_path, config_path, 30, 1800);
    match backend {
        ServiceBackend::LinuxSystemdUser | ServiceBackend::LinuxSystemdSystem => {
            systemd_service_status(backend, service_name)
        }
        ServiceBackend::LinuxCron => cron_service_status(service_name, &command),
        ServiceBackend::WindowsTask => windows_service_task_status(service_name),
    }
}

pub fn uninstall_service(backend: ServiceBackend, service_name: &str) -> Result<String> {
    match backend {
        ServiceBackend::LinuxSystemdUser | ServiceBackend::LinuxSystemdSystem => {
            uninstall_systemd_service(backend, service_name)
        }
        ServiceBackend::LinuxCron => uninstall_cron_service(service_name),
        ServiceBackend::WindowsTask => uninstall_windows_service_task(service_name),
    }
}

pub fn build_service_run_command(
    binary_path: &Path,
    config_path: &Path,
    heartbeat_interval_secs: u64,
    reconcile_interval_secs: u64,
) -> String {
    format!(
        "\"{}\" --config \"{}\" service run --quiet --heartbeat-interval {} --reconcile-interval {}",
        binary_path.display(),
        config_path.display(),
        heartbeat_interval_secs,
        reconcile_interval_secs
    )
}

fn install_systemd_service(
    backend: ServiceBackend,
    service_name: &str,
    binary_path: &Path,
    command: &str,
) -> Result<String> {
    let unit_path = systemd_unit_path(backend, service_name)?;
    if let Some(parent) = unit_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("kan systemd map niet maken: {}", parent.display()))?;
    }

    let wanted_by = if matches!(backend, ServiceBackend::LinuxSystemdUser) {
        "default.target"
    } else {
        "multi-user.target"
    };
    let working_dir = binary_path.parent().unwrap_or_else(|| Path::new("."));
    let unit = format!(
        "[Unit]\nDescription={}\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nWorkingDirectory={}\nExecStart={}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy={}\n",
        service_name,
        working_dir.display(),
        command,
        wanted_by
    );
    fs::write(&unit_path, unit)
        .with_context(|| format!("kan systemd unit niet schrijven: {}", unit_path.display()))?;

    run_systemctl(backend, &["daemon-reload"])?;
    run_systemctl(
        backend,
        &["enable", "--now", &systemd_unit_name(service_name)],
    )?;
    Ok(format!(
        "Systemd service installed: {}",
        unit_path.display()
    ))
}

fn systemd_service_status(backend: ServiceBackend, service_name: &str) -> Result<ServiceStatus> {
    let unit_name = systemd_unit_name(service_name);
    let output = systemctl_output(backend, &["status", "--no-pager", &unit_name])?;
    Ok(ServiceStatus {
        installed: output.status.success(),
        details: combined_output(&output),
    })
}

fn uninstall_systemd_service(backend: ServiceBackend, service_name: &str) -> Result<String> {
    let unit_name = systemd_unit_name(service_name);
    let _ = run_systemctl(backend, &["disable", "--now", &unit_name]);
    let unit_path = systemd_unit_path(backend, service_name)?;
    if unit_path.exists() {
        fs::remove_file(&unit_path).with_context(|| {
            format!("kan systemd unit niet verwijderen: {}", unit_path.display())
        })?;
    }
    let _ = run_systemctl(backend, &["daemon-reload"]);
    Ok("Systemd service verwijderd".to_string())
}

fn install_cron_service(service_name: &str, command: &str) -> Result<String> {
    let marker = service_marker(service_name);
    let mut lines = current_crontab_lines()?;
    lines.retain(|line| !line.contains(&marker));
    lines.push(format!("@reboot {} # {}", command, marker));
    write_crontab_lines(&lines)?;
    Ok(format!(
        "@reboot service installed: {} # {}",
        command, marker
    ))
}

fn cron_service_status(service_name: &str, command: &str) -> Result<ServiceStatus> {
    let marker = service_marker(service_name);
    let lines = current_crontab_lines()?;
    let maybe = lines
        .into_iter()
        .find(|line| line.contains(&marker) || line.contains(command));
    if let Some(line) = maybe {
        return Ok(ServiceStatus {
            installed: true,
            details: line,
        });
    }
    Ok(ServiceStatus {
        installed: false,
        details: "Service cron entry niet gevonden".to_string(),
    })
}

fn uninstall_cron_service(service_name: &str) -> Result<String> {
    let marker = service_marker(service_name);
    let mut lines = current_crontab_lines()?;
    let before = lines.len();
    lines.retain(|line| !line.contains(&marker));
    if before == lines.len() {
        return Ok("Geen service cron entry gevonden om te verwijderen".to_string());
    }
    write_crontab_lines(&lines)?;
    Ok("Service cron entry verwijderd".to_string())
}

fn install_windows_service_task(service_name: &str, command: &str) -> Result<String> {
    let output = Command::new("schtasks")
        .args([
            "/Create",
            "/F",
            "/SC",
            "ONLOGON",
            "/TN",
            service_name,
            "/TR",
            command,
        ])
        .output()
        .context("kan schtasks /Create niet uitvoeren")?;
    if !output.status.success() {
        bail!(
            "schtasks /Create faalde: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(format!("Windows service task installed: {}", service_name))
}

fn windows_service_task_status(service_name: &str) -> Result<ServiceStatus> {
    let output = Command::new("schtasks")
        .args(["/Query", "/TN", service_name, "/V", "/FO", "LIST"])
        .output()
        .context("kan schtasks /Query niet uitvoeren")?;
    if output.status.success() {
        return Ok(ServiceStatus {
            installed: true,
            details: String::from_utf8_lossy(&output.stdout).to_string(),
        });
    }

    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    if stderr.contains("cannot find") {
        return Ok(ServiceStatus {
            installed: false,
            details: "Windows service task niet gevonden".to_string(),
        });
    }
    bail!("schtasks /Query faalde: {}", stderr.trim())
}

fn uninstall_windows_service_task(service_name: &str) -> Result<String> {
    let output = Command::new("schtasks")
        .args(["/Delete", "/F", "/TN", service_name])
        .output()
        .context("kan schtasks /Delete niet uitvoeren")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        if stderr.contains("cannot find") {
            return Ok("Geen Windows service task gevonden om te verwijderen".to_string());
        }
        bail!("schtasks /Delete faalde: {}", stderr.trim());
    }
    Ok("Windows service task verwijderd".to_string())
}

fn systemctl_available() -> bool {
    Command::new("systemctl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn is_root_user() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim() == "0")
        .unwrap_or(false)
}

fn systemd_unit_name(service_name: &str) -> String {
    let normalized = service_name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    format!("{}.service", normalized)
}

fn systemd_unit_path(backend: ServiceBackend, service_name: &str) -> Result<PathBuf> {
    let unit_name = systemd_unit_name(service_name);
    match backend {
        ServiceBackend::LinuxSystemdUser => {
            let home = env::var_os("HOME").context("HOME is niet gezet voor user systemd")?;
            Ok(PathBuf::from(home)
                .join(".config")
                .join("systemd")
                .join("user")
                .join(unit_name))
        }
        ServiceBackend::LinuxSystemdSystem => {
            Ok(PathBuf::from("/etc/systemd/system").join(unit_name))
        }
        _ => bail!("backend heeft geen systemd unit pad"),
    }
}

fn run_systemctl(backend: ServiceBackend, args: &[&str]) -> Result<()> {
    let output = systemctl_output(backend, args)?;
    if !output.status.success() {
        bail!("systemctl faalde: {}", combined_output(&output).trim());
    }
    Ok(())
}

fn systemctl_output(backend: ServiceBackend, args: &[&str]) -> Result<std::process::Output> {
    let mut command = Command::new("systemctl");
    if matches!(backend, ServiceBackend::LinuxSystemdUser) {
        command.arg("--user");
    }
    command
        .args(args)
        .output()
        .context("kan systemctl niet uitvoeren")
}

fn combined_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.trim().is_empty() {
        stdout.to_string()
    } else if stdout.trim().is_empty() {
        stderr.to_string()
    } else {
        format!("{}\n{}", stdout, stderr)
    }
}

fn current_crontab_lines() -> Result<Vec<String>> {
    let output = Command::new("crontab")
        .arg("-l")
        .output()
        .context("kan crontab -l niet uitvoeren")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        if stderr.contains("no crontab") || stderr.contains("geen crontab") {
            return Ok(Vec::new());
        }
        let code = output.status.code().unwrap_or(-1);
        bail!("crontab -l faalde (code={}): {}", code, stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.to_string())
        .collect())
}

fn write_crontab_lines(lines: &[String]) -> Result<()> {
    let mut child = Command::new("crontab")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("kan crontab - niet starten")?;

    {
        let mut stdin = child
            .stdin
            .take()
            .context("kan stdin voor crontab niet openen")?;
        let payload = if lines.is_empty() {
            String::new()
        } else {
            format!("{}\n", lines.join("\n"))
        };
        stdin
            .write_all(payload.as_bytes())
            .context("kan crontab stdin niet schrijven")?;
    }

    let output = child
        .wait_with_output()
        .context("kan crontab - output niet lezen")?;
    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        bail!(
            "crontab - faalde (code={}): {}",
            code,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn service_marker(service_name: &str) -> String {
    format!("sgm-helper-service:{}", service_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    #[test]
    fn builds_service_run_command_with_config_and_intervals() {
        let command = build_service_run_command(
            &PathBuf::from("/opt/sgm/sgm-helper"),
            &PathBuf::from("/opt/sgm/config.ini"),
            15,
            900,
        );
        assert!(command.contains("--config \"/opt/sgm/config.ini\""));
        assert!(command.contains("service run --quiet"));
        assert!(command.contains("--heartbeat-interval 15"));
        assert!(command.contains("--reconcile-interval 900"));
    }

    #[test]
    fn heartbeat_payload_redacts_secret_and_exposes_sensors() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.ini");
        fs::write(
            &config_path,
            "URL=\"127.0.0.1\"\nPORT=\"80\"\nAPP_PASSWORD=\"secret\"\n",
        )
        .unwrap();
        let config = AppConfig {
            url: "127.0.0.1".to_string(),
            port: 80,
            email: "helper@example.com".to_string(),
            app_password: "secret".to_string(),
            root: tmp.path().join("root"),
            state_dir: tmp.path().join("state"),
            watch: false,
            watch_interval: 30,
            force_upload: false,
            dry_run: false,
            route_prefix: String::new(),
            binary_dir: tmp.path().to_path_buf(),
            config_path,
        };
        fs::create_dir_all(config.resolved_state_dir().unwrap()).unwrap();

        let options = ServiceRunOptions {
            heartbeat_interval_secs: 30,
            reconcile_interval_secs: 1800,
            force_upload: false,
            dry_run: false,
            scan: false,
            deep_scan: false,
            apply_scan: false,
            slot_name: "default".to_string(),
            default_source_kind: SourceKind::MisterFpga,
            max_cycles: None,
        };
        let runtime = ServiceRuntimeState::new("2026-04-25T00:00:00Z".to_string());
        let payload = build_heartbeat_payload(
            &config,
            None,
            &SourceKind::MisterFpga,
            &options,
            &runtime,
            "idle",
            12,
            &tmp.path().join("sgm-helper"),
        )
        .unwrap();

        assert_eq!(payload["sensors"]["online"], true);
        assert_eq!(payload["config"]["appPasswordConfigured"], true);
        assert!(payload["sensors"]["configHash"].as_str().unwrap().len() >= 64);
        let serialized = serde_json::to_string(&payload).unwrap();
        assert!(!serialized.contains("secret"));
    }
}
