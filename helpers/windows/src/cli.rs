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

    #[arg(long = "api-url", global = true)]
    pub api_url: Option<String>,

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
    Signup {
        #[arg(long)]
        email: Option<String>,
        #[arg(long = "display-name")]
        display_name: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long, action = ArgAction::SetTrue)]
        skip_verification: bool,
    },
    Login {
        #[arg(long)]
        email: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long = "app-password")]
        app_password: Option<String>,
        #[arg(long, action = ArgAction::SetTrue)]
        device: bool,
    },
    ResendVerification {
        #[arg(long)]
        email: Option<String>,
    },
    Logout,
    Token {
        #[arg(long, action = ArgAction::SetTrue)]
        details: bool,
    },
    Sync {
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
        force_upload: Option<bool>,
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
        dry_run: Option<bool>,
        #[arg(long, action = ArgAction::SetTrue)]
        scan: bool,
        #[arg(long, action = ArgAction::SetTrue)]
        deep_scan: bool,
        #[arg(long, action = ArgAction::SetTrue)]
        apply_scan: bool,
        #[arg(long = "slot-name")]
        slot_name: Option<String>,
    },
    Convert {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "auto")]
        from: String,
        #[arg(long)]
        to: String,
    },
    Watch {
        #[arg(long = "watch-interval")]
        watch_interval: Option<u64>,
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
        force_upload: Option<bool>,
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
        dry_run: Option<bool>,
        #[arg(long, action = ArgAction::SetTrue)]
        scan: bool,
        #[arg(long, action = ArgAction::SetTrue)]
        deep_scan: bool,
        #[arg(long, action = ArgAction::SetTrue)]
        apply_scan: bool,
        #[arg(long = "slot-name")]
        slot_name: Option<String>,
        #[arg(long, hide = true)]
        max_cycles: Option<u32>,
    },
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },
    State {
        #[command(subcommand)]
        command: StateCommand,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
    DeviceAuth {
        #[arg(long = "poll-interval", default_value_t = 5)]
        poll_interval: u64,
    },
}

#[derive(Debug, Subcommand)]
pub enum SourceCommand {
    List,
    Add {
        #[command(subcommand)]
        source: SourceAddCommand,
    },
    Remove {
        #[arg(long)]
        name: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum SourceAddCommand {
    Custom {
        #[arg(long)]
        name: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long = "saves", required = true)]
        saves: Vec<PathBuf>,
        #[arg(long = "roms")]
        roms: Vec<PathBuf>,
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
        recursive: Option<bool>,
    },
    MisterFpga {
        #[arg(long)]
        name: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        root: PathBuf,
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
        recursive: Option<bool>,
    },
    Retroarch {
        #[arg(long)]
        name: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        root: PathBuf,
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
        recursive: Option<bool>,
    },
    Openemu {
        #[arg(long)]
        name: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        root: PathBuf,
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
        recursive: Option<bool>,
    },
    AnaloguePocket {
        #[arg(long)]
        name: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        root: PathBuf,
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
        recursive: Option<bool>,
    },
}

#[derive(Debug, Subcommand)]
pub enum StateCommand {
    List,
    Clean {
        #[arg(long, action = ArgAction::SetTrue)]
        missing: bool,
        #[arg(long, action = ArgAction::SetTrue)]
        all: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Show,
}

#[derive(Debug, Subcommand)]
pub enum ScheduleCommand {
    Install {
        #[arg(long = "every-minutes", default_value_t = 30)]
        every_minutes: u32,
    },
    Status,
    Uninstall,
}
