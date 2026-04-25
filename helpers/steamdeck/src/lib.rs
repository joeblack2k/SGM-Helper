pub mod api;
pub mod backend_config;
pub mod cli;
pub mod config;
pub mod scanner;
pub mod scheduler;
pub mod service;
pub mod sources;
pub mod state;
pub mod syncer;
pub mod watcher;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;

use crate::api::{ApiClient, AutoProvisionRequest, DeviceTokenPoll};
use crate::cli::{
    Cli, Commands, ConfigCommand, ScheduleCommand, ServiceCommand, SourceAddCommand, SourceCommand,
    StateCommand,
};
use crate::config::{AppConfig, ConfigOverrides, LoadedConfig};
use crate::scanner::{
    SaveContainerFormat, encode_download_for_local_container, normalize_save_bytes_for_sync,
};
use crate::scheduler::{SchedulerBackend, install_schedule, scheduler_status, uninstall_schedule};
use crate::service::{
    ServiceRunOptions, detect_service_backend, install_service, run_service, service_status,
    uninstall_service,
};
use crate::sources::{
    EmulatorProfile, Source, SourceKind, load_source_store, migrate_legacy_sources_if_needed,
    remove_source, resolved_sources_or_default, save_source_store, steamdeck_autodetect_note,
    upsert_source,
};
use crate::state::{
    AuthState, clear_auth_state_for_base_url, load_auth_state, load_auth_state_for_base_url,
    load_sync_state, save_auth_state_for_base_url,
};
use crate::syncer::{SyncOptions, run_sync};
use crate::watcher::{WatchOptions, run_watch};

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    if cli.verbose && cli.quiet {
        bail!("`--verbose` en `--quiet` kunnen niet tegelijk actief zijn");
    }

    let global_overrides = ConfigOverrides {
        url: cli.url.clone(),
        api_url: cli.api_url.clone(),
        port: cli.port,
        email: cli.email.clone(),
        app_password: cli.app_password.clone(),
        root: cli.root.clone(),
        state_dir: cli.state_dir.clone(),
        watch: None,
        watch_interval: None,
        force_upload: None,
        dry_run: None,
        route_prefix: cli.route_prefix.clone(),
    };

    let loaded = LoadedConfig::load(cli.config.clone(), &global_overrides)?;
    dispatch(cli, loaded)
}

fn dispatch(cli: Cli, loaded: LoadedConfig) -> Result<()> {
    match cli.command {
        Commands::Signup {
            email,
            display_name,
            password,
            skip_verification,
        } => {
            let mut cfg = loaded.config.clone();
            if let Some(value) = email {
                cfg.email = value;
            }
            if cfg.email.trim().is_empty() {
                bail!("signup vereist een email (`--email` of EMAIL in config.ini)");
            }

            let inferred_display = cfg
                .email
                .split('@')
                .next()
                .unwrap_or("sgm-user")
                .to_string();
            let display_name = display_name.unwrap_or(inferred_display);
            let password = password
                .or_else(|| {
                    if cfg.app_password.trim().is_empty() {
                        None
                    } else {
                        Some(cfg.app_password.clone())
                    }
                })
                .unwrap_or_else(|| "sgm-helper-password".to_string());

            let api = ApiClient::new(cfg.base_url(), cfg.route_prefix.clone(), None)?;
            let response = api.signup(&cfg.email, &display_name, &password, skip_verification)?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Commands::Login {
            email,
            password,
            app_password,
            device,
        } => {
            let mut cfg = loaded.config.clone();
            if let Some(value) = email {
                cfg.email = value;
            }
            if let Some(value) = app_password {
                cfg.app_password = value;
            }

            let wants_device = device || (password.is_none() && cfg.app_password.trim().is_empty());
            if wants_device {
                run_device_auth(&cfg, 5, cli.quiet)?;
                return Ok(());
            }

            if cfg.email.trim().is_empty() {
                bail!("login vereist een email (`--email` of EMAIL in config.ini)");
            }

            let fingerprint = hostname::get()
                .ok()
                .and_then(|value| value.into_string().ok())
                .unwrap_or_else(|| "helper".to_string());

            let api = ApiClient::new(cfg.base_url(), cfg.route_prefix.clone(), None)?;
            let token_response = if let Some(password) = password {
                let _login = api
                    .login_password(&cfg.email, &password, "steamdeck", &fingerprint)
                    .context("email/password login faalde")?;
                api.mint_token().context("token mint faalde na login")?
            } else {
                if cfg.app_password.trim().is_empty() {
                    bail!("login vereist --password of --app-password (of gebruik --device)");
                }
                api.token_app_password(&cfg.email, &cfg.app_password)
                    .context("app-password login faalde")?
            };

            let auth_me_email = api
                .with_token(Some(token_response.token.clone()))?
                .auth_me()
                .ok()
                .and_then(|user| user.email)
                .unwrap_or_else(|| cfg.email.clone());

            let auth_state = AuthState::new(token_response.token, auth_me_email, cfg.base_url());
            save_auth_state_for_base_url(&cfg.resolved_state_dir()?, &auth_state)?;
            if !cli.quiet {
                println!(
                    "Login successful. Token opgeslagen in {}",
                    cfg.resolved_state_dir()?.join("auth.json").display()
                );
            }
        }
        Commands::Logout => {
            let state_dir = loaded.config.resolved_state_dir()?;
            let base_url = loaded.config.base_url();
            clear_auth_state_for_base_url(&state_dir, &base_url)?;
            if !cli.quiet {
                println!(
                    "Token verwijderd voor {} (state: {})",
                    base_url,
                    state_dir.display()
                );
            }
        }
        Commands::ResendVerification { email } => {
            let mut cfg = loaded.config.clone();
            if let Some(value) = email {
                cfg.email = value;
            }
            if cfg.email.trim().is_empty() {
                bail!("resend-verification vereist een email (`--email` of EMAIL in config.ini)");
            }
            let api = ApiClient::new(cfg.base_url(), cfg.route_prefix.clone(), None)?;
            let response = api.resend_verification(&cfg.email)?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Commands::Token { details } => {
            let auth = load_active_auth_state(&loaded.config)?.context(format!(
                "geen auth-token gevonden voor {}; run eerst `login` of `device-auth`",
                loaded.config.base_url()
            ))?;
            if details {
                println!(
                    "Token aanwezig: yes\nemail: {}\nbase_url: {}\naangemaakt: {}\ntoken_suffix: {}",
                    auth.email,
                    auth.base_url,
                    auth.created_at,
                    auth.token_suffix(6)
                );
            } else {
                println!("{}", auth.token);
            }
        }
        Commands::Sync {
            force_upload,
            dry_run,
            scan,
            deep_scan,
            apply_scan,
            slot_name,
        } => {
            let mut cfg = loaded.config.clone();
            if let Some(value) = force_upload {
                cfg.force_upload = value;
            }
            if let Some(value) = dry_run {
                cfg.dry_run = value;
            }

            print_steamdeck_autodetect_note_if_needed(&cfg, cli.quiet)?;

            let auth =
                ensure_auth_or_auto_enroll(&cfg, cli.verbose, cli.quiet, SourceKind::SteamDeck)?;

            let report = run_sync(
                &cfg,
                auth.as_ref(),
                &SyncOptions {
                    force_upload: cfg.force_upload,
                    dry_run: cfg.dry_run,
                    scan,
                    deep_scan,
                    apply_scan,
                    slot_name: slot_name.unwrap_or_else(|| "default".to_string()),
                    default_source_kind: SourceKind::SteamDeck,
                },
                cli.verbose,
            )?;

            if !cli.quiet {
                println!(
                    "Sync complete: scanned={} uploaded={} downloaded={} in_sync={} conflicts={} skipped={} errors={}",
                    report.scanned,
                    report.uploaded,
                    report.downloaded,
                    report.in_sync,
                    report.conflicts,
                    report.skipped,
                    report.errors
                );
            }
        }
        Commands::Convert {
            input,
            output,
            from,
            to,
        } => {
            let input_bytes = fs::read(&input)
                .with_context(|| format!("kan input bestand niet lezen: {}", input.display()))?;
            let normalized_path = input_path_for_from_override(&input, &from)?;
            let normalized = normalize_save_bytes_for_sync(&normalized_path, "psx", &input_bytes)?
                .context("input is geen geldige PS1 savecontainer (raw/gme/vmp)")?;
            let target_container = parse_ps1_target_container(&to)?;
            let output_bytes =
                encode_download_for_local_container(&normalized.canonical_bytes, target_container)?;

            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("kan output map niet maken: {}", parent.display()))?;
            }
            fs::write(&output, output_bytes).with_context(|| {
                format!("kan output bestand niet schrijven: {}", output.display())
            })?;

            if !cli.quiet {
                println!(
                    "Convert complete: {} -> {}",
                    input.display(),
                    output.display()
                );
            }
        }
        Commands::Watch {
            watch_interval,
            force_upload,
            dry_run,
            scan,
            deep_scan,
            apply_scan,
            slot_name,
            max_cycles,
        } => {
            let mut cfg = loaded.config.clone();
            if let Some(value) = watch_interval {
                cfg.watch_interval = value;
            }
            if let Some(value) = force_upload {
                cfg.force_upload = value;
            }
            if let Some(value) = dry_run {
                cfg.dry_run = value;
            }

            print_steamdeck_autodetect_note_if_needed(&cfg, cli.quiet)?;

            let auth =
                ensure_auth_or_auto_enroll(&cfg, cli.verbose, cli.quiet, SourceKind::SteamDeck)?;

            run_watch(
                &cfg,
                auth.as_ref(),
                WatchOptions {
                    interval_secs: cfg.watch_interval,
                    force_upload: cfg.force_upload,
                    dry_run: cfg.dry_run,
                    scan,
                    deep_scan,
                    apply_scan,
                    slot_name: slot_name.unwrap_or_else(|| "default".to_string()),
                    default_source_kind: SourceKind::SteamDeck,
                    max_cycles,
                },
                cli.verbose,
                cli.quiet,
            )?;
        }
        Commands::Source { command } => {
            migrate_legacy_sources_if_needed(&loaded.config, cli.verbose)?;
            let mut store = load_source_store(&loaded.config.config_path)?;

            match command {
                SourceCommand::List => {
                    if store.sources.is_empty() {
                        println!(
                            "Geen geconfigureerde sources. Fallback: default-steamdeck op ROOT."
                        );
                        if let Some(note) = steamdeck_autodetect_note() {
                            println!("{}", note);
                        }
                    } else {
                        for source in &store.sources {
                            println!(
                                "{} | kind={} | profile={} | recursive={} | saves={} | roms={}",
                                source.name,
                                source.kind.as_str(),
                                source.profile.as_str(),
                                source.recursive,
                                source.save_roots.len(),
                                source.rom_roots.len()
                            );
                        }
                    }
                }
                SourceCommand::Add { source } => {
                    let source = match source {
                        SourceAddCommand::Custom {
                            name,
                            profile,
                            saves,
                            roms,
                            recursive,
                        } => {
                            if saves.is_empty() {
                                bail!("custom source vereist minimaal één --saves pad");
                            }
                            let rom_roots = if roms.is_empty() { saves.clone() } else { roms };
                            let mut source = Source::new(
                                name,
                                SourceKind::Custom,
                                saves,
                                rom_roots,
                                recursive.unwrap_or(true),
                            );
                            if let Some(profile) = profile {
                                source.profile = parse_emulator_profile(&profile)?;
                            }
                            source
                        }
                        SourceAddCommand::MisterFpga {
                            name,
                            profile,
                            root,
                            recursive,
                        } => {
                            let mut source = Source::new(
                                name,
                                SourceKind::MisterFpga,
                                vec![root.join("saves")],
                                vec![root.join("games")],
                                recursive.unwrap_or(true),
                            );
                            if let Some(profile) = profile {
                                source.profile = parse_emulator_profile(&profile)?;
                            }
                            source
                        }
                        SourceAddCommand::Retroarch {
                            name,
                            profile,
                            root,
                            recursive,
                        } => {
                            let mut source = Source::new(
                                name,
                                SourceKind::RetroArch,
                                vec![root.join("saves")],
                                vec![root.join("roms"), root.join("content")],
                                recursive.unwrap_or(true),
                            );
                            if let Some(profile) = profile {
                                source.profile = parse_emulator_profile(&profile)?;
                            }
                            source
                        }
                        SourceAddCommand::Openemu {
                            name,
                            profile,
                            root,
                            recursive,
                        } => {
                            let mut source = Source::new(
                                name,
                                SourceKind::OpenEmu,
                                vec![root.join("Save States")],
                                vec![root],
                                recursive.unwrap_or(true),
                            );
                            if let Some(profile) = profile {
                                source.profile = parse_emulator_profile(&profile)?;
                            }
                            source
                        }
                        SourceAddCommand::AnaloguePocket {
                            name,
                            profile,
                            root,
                            recursive,
                        } => {
                            let mut source = Source::new(
                                name,
                                SourceKind::AnaloguePocket,
                                vec![root.join("Saves"), root.join("saves")],
                                vec![root],
                                recursive.unwrap_or(true),
                            );
                            if let Some(profile) = profile {
                                source.profile = parse_emulator_profile(&profile)?;
                            }
                            source
                        }
                    };

                    upsert_source(&mut store, source.clone());
                    save_source_store(&loaded.config.config_path, &store)?;
                    println!("Source '{}' opgeslagen.", source.name);
                }
                SourceCommand::Remove { name } => {
                    if remove_source(&mut store, &name) {
                        save_source_store(&loaded.config.config_path, &store)?;
                        println!("Source '{}' verwijderd.", name);
                    } else {
                        println!("Source '{}' niet gevonden.", name);
                    }
                }
            }
        }
        Commands::State { command } => {
            let state_dir = loaded.config.resolved_state_dir()?;
            match command {
                StateCommand::List => {
                    let state = load_sync_state(&state_dir)?;
                    if state.entries.is_empty() {
                        println!("Geen sync-state entries.");
                    } else {
                        for (path, entry) in state.entries {
                            let source_label =
                                match (entry.source_kind.clone(), entry.source_name.clone()) {
                                    (Some(kind), Some(name)) => format!("{} ({})", kind, name),
                                    (Some(kind), None) => kind,
                                    (None, Some(name)) => name,
                                    (None, None) => "-".to_string(),
                                };
                            println!(
                                "{} | sha256={} | rom_sha1={} | version={} | system={} | container={} | adapter={} | source={}",
                                path,
                                entry.sha256,
                                entry.rom_sha1.unwrap_or_else(|| "-".to_string()),
                                entry
                                    .version
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "-".to_string()),
                                entry.system_slug.unwrap_or_else(|| "-".to_string()),
                                entry
                                    .local_container
                                    .map(|value| value.as_str().to_string())
                                    .unwrap_or_else(|| "-".to_string()),
                                entry
                                    .adapter_profile
                                    .map(|value| value.as_str().to_string())
                                    .unwrap_or_else(|| "-".to_string()),
                                source_label,
                            );
                        }
                    }
                }
                StateCommand::Clean { missing, all } => {
                    let mut state = load_sync_state(&state_dir)?;
                    let before = state.entries.len();
                    if all {
                        state.entries.clear();
                    } else {
                        let remove_missing = missing || !all;
                        if remove_missing {
                            state
                                .entries
                                .retain(|path, _| std::path::Path::new(path).exists());
                        }
                    }
                    let removed = before.saturating_sub(state.entries.len());
                    crate::state::save_sync_state(&state_dir, &state)?;
                    println!("State cleaned. Removed {} missing entries.", removed);
                }
            }
        }
        Commands::Config { command } => match command {
            ConfigCommand::Show => {
                let mut redacted = loaded.config.clone();
                if !redacted.app_password.is_empty() {
                    redacted.app_password = "***redacted***".to_string();
                }
                let state_path = redacted.resolved_state_dir()?;
                let payload = serde_json::json!({
                    "config_path": loaded.config_path,
                    "resolved": {
                        "url": redacted.url,
                        "port": redacted.port,
                        "base_url": redacted.base_url(),
                        "email": redacted.email,
                        "app_password": redacted.app_password,
                        "root": redacted.resolved_root()?,
                        "state_dir": state_path,
                        "watch": redacted.watch,
                        "watch_interval": redacted.watch_interval,
                        "force_upload": redacted.force_upload,
                        "dry_run": redacted.dry_run,
                        "route_prefix": redacted.route_prefix,
                    }
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            }
        },
        Commands::Schedule { command } => {
            let task_name = "SGM SteamDeck Helper Sync";
            let binary_path =
                std::env::current_exe().context("kan executable pad niet bepalen voor schedule")?;
            let config_path = loaded.config.config_path.clone();
            let backend = SchedulerBackend::LinuxCron;

            match command {
                ScheduleCommand::Install { every_minutes } => {
                    let result = install_schedule(
                        backend,
                        task_name,
                        &binary_path,
                        &config_path,
                        every_minutes,
                    )?;
                    if !cli.quiet {
                        println!("{}", result);
                    }
                }
                ScheduleCommand::Status => {
                    let status = scheduler_status(backend, task_name, &binary_path, &config_path)?;
                    if !cli.quiet {
                        println!(
                            "Installed: {}\nDetails: {}",
                            if status.installed { "yes" } else { "no" },
                            status.details.trim()
                        );
                    }
                }
                ScheduleCommand::Uninstall => {
                    let result = uninstall_schedule(backend, task_name)?;
                    if !cli.quiet {
                        println!("{}", result);
                    }
                }
            }
        }
        Commands::Service { command } => {
            let service_name = "SGM SteamDeck Helper Service";
            let binary_path =
                std::env::current_exe().context("kan executable pad niet bepalen voor service")?;
            let config_path = loaded.config.config_path.clone();
            let backend = detect_service_backend();

            match command {
                ServiceCommand::Run {
                    heartbeat_interval,
                    reconcile_interval,
                    force_upload,
                    dry_run,
                    scan,
                    deep_scan,
                    apply_scan,
                    slot_name,
                    max_cycles,
                } => {
                    let mut cfg = loaded.config.clone();
                    if let Some(value) = force_upload {
                        cfg.force_upload = value;
                    }
                    if let Some(value) = dry_run {
                        cfg.dry_run = value;
                    }

                    print_steamdeck_autodetect_note_if_needed(&cfg, cli.quiet)?;

                    let auth = ensure_auth_or_auto_enroll(
                        &cfg,
                        cli.verbose,
                        cli.quiet,
                        SourceKind::SteamDeck,
                    )?;
                    run_service(
                        &cfg,
                        auth.as_ref(),
                        ServiceRunOptions {
                            heartbeat_interval_secs: heartbeat_interval,
                            reconcile_interval_secs: reconcile_interval,
                            force_upload: cfg.force_upload,
                            dry_run: cfg.dry_run,
                            scan,
                            deep_scan,
                            apply_scan,
                            slot_name: slot_name.unwrap_or_else(|| "default".to_string()),
                            default_source_kind: SourceKind::SteamDeck,
                            max_cycles,
                        },
                        cli.verbose,
                        cli.quiet,
                    )?;
                }
                ServiceCommand::Install {
                    heartbeat_interval,
                    reconcile_interval,
                } => {
                    let result = install_service(
                        backend,
                        service_name,
                        &binary_path,
                        &config_path,
                        heartbeat_interval,
                        reconcile_interval,
                    )?;
                    if !cli.quiet {
                        println!("{}", result);
                    }
                }
                ServiceCommand::Status => {
                    let status = service_status(backend, service_name, &binary_path, &config_path)?;
                    if !cli.quiet {
                        println!(
                            "Installed: {}\nDetails: {}",
                            if status.installed { "yes" } else { "no" },
                            status.details.trim()
                        );
                    }
                }
                ServiceCommand::Uninstall => {
                    let result = uninstall_service(backend, service_name)?;
                    if !cli.quiet {
                        println!("{}", result);
                    }
                }
            }
        }
        Commands::DeviceAuth { poll_interval } => {
            let cfg = loaded.config.clone();
            run_device_auth(&cfg, poll_interval, cli.quiet)?;
        }
    }

    Ok(())
}

fn parse_emulator_profile(value: &str) -> Result<EmulatorProfile> {
    EmulatorProfile::parse(value).ok_or_else(|| anyhow!("ongeldig emulatorprofiel '{}'", value))
}

fn load_active_auth_state(cfg: &AppConfig) -> Result<Option<AuthState>> {
    let state_dir = cfg.resolved_state_dir()?;
    if let Some(auth) = load_auth_state_for_base_url(&state_dir, &cfg.base_url())? {
        return Ok(Some(auth));
    }
    load_auth_state(&state_dir)
}

fn ensure_auth_or_auto_enroll(
    cfg: &AppConfig,
    verbose: bool,
    quiet: bool,
    default_source_kind: SourceKind,
) -> Result<Option<AuthState>> {
    if let Some(auth) = load_active_auth_state(cfg)? {
        return Ok(Some(auth));
    }

    let state_dir = cfg.resolved_state_dir()?;
    fs::create_dir_all(&state_dir)
        .with_context(|| format!("kan state map niet maken: {}", state_dir.display()))?;

    let api = ApiClient::new(cfg.base_url(), cfg.route_prefix.clone(), None)?;
    let gate_status = match api.auto_enroll_status() {
        Ok(value) => value,
        Err(err) => {
            if verbose {
                eprintln!("Auto-enroll gate check mislukt: {}", err);
            }
            bail!(
                "geen auth-token gevonden voor {}; klik in de backend op 'Add helper' en probeer opnieuw, of gebruik `login`",
                cfg.base_url()
            );
        }
    };

    if !gate_status.active {
        bail!(
            "geen auth-token gevonden voor {}; klik in de backend op 'Add helper' en probeer opnieuw, of gebruik `login`",
            cfg.base_url()
        );
    }

    let hostname = helper_hostname();
    let fingerprint = hostname.clone();
    let sync_paths = collect_auto_enroll_sync_paths(cfg, default_source_kind.clone())?;
    let request = AutoProvisionRequest {
        name: format!(
            "{} ({})",
            helper_display_name(&default_source_kind),
            hostname
        ),
        device_type: default_source_kind.helper_device_type().to_string(),
        fingerprint,
        hostname,
        helper_name: env!("CARGO_PKG_NAME").to_string(),
        helper_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: helper_platform_name(&default_source_kind).to_string(),
        sync_paths,
        systems: Vec::new(),
    };

    let token_response = api
        .token_app_password_auto_provision(&request)
        .context("auto-enroll provisioning faalde")?;
    let auth_email = if cfg.email.trim().is_empty() {
        format!("{}@local", default_source_kind.as_str())
    } else {
        cfg.email.clone()
    };

    let auth_state = AuthState::new(token_response.token, auth_email, cfg.base_url());
    save_auth_state_for_base_url(&state_dir, &auth_state)?;

    if !quiet {
        println!(
            "Auto-enroll succesvol. Helper geregistreerd en token opgeslagen in {}",
            state_dir.join("auth.json").display()
        );
    } else if verbose {
        eprintln!("Auto-enroll succesvol; token opgeslagen.");
    }

    Ok(Some(auth_state))
}

fn collect_auto_enroll_sync_paths(
    cfg: &AppConfig,
    default_source_kind: SourceKind,
) -> Result<Vec<String>> {
    migrate_legacy_sources_if_needed(cfg, false)?;
    let store = load_source_store(&cfg.config_path)?;
    let sources = resolved_sources_or_default(&store, cfg, default_source_kind)?;
    let mut paths = BTreeSet::new();
    for source in sources {
        for root in source.save_roots {
            paths.insert(root.display().to_string());
        }
    }
    Ok(paths.into_iter().collect())
}

fn helper_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| "helper".to_string())
}

fn helper_display_name(kind: &SourceKind) -> &'static str {
    match kind {
        SourceKind::MisterFpga => "SGM MiSTer Helper",
        SourceKind::SteamDeck => "SGM SteamDeck Helper",
        SourceKind::Windows => "SGM Windows Helper",
        SourceKind::RetroArch => "SGM RetroArch Helper",
        SourceKind::OpenEmu => "SGM OpenEmu Helper",
        SourceKind::AnaloguePocket => "SGM Analogue Pocket Helper",
        SourceKind::Custom => "SGM Custom Helper",
    }
}

fn helper_platform_name(kind: &SourceKind) -> &'static str {
    match kind {
        SourceKind::MisterFpga => "MiSTer",
        SourceKind::SteamDeck => "SteamDeck",
        SourceKind::Windows => "Windows",
        SourceKind::RetroArch => "RetroArch",
        SourceKind::OpenEmu => "OpenEmu",
        SourceKind::AnaloguePocket => "Analogue Pocket",
        SourceKind::Custom => "Custom",
    }
}

fn print_steamdeck_autodetect_note_if_needed(cfg: &AppConfig, quiet: bool) -> Result<()> {
    if quiet {
        return Ok(());
    }

    let store = load_source_store(&cfg.config_path)?;
    if store.sources.is_empty()
        && let Some(note) = steamdeck_autodetect_note()
    {
        println!("{}", note);
    }

    Ok(())
}

fn run_device_auth(cfg: &AppConfig, poll_interval: u64, quiet: bool) -> Result<()> {
    let api = ApiClient::new(cfg.base_url(), cfg.route_prefix.clone(), None)?;
    let device = api.start_device_auth()?;

    if !quiet {
        println!("Device authorization started.");
        println!("Open: {}", device.verification_uri);
        println!("Code: {}", device.user_code);
    }

    let poll_interval = poll_interval.max(1);
    let max_attempts = (device.expires_in_seconds / poll_interval).max(1);

    for _ in 0..max_attempts {
        match api.poll_device_token(&device.device_code)? {
            DeviceTokenPoll::Pending => {
                thread::sleep(Duration::from_secs(poll_interval));
            }
            DeviceTokenPoll::Success(token_response) => {
                let auth_me_email = api
                    .with_token(Some(token_response.token.clone()))?
                    .auth_me()
                    .ok()
                    .and_then(|user| user.email)
                    .unwrap_or_else(|| "device-auth@local".to_string());
                let auth_state =
                    AuthState::new(token_response.token, auth_me_email, cfg.base_url());
                save_auth_state_for_base_url(&cfg.resolved_state_dir()?, &auth_state)?;
                if !quiet {
                    println!("Device login successful.");
                }
                return Ok(());
            }
        }
    }

    bail!("Device authorization timed out.");
}

fn input_path_for_from_override(input: &Path, from: &str) -> Result<PathBuf> {
    let normalized = from.trim().to_ascii_lowercase();
    let mut path = input.to_path_buf();

    let ext = match normalized.as_str() {
        "auto" => return Ok(path),
        "raw" | "ps1-raw" => "mcr",
        "gme" | "ps1-gme" | "dexdrive" => "gme",
        "vmp" | "ps1-vmp" => "vmp",
        _ => {
            bail!(
                "onbekende --from waarde '{}'; gebruik auto, raw, gme of vmp",
                from
            );
        }
    };
    path.set_extension(ext);
    Ok(path)
}

fn parse_ps1_target_container(to: &str) -> Result<SaveContainerFormat> {
    let normalized = to.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "raw" | "ps1-raw" => Ok(SaveContainerFormat::Ps1Raw),
        "gme" | "ps1-gme" | "dexdrive" => Ok(SaveContainerFormat::Ps1DexDrive),
        "vmp" | "ps1-vmp" => Ok(SaveContainerFormat::Ps1Vmp),
        _ => bail!("onbekende --to waarde '{}'; gebruik raw, gme of vmp", to),
    }
}
