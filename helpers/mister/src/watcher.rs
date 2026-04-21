use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::Result;

use crate::api::ApiClient;
use crate::config::AppConfig;
use crate::sources::SourceKind;
use crate::state::AuthState;
use crate::syncer::{SyncOptions, run_sync};

#[derive(Debug, Clone)]
pub struct WatchOptions {
    pub interval_secs: u64,
    pub force_upload: bool,
    pub dry_run: bool,
    pub scan: bool,
    pub deep_scan: bool,
    pub apply_scan: bool,
    pub slot_name: String,
    pub default_source_kind: SourceKind,
    pub max_cycles: Option<u32>,
}

#[derive(Debug)]
enum WatchSignal {
    SseEvent(String),
}

pub fn run_watch(
    config: &AppConfig,
    auth: Option<&AuthState>,
    options: WatchOptions,
    verbose: bool,
    quiet: bool,
) -> Result<()> {
    let interval_secs = options.interval_secs.max(1);
    let mut scan_once = options.scan;
    let mut deep_scan_once = options.deep_scan;
    let mut apply_scan_once = options.apply_scan;
    let stop_flag = Arc::new(AtomicBool::new(false));

    {
        let stop_flag = Arc::clone(&stop_flag);
        ctrlc::set_handler(move || {
            stop_flag.store(true, Ordering::SeqCst);
        })?;
    }

    let (tx, rx) = mpsc::channel::<WatchSignal>();
    if let Some(auth) = auth {
        let api = ApiClient::new(
            config.base_url(),
            config.route_prefix.clone(),
            Some(auth.token.clone()),
        )?;
        spawn_sse_listener(api, tx, Arc::clone(&stop_flag), verbose);
    }

    let mut cycles: u32 = 0;
    while !stop_flag.load(Ordering::SeqCst) {
        let report = run_sync(
            config,
            auth,
            &SyncOptions {
                force_upload: options.force_upload,
                dry_run: options.dry_run,
                scan: scan_once,
                deep_scan: deep_scan_once,
                apply_scan: apply_scan_once,
                slot_name: options.slot_name.clone(),
                default_source_kind: options.default_source_kind.clone(),
            },
            verbose,
        )?;

        scan_once = false;
        deep_scan_once = false;
        apply_scan_once = false;

        cycles += 1;
        if !quiet {
            println!(
                "Watch cycle {} complete: scanned={} uploaded={} downloaded={} in_sync={} conflicts={} skipped={} errors={}",
                cycles,
                report.scanned,
                report.uploaded,
                report.downloaded,
                report.in_sync,
                report.conflicts,
                report.skipped,
                report.errors
            );
        }

        if let Some(max_cycles) = options.max_cycles
            && cycles >= max_cycles
        {
            break;
        }

        match rx.recv_timeout(Duration::from_secs(interval_secs)) {
            Ok(WatchSignal::SseEvent(event_name)) => {
                if verbose {
                    eprintln!("Remote change detected via SSE event: {}", event_name);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if verbose {
                    eprintln!("SSE channel disconnected; continuing with polling");
                }
            }
        }
    }

    stop_flag.store(true, Ordering::SeqCst);
    Ok(())
}

fn spawn_sse_listener(
    api: ApiClient,
    tx: mpsc::Sender<WatchSignal>,
    stop_flag: Arc<AtomicBool>,
    verbose: bool,
) {
    thread::spawn(move || {
        while !stop_flag.load(Ordering::SeqCst) {
            let response = match api.open_events() {
                Ok(response) => response,
                Err(err) => {
                    if verbose {
                        eprintln!("Failed to connect SSE, fallback polling active: {}", err);
                    }
                    thread::sleep(Duration::from_secs(5));
                    continue;
                }
            };

            let mut current_event: Option<String> = None;
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
                    let event_name = trimmed.trim_start_matches("event:").trim().to_string();
                    current_event = Some(event_name);
                    continue;
                }
                if trimmed.is_empty()
                    && let Some(event_name) = current_event.take()
                    && is_sync_event(&event_name)
                {
                    let _ = tx.send(WatchSignal::SseEvent(event_name));
                }
            }

            thread::sleep(Duration::from_secs(1));
        }
    });
}

fn is_sync_event(event_name: &str) -> bool {
    matches!(
        event_name,
        "save_created" | "save_parsed" | "save_deleted" | "conflict_created" | "conflict_resolved"
    )
}
