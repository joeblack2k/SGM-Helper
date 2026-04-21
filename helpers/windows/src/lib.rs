pub mod api;
pub mod cli;
pub mod config;
pub mod scanner;
pub mod state;
pub mod syncer;
pub mod watcher;

use anyhow::{Context, Result, bail};
use clap::Parser;

use crate::api::ApiClient;
use crate::cli::{Cli, Commands, ConfigCommand, StateCommand};
use crate::config::{ConfigOverrides, LoadedConfig};
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
        Commands::Token => {
            let state_dir = loaded.config.resolved_state_dir()?;
            let auth = load_auth_state(&state_dir)?;
            match auth {
                Some(auth) => {
                    let suffix = auth.token_suffix(6);
                    println!(
                        "Token aanwezig: yes\nemail: {}\nbase_url: {}\naangemaakt: {}\ntoken_suffix: {}",
                        auth.email, auth.base_url, auth.created_at, suffix
                    );
                }
                None => {
                    println!("Token aanwezig: no");
                }
            }
        }
        Commands::Sync {
            force_upload,
            dry_run,
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
                    slot_name: "default".to_string(),
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
                    max_cycles,
                },
                cli.verbose,
                cli.quiet,
            )?;
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
        Commands::DeviceAuth => {
            println!(
                "device-auth is not supported in phase 1. Use `login --email ... --app-password ...`."
            );
        }
    }

    Ok(())
}
