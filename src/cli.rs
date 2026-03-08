use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::events::MessageFormat;

#[derive(Debug, Parser)]
#[command(
    name = "clawhip",
    version,
    about = "Daemon-first event gateway for Discord"
)]
pub struct Cli {
    /// Override the config file path.
    #[arg(long, global = true, env = "CLAWHIP_CONFIG")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

impl Cli {
    pub fn config_path(&self) -> PathBuf {
        self.config
            .clone()
            .unwrap_or_else(crate::config::default_config_path)
    }
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Start the daemon (HTTP server + git/tmux monitors).
    #[command(alias = "serve")]
    Start {
        #[arg(long)]
        port: Option<u16>,
    },
    /// Check daemon health/status.
    Status,
    /// Send a custom event to the local daemon.
    Send {
        #[arg(long)]
        channel: Option<String>,
        #[arg(long)]
        message: String,
    },
    /// Send git-related events to the local daemon.
    Git {
        #[command(subcommand)]
        command: GitCommands,
    },
    /// Send GitHub-related events to the local daemon.
    Github {
        #[command(subcommand)]
        command: GithubCommands,
    },
    /// Send tmux-related events to the local daemon or launch/register tmux sessions.
    Tmux {
        #[command(subcommand)]
        command: TmuxCommands,
    },
    /// Install clawhip from the current git clone.
    Install {
        #[arg(long, default_value_t = false)]
        systemd: bool,
    },
    /// Update clawhip from the current git clone.
    Update {
        #[arg(long, default_value_t = false)]
        restart: bool,
    },
    /// Uninstall clawhip.
    Uninstall {
        #[arg(long, default_value_t = false)]
        remove_systemd: bool,
        #[arg(long, default_value_t = false)]
        remove_config: bool,
    },
    /// Manage configuration.
    Config {
        #[command(subcommand)]
        command: Option<ConfigCommand>,
    },
}

#[derive(Debug, Subcommand)]
pub enum GitCommands {
    Commit {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        branch: String,
        #[arg(long)]
        commit: String,
        #[arg(long)]
        summary: String,
        #[arg(long)]
        channel: Option<String>,
    },
    BranchChanged {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        old_branch: String,
        #[arg(long)]
        new_branch: String,
        #[arg(long)]
        channel: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum GithubCommands {
    IssueOpened {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        number: u64,
        #[arg(long)]
        title: String,
        #[arg(long)]
        channel: Option<String>,
    },
    PrStatusChanged {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        number: u64,
        #[arg(long)]
        title: String,
        #[arg(long)]
        old_status: String,
        #[arg(long)]
        new_status: String,
        #[arg(long, default_value = "")]
        url: String,
        #[arg(long)]
        channel: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum TmuxCommands {
    Keyword {
        #[arg(long)]
        session: String,
        #[arg(long)]
        keyword: String,
        #[arg(long)]
        line: String,
        #[arg(long)]
        channel: Option<String>,
    },
    Stale {
        #[arg(long)]
        session: String,
        #[arg(long)]
        pane: String,
        #[arg(long)]
        minutes: u64,
        #[arg(long)]
        last_line: String,
        #[arg(long)]
        channel: Option<String>,
    },
    New(TmuxNewArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TmuxWrapperFormat {
    Compact,
    Alert,
    Inline,
}

impl From<TmuxWrapperFormat> for MessageFormat {
    fn from(value: TmuxWrapperFormat) -> Self {
        match value {
            TmuxWrapperFormat::Compact => MessageFormat::Compact,
            TmuxWrapperFormat::Alert => MessageFormat::Alert,
            TmuxWrapperFormat::Inline => MessageFormat::Inline,
        }
    }
}

#[derive(Debug, Clone, Args)]
pub struct TmuxNewArgs {
    #[arg(short = 's', long = "session")]
    pub session: String,
    #[arg(short = 'n', long = "window-name")]
    pub window_name: Option<String>,
    #[arg(short = 'c', long = "cwd")]
    pub cwd: Option<String>,
    #[arg(long)]
    pub channel: Option<String>,
    #[arg(long)]
    pub mention: Option<String>,
    #[arg(long, value_delimiter = ',')]
    pub keywords: Vec<String>,
    #[arg(long, default_value_t = 10)]
    pub stale_minutes: u64,
    #[arg(long)]
    pub format: Option<TmuxWrapperFormat>,
    #[arg(long, default_value_t = false)]
    pub attach: bool,
    #[arg(long)]
    pub shell: Option<String>,
    #[arg(last = true, allow_hyphen_values = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Default, Subcommand)]
pub enum ConfigCommand {
    #[default]
    Interactive,
    Show,
    Path,
}
