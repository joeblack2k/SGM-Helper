pub mod api;
pub mod cli;
pub mod config;
pub mod scanner;
pub mod sources;
pub mod state;
pub mod syncer;
pub mod watcher;

use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;

use crate::api::{ApiClient, DeviceTokenPoll};
use crate::cli::{Cli, Commands, ConfigCommand, SourceAddCommand, SourceCommand, StateCommand};
use crate::config::{ConfigOverrides, LoadedConfig};
use crate::sources::{
    Source, SourceKind, load_source_store, remove_source, save_source_store, upsert_source,
};
use crate::state::{
    AuthState, clear_auth_state, load_auth_state, load_sync_state, save_auth_state,
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
        Commands::Login {
            email,
            app_password,
        } => {
            let mut cfg = loaded.config.clone();
            if let Some(value) = email {
                cfg.email = value;
            }
            if let Some(value) = app_password {
                cfg.app_password = value;
            }

            if cfg.email.trim().is_empty() {
                bail!("login vereist een email (`--email` of EMAIL in config.ini)");
            }
            if cfg.app_password.trim().is_empty() {
                bail!(
                    "login vereist een app-password (`--app-password` of APP_PASSWORD in config.ini)"
                );
            }

            let api = ApiClient::new(cfg.base_url(), cfg.route_prefix.clone(), None)?;
            let token_response = api
                .token_app_password(&cfg.email, &cfg.app_password)
                .context("app-password login faalde")?;
            let auth_me_email = api
                .auth_me()
                .ok()
                .and_then(|user| user.email)
                .unwrap_or_else(|| cfg.email.clone());

            let auth_state = AuthState::new(token_response.token, auth_me_email, cfg.base_url());
            save_auth_state(&cfg.resolved_state_dir()?, &auth_state)?;
            if !cli.quiet {
                println!(
                    "Login successful. Token opgeslagen in {}",
                    cfg.resolved_state_dir()?.join("auth.json").display()
                );
            }
        }
        Commands::Logout => {
            let state_dir = loaded.config.resolved_state_dir()?;
            clear_auth_state(&state_dir)?;
            if !cli.quiet {
                println!(
                    "Token verwijderd uit {}",
                    state_dir.join("auth.json").display()
                );
            }
        }
        Commands::Token { details } => {
            let state_dir = loaded.config.resolved_state_dir()?;
            let auth = load_auth_state(&state_dir)?
                .context("geen auth.json gevonden; run eerst `login` of `device-auth`")?;
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
            slot_name,
        } => {
            let mut cfg = loaded.config.clone();
            if let Some(value) = force_upload {
                cfg.force_upload = value;
            }
            if let Some(value) = dry_run {
                cfg.dry_run = value;
            }

            let auth = load_auth_state(&cfg.resolved_state_dir()?)?
                .context("geen auth.json gevonden; run eerst `login` met app-password")?;

            let report = run_sync(
                &cfg,
                Some(&auth),
                &SyncOptions {
                    force_upload: cfg.force_upload,
                    dry_run: cfg.dry_run,
                    slot_name: slot_name.unwrap_or_else(|| "default".to_string()),
                    default_source_kind: SourceKind::Windows,
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
        Commands::Watch {
            watch_interval,
            force_upload,
            dry_run,
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

            let auth = load_auth_state(&cfg.resolved_state_dir()?)?
                .context("geen auth.json gevonden; run eerst `login` met app-password")?;

            run_watch(
                &cfg,
                Some(&auth),
                WatchOptions {
                    interval_secs: cfg.watch_interval,
                    force_upload: cfg.force_upload,
                    dry_run: cfg.dry_run,
                    slot_name: slot_name.unwrap_or_else(|| "default".to_string()),
                    default_source_kind: SourceKind::Windows,
                    max_cycles,
                },
                cli.verbose,
                cli.quiet,
            )?;
        }
        Commands::Source { command } => {
            let state_dir = loaded.config.resolved_state_dir()?;
            let mut store = load_source_store(&state_dir)?;

            match command {
                SourceCommand::List => {
                    if store.sources.is_empty() {
                        println!(
                            "Geen geconfigureerde sources. Fallback: default-windows op ROOT."
                        );
                    } else {
                        for source in &store.sources {
                            println!(
                                "{} | kind={} | recursive={} | saves={} | roms={}",
                                source.name,
                                source.kind.as_str(),
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
                            saves,
                            roms,
                            recursive,
                        } => {
                            if saves.is_empty() {
                                bail!("custom source vereist minimaal één --saves pad");
                            }
                            let rom_roots = if roms.is_empty() { saves.clone() } else { roms };
                            Source::new(
                                name,
                                SourceKind::Custom,
                                saves,
                                rom_roots,
                                recursive.unwrap_or(true),
                            )
                        }
                        SourceAddCommand::MisterFpga {
                            name,
                            root,
                            recursive,
                        } => Source::new(
                            name,
                            SourceKind::MisterFpga,
                            vec![root.join("saves")],
                            vec![root.join("games")],
                            recursive.unwrap_or(true),
                        ),
                        SourceAddCommand::Retroarch {
                            name,
                            root,
                            recursive,
                        } => Source::new(
                            name,
                            SourceKind::RetroArch,
                            vec![root.join("saves")],
                            vec![root.join("roms"), root.join("content")],
                            recursive.unwrap_or(true),
                        ),
                        SourceAddCommand::Openemu {
                            name,
                            root,
                            recursive,
                        } => Source::new(
                            name,
                            SourceKind::OpenEmu,
                            vec![root.join("Save States")],
                            vec![root],
                            recursive.unwrap_or(true),
                        ),
                        SourceAddCommand::AnaloguePocket {
                            name,
                            root,
                            recursive,
                        } => Source::new(
                            name,
                            SourceKind::AnaloguePocket,
                            vec![root.join("Saves"), root.join("saves")],
                            vec![root],
                            recursive.unwrap_or(true),
                        ),
                    };

                    upsert_source(&mut store, source.clone());
                    save_source_store(&state_dir, &store)?;
                    println!("Source '{}' opgeslagen.", source.name);
                }
                SourceCommand::Remove { name } => {
                    if remove_source(&mut store, &name) {
                        save_source_store(&state_dir, &store)?;
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
                            println!(
                                "{} | sha256={} | rom_sha1={} | version={}",
                                path,
                                entry.sha256,
                                entry.rom_sha1.unwrap_or_else(|| "-".to_string()),
                                entry
                                    .version
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "-".to_string())
                            );
                        }
                    }
                }
                StateCommand::Clean => {
                    let mut state = load_sync_state(&state_dir)?;
                    let before = state.entries.len();
                    state
                        .entries
                        .retain(|path, _| std::path::Path::new(path).exists());
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
        Commands::DeviceAuth { poll_interval } => {
            let cfg = loaded.config.clone();
            let api = ApiClient::new(cfg.base_url(), cfg.route_prefix.clone(), None)?;
            let device = api.start_device_auth()?;

            println!("Device authorization started.");
            println!("Open: {}", device.verification_uri);
            println!("Code: {}", device.user_code);

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
                        save_auth_state(&cfg.resolved_state_dir()?, &auth_state)?;
                        println!("Device login successful.");
                        return Ok(());
                    }
                }
            }

            bail!("Device authorization timed out.");
        }
    }

    Ok(())
}
