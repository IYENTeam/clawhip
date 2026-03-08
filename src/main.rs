mod cli;
mod client;
mod config;
mod daemon;
mod discord;
mod dynamic_tokens;
mod events;
mod lifecycle;
mod monitor;
mod router;
mod tmux_wrapper;

use std::sync::Arc;

use clap::Parser;

use crate::cli::{Cli, Commands, ConfigCommand, GitCommands, GithubCommands, TmuxCommands};
use crate::client::DaemonClient;
use crate::config::AppConfig;
use crate::events::IncomingEvent;

pub type DynError = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, DynError>;

#[tokio::main]
async fn main() {
    if let Err(error) = real_main().await {
        eprintln!("clawhip error: {error}");
        std::process::exit(1);
    }
}

async fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config_path();
    let config = Arc::new(AppConfig::load_or_default(&config_path)?);

    match cli.command.unwrap_or(Commands::Start { port: None }) {
        Commands::Start { port } => daemon::run(config, port).await,
        Commands::Status => {
            let client = DaemonClient::from_config(config.as_ref());
            let health = client.health().await?;
            println!("{}", serde_json::to_string_pretty(&health)?);
            Ok(())
        }
        Commands::Send { channel, message } => {
            let client = DaemonClient::from_config(config.as_ref());
            client
                .send_event(&IncomingEvent::custom(channel, message))
                .await
        }
        Commands::Git { command } => {
            let client = DaemonClient::from_config(config.as_ref());
            let event = match command {
                GitCommands::Commit {
                    repo,
                    branch,
                    commit,
                    summary,
                    channel,
                } => IncomingEvent::git_commit(repo, branch, commit, summary, channel),
                GitCommands::BranchChanged {
                    repo,
                    old_branch,
                    new_branch,
                    channel,
                } => IncomingEvent::git_branch_changed(repo, old_branch, new_branch, channel),
            };
            client.send_event(&event).await
        }
        Commands::Github { command } => {
            let client = DaemonClient::from_config(config.as_ref());
            let event = match command {
                GithubCommands::IssueOpened {
                    repo,
                    number,
                    title,
                    channel,
                } => IncomingEvent::github_issue_opened(repo, number, title, channel),
                GithubCommands::PrStatusChanged {
                    repo,
                    number,
                    title,
                    old_status,
                    new_status,
                    url,
                    channel,
                } => IncomingEvent::github_pr_status_changed(
                    repo, number, title, old_status, new_status, url, channel,
                ),
            };
            client.send_event(&event).await
        }
        Commands::Install { systemd } => lifecycle::install(systemd),
        Commands::Update { restart } => lifecycle::update(restart),
        Commands::Uninstall {
            remove_systemd,
            remove_config,
        } => lifecycle::uninstall(remove_systemd, remove_config),
        Commands::Tmux { command } => match command {
            TmuxCommands::Keyword {
                session,
                keyword,
                line,
                channel,
            } => {
                let client = DaemonClient::from_config(config.as_ref());
                client
                    .send_event(&IncomingEvent::tmux_keyword(
                        session, keyword, line, channel,
                    ))
                    .await
            }
            TmuxCommands::Stale {
                session,
                pane,
                minutes,
                last_line,
                channel,
            } => {
                let client = DaemonClient::from_config(config.as_ref());
                client
                    .send_event(&IncomingEvent::tmux_stale(
                        session, pane, minutes, last_line, channel,
                    ))
                    .await
            }
            TmuxCommands::New(args) => tmux_wrapper::run(args, config.as_ref()).await,
        },
        Commands::Config { command } => match command.unwrap_or(ConfigCommand::Interactive) {
            ConfigCommand::Interactive => {
                let mut editable = AppConfig::load_or_default(&config_path)?;
                editable.run_interactive_editor(&config_path)
            }
            ConfigCommand::Show => {
                println!("{}", config.to_pretty_toml()?);
                Ok(())
            }
            ConfigCommand::Path => {
                println!("{}", config_path.display());
                Ok(())
            }
        },
    }
}
