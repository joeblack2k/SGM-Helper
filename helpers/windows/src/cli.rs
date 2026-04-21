use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "sgm-windows-helper",
    about = "Windows save sync helper for SGM self-hosted backends",
    version
)]
pub struct Cli {
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[arg(long, global = true)]
    pub url: Option<String>,

    #[arg(long, global = true)]
    pub port: Option<u16>,

    #[arg(long, global = true)]
    pub email: Option<String>,

    #[arg(long = "app-password", global = true)]
    pub app_password: Option<String>,

    #[arg(long, global = true)]
    pub root: Option<PathBuf>,

    #[arg(long = "state-dir", global = true)]
    pub state_dir: Option<PathBuf>,

    #[arg(long = "route-prefix", global = true)]
    pub route_prefix: Option<String>,

    #[arg(long, action = ArgAction::SetTrue, global = true)]
    pub verbose: bool,

    #[arg(long, action = ArgAction::SetTrue, global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    Login {
        #[arg(long)]
        email: Option<String>,
        #[arg(long = "app-password")]
        app_password: Option<String>,
    },
    Logout,
    Token,
    Sync {
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
        force_upload: Option<bool>,
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
        dry_run: Option<bool>,
    },
    Watch {
        #[arg(long = "watch-interval")]
        watch_interval: Option<u64>,
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
        force_upload: Option<bool>,
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
        dry_run: Option<bool>,
        #[arg(long, hide = true)]
        max_cycles: Option<u32>,
    },
    State {
        #[command(subcommand)]
        command: StateCommand,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    DeviceAuth,
}

#[derive(Debug, Subcommand)]
pub enum StateCommand {
    List,
    Clean,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Show,
}
