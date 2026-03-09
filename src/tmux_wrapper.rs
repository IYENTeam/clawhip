use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

use tokio::process::Command;
use tokio::time::sleep;

use crate::Result;
use crate::cli::{TmuxNewArgs, TmuxWatchArgs, TmuxWrapperFormat};
use crate::client::DaemonClient;
use crate::config::AppConfig;
use crate::events::IncomingEvent;
use crate::keyword_window::{PendingKeywordHits, collect_keyword_hits};
use crate::monitor::RegisteredTmuxSession;

pub async fn run(args: TmuxNewArgs, config: &AppConfig) -> Result<()> {
    launch_session(&args).await?;
    let monitor_args = TmuxMonitorArgs::from(&args);
    let monitor = register_and_start_monitor(monitor_args, config).await?;

    if args.attach {
        attach_session(&args.session).await?;
    }

    monitor.await??;
    Ok(())
}

pub async fn watch(args: TmuxWatchArgs, config: &AppConfig) -> Result<()> {
    if !session_exists(&args.session).await? {
        return Err(format!("tmux session '{}' does not exist", args.session).into());
    }

    let monitor = register_and_start_monitor(TmuxMonitorArgs::from(&args), config).await?;
    monitor.await??;
    Ok(())
}

#[derive(Clone)]
struct TmuxMonitorArgs {
    session: String,
    channel: Option<String>,
    mention: Option<String>,
    keywords: Vec<String>,
    keyword_window_secs: u64,
    stale_minutes: u64,
    format: Option<TmuxWrapperFormat>,
}

impl From<&TmuxNewArgs> for TmuxMonitorArgs {
    fn from(value: &TmuxNewArgs) -> Self {
        Self {
            session: value.session.clone(),
            channel: value.channel.clone(),
            mention: value.mention.clone(),
            keywords: value.keywords.clone(),
            keyword_window_secs: default_keyword_window_secs(),
            stale_minutes: value.stale_minutes,
            format: value.format,
        }
    }
}

impl From<&TmuxWatchArgs> for TmuxMonitorArgs {
    fn from(value: &TmuxWatchArgs) -> Self {
        Self {
            session: value.session.clone(),
            channel: value.channel.clone(),
            mention: value.mention.clone(),
            keywords: value.keywords.clone(),
            keyword_window_secs: default_keyword_window_secs(),
            stale_minutes: value.stale_minutes,
            format: value.format,
        }
    }
}

async fn register_and_start_monitor(
    args: TmuxMonitorArgs,
    config: &AppConfig,
) -> Result<tokio::task::JoinHandle<Result<()>>> {
    let client = DaemonClient::from_config(config);
    let registration = RegisteredTmuxSession {
        session: args.session.clone(),
        channel: args.channel.clone(),
        mention: args.mention.clone(),
        keywords: args.keywords.clone(),
        keyword_window_secs: args.keyword_window_secs,
        stale_minutes: args.stale_minutes,
        format: args.format.map(Into::into),
        active_wrapper_monitor: true,
    };
    client.register_tmux(&registration).await?;

    let monitor_client = client.clone();
    Ok(tokio::spawn(async move {
        monitor_session(args, monitor_client).await
    }))
}

#[derive(Clone)]
struct PaneState {
    session: String,
    pane_name: String,
    content_hash: u64,
    snapshot: String,
    last_change: Instant,
    last_stale_notification: Option<Instant>,
    pending_keyword_hits: Option<PendingKeywordHits>,
}

#[derive(Clone)]
struct PaneSnapshot {
    pane_id: String,
    session: String,
    pane_name: String,
    content: String,
}

async fn monitor_session(args: TmuxMonitorArgs, client: DaemonClient) -> Result<()> {
    let mut state: HashMap<String, PaneState> = HashMap::new();
    let poll_interval = Duration::from_secs(1);
    let stale_after = Duration::from_secs(args.stale_minutes.max(1) * 60);
    let keyword_window = Duration::from_secs(args.keyword_window_secs.max(1));
    let keywords = args
        .keywords
        .iter()
        .map(|keyword| keyword.trim().to_string())
        .filter(|keyword| !keyword.is_empty())
        .collect::<Vec<_>>();

    loop {
        if !session_exists(&args.session).await? {
            flush_removed_panes(
                &mut state,
                &HashSet::new(),
                &args,
                &client,
                Instant::now(),
                true,
            )
            .await?;
            break;
        }

        let panes = snapshot_session(&args.session).await?;
        let mut active = HashSet::new();
        let now = Instant::now();

        for pane in panes {
            active.insert(pane.pane_id.clone());
            let pane_key = pane.pane_id.clone();
            let hash = content_hash(&pane.content);
            let latest_line = last_nonempty_line(&pane.content);

            match state.get_mut(&pane_key) {
                None => {
                    state.insert(
                        pane_key,
                        PaneState {
                            session: pane.session,
                            pane_name: pane.pane_name,
                            content_hash: hash,
                            snapshot: pane.content,
                            last_change: now,
                            last_stale_notification: None,
                            pending_keyword_hits: None,
                        },
                    );
                }
                Some(existing) => {
                    flush_pending_keyword_hits(
                        existing,
                        &args,
                        &client,
                        now,
                        keyword_window,
                        false,
                    )
                    .await?;
                    if existing.content_hash != hash {
                        let hits =
                            collect_keyword_hits(&existing.snapshot, &pane.content, &keywords);
                        if !hits.is_empty() {
                            existing
                                .pending_keyword_hits
                                .get_or_insert_with(|| PendingKeywordHits::new(now))
                                .push(hits);
                        }

                        existing.session = pane.session;
                        existing.pane_name = pane.pane_name;
                        existing.content_hash = hash;
                        existing.snapshot = pane.content;
                        existing.last_change = now;
                        existing.last_stale_notification = None;
                    } else if now.duration_since(existing.last_change) >= stale_after
                        && existing
                            .last_stale_notification
                            .map(|previous| now.duration_since(previous) >= stale_after)
                            .unwrap_or(true)
                    {
                        let event = tmux_stale_event(
                            &args,
                            existing.session.clone(),
                            existing.pane_name.clone(),
                            latest_line,
                        );
                        client.send_event(&event).await?;
                        existing.last_stale_notification = Some(now);
                    }
                }
            }
        }

        flush_removed_panes(&mut state, &active, &args, &client, now, true).await?;
        state.retain(|pane_id, _| active.contains(pane_id));
        sleep(poll_interval).await;
    }

    Ok(())
}

fn tmux_keyword_event(
    args: &TmuxMonitorArgs,
    session: String,
    hits: Vec<(String, String)>,
) -> IncomingEvent {
    let mut event = IncomingEvent::tmux_keywords(session, hits, args.channel.clone());
    event.format = args.format.map(Into::into);
    event.mention = args.mention.clone();
    event
}

fn tmux_stale_event(
    args: &TmuxMonitorArgs,
    session: String,
    pane: String,
    last_line: String,
) -> IncomingEvent {
    let mut event = IncomingEvent::tmux_stale(
        session,
        pane,
        args.stale_minutes,
        last_line,
        args.channel.clone(),
    );
    event.format = args.format.map(Into::into);
    event.mention = args.mention.clone();
    event
}

async fn flush_pending_keyword_hits(
    pane: &mut PaneState,
    args: &TmuxMonitorArgs,
    client: &DaemonClient,
    now: Instant,
    keyword_window: Duration,
    force: bool,
) -> Result<()> {
    let should_flush = pane
        .pending_keyword_hits
        .as_ref()
        .map(|pending| force || pending.ready_to_flush(now, keyword_window))
        .unwrap_or(false);
    if !should_flush {
        return Ok(());
    }

    let Some(pending) = pane.pending_keyword_hits.take() else {
        return Ok(());
    };
    let hits = pending
        .into_hits()
        .into_iter()
        .map(|hit| (hit.keyword, hit.line))
        .collect::<Vec<_>>();
    if hits.is_empty() {
        return Ok(());
    }

    let event = tmux_keyword_event(args, pane.session.clone(), hits);
    client.send_event(&event).await
}

async fn flush_removed_panes(
    state: &mut HashMap<String, PaneState>,
    active: &HashSet<String>,
    args: &TmuxMonitorArgs,
    client: &DaemonClient,
    now: Instant,
    force: bool,
) -> Result<()> {
    let keys_to_remove = state
        .keys()
        .filter(|pane_id| !active.contains(*pane_id))
        .cloned()
        .collect::<Vec<_>>();
    for pane_id in keys_to_remove {
        if let Some(mut pane) = state.remove(&pane_id) {
            flush_pending_keyword_hits(
                &mut pane,
                args,
                client,
                now,
                Duration::from_secs(args.keyword_window_secs.max(1)),
                force,
            )
            .await?;
        }
    }
    Ok(())
}

async fn launch_session(args: &TmuxNewArgs) -> Result<()> {
    let mut command = Command::new(tmux_bin());
    command
        .arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(&args.session);
    if let Some(window_name) = &args.window_name {
        command.arg("-n").arg(window_name);
    }
    if let Some(cwd) = &args.cwd {
        command.arg("-c").arg(cwd);
    }
    let output = command.output().await?;
    if !output.status.success() {
        return Err(tmux_stderr(&output.stderr).into());
    }

    if let Some(command) = build_command_to_send(args) {
        if args.retry_enter {
            send_keys_reliable(
                &args.session,
                &command,
                args.retry_enter_count,
                args.retry_enter_delay_ms,
            )
            .await?;
        } else {
            send_command_to_session(&args.session, &command).await?;
        }
    }

    Ok(())
}

async fn send_command_to_session(session: &str, command: &str) -> Result<()> {
    send_literal_keys(session, command).await?;
    send_enter_key(session, "Enter").await
}

async fn send_keys_reliable(
    session: &str,
    text: &str,
    retry_count: u32,
    retry_delay_ms: u64,
) -> Result<()> {
    send_literal_keys(session, text).await?;
    let mut baseline_hash = capture_target_hash(session).await?;

    for delay in retry_enter_delays(retry_count, retry_delay_ms) {
        send_enter_key(session, "Enter").await?;
        sleep(delay).await;
        let current_hash = capture_target_hash(session).await?;
        if current_hash != baseline_hash {
            return Ok(());
        }

        baseline_hash = current_hash;
    }

    Ok(())
}

fn retry_enter_delays(retry_count: u32, retry_delay_ms: u64) -> Vec<Duration> {
    let base_delay = retry_delay_ms.max(1);
    let mut next_delay_ms = base_delay;

    (0..=retry_count)
        .map(|_| {
            let delay = Duration::from_millis(next_delay_ms);
            next_delay_ms = next_delay_ms.saturating_mul(2);
            delay
        })
        .collect()
}

async fn send_literal_keys(session: &str, text: &str) -> Result<()> {
    let literal_output = Command::new(tmux_bin())
        .arg("send-keys")
        .arg("-t")
        .arg(session)
        .arg("-l")
        .arg(text)
        .output()
        .await?;
    if !literal_output.status.success() {
        return Err(tmux_stderr(&literal_output.stderr).into());
    }

    Ok(())
}

async fn send_enter_key(session: &str, key: &str) -> Result<()> {
    let enter_output = Command::new(tmux_bin())
        .arg("send-keys")
        .arg("-t")
        .arg(session)
        .arg(key)
        .output()
        .await?;
    if !enter_output.status.success() {
        return Err(tmux_stderr(&enter_output.stderr).into());
    }

    Ok(())
}

async fn capture_target_hash(target: &str) -> Result<u64> {
    let capture = Command::new(tmux_bin())
        .arg("capture-pane")
        .arg("-p")
        .arg("-t")
        .arg(target)
        .arg("-S")
        .arg("-200")
        .output()
        .await?;
    if !capture.status.success() {
        return Err(tmux_stderr(&capture.stderr).into());
    }

    Ok(content_hash(&String::from_utf8(capture.stdout)?))
}

fn build_command_to_send(args: &TmuxNewArgs) -> Option<String> {
    if args.command.is_empty() {
        return None;
    }

    let joined = if args.command.len() == 1 {
        args.command[0].clone()
    } else {
        shell_join(&args.command)
    };
    Some(match &args.shell {
        Some(shell) => format!("{} -c {}", shell_escape(shell), shell_escape(&joined)),
        None => joined,
    })
}

fn shell_join(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| shell_escape(part))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_escape(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "_@%+=:,./-".contains(ch))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn tmux_stderr(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr).trim().to_string()
}

async fn attach_session(session: &str) -> Result<()> {
    let output = Command::new(tmux_bin())
        .arg("attach-session")
        .arg("-t")
        .arg(session)
        .output()
        .await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into())
    }
}

async fn session_exists(session: &str) -> Result<bool> {
    let output = Command::new(tmux_bin())
        .arg("has-session")
        .arg("-t")
        .arg(session)
        .output()
        .await?;
    Ok(output.status.success())
}

async fn snapshot_session(session: &str) -> Result<Vec<PaneSnapshot>> {
    let output = Command::new(tmux_bin())
        .arg("list-panes")
        .arg("-t")
        .arg(session)
        .arg("-F")
        .arg("#{pane_id}|#{session_name}|#{window_index}.#{pane_index}|#{pane_title}")
        .output()
        .await?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into());
    }

    let mut panes = Vec::new();
    for line in String::from_utf8(output.stdout)?.lines() {
        let mut parts = line.splitn(4, '|');
        let pane_id = parts.next().unwrap_or_default().to_string();
        if pane_id.is_empty() {
            continue;
        }
        let session_name = parts.next().unwrap_or_default().to_string();
        let pane_name = parts.next().unwrap_or_default().to_string();
        let capture = Command::new(tmux_bin())
            .arg("capture-pane")
            .arg("-p")
            .arg("-t")
            .arg(&pane_id)
            .arg("-S")
            .arg("-200")
            .output()
            .await?;
        if !capture.status.success() {
            return Err(String::from_utf8_lossy(&capture.stderr)
                .trim()
                .to_string()
                .into());
        }
        panes.push(PaneSnapshot {
            pane_id,
            session: session_name,
            pane_name,
            content: String::from_utf8(capture.stdout)?,
        });
    }
    Ok(panes)
}

fn content_hash(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

fn last_nonempty_line(content: &str) -> String {
    content
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("<no output>")
        .trim()
        .to_string()
}

fn tmux_bin() -> String {
    std::env::var("CLAWHIP_TMUX_BIN").unwrap_or_else(|_| "tmux".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyword_window::KeywordHit;

    #[test]
    fn keyword_hits_only_emit_for_new_lines() {
        let hits = collect_keyword_hits(
            "done
all good",
            "done
all good
error: failed
PR created #7",
            &["error".into(), "PR created".into()],
        );
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].keyword, "error");
        assert_eq!(hits[1].keyword, "PR created");
    }

    #[test]
    fn build_command_to_send_preserves_shell_arguments_when_joining() {
        let args = TmuxNewArgs {
            session: "dev".into(),
            window_name: None,
            cwd: None,
            channel: None,
            mention: None,
            keywords: Vec::new(),
            stale_minutes: 10,
            format: None,
            attach: false,
            retry_enter: true,
            retry_enter_count: crate::cli::DEFAULT_RETRY_ENTER_COUNT,
            retry_enter_delay_ms: crate::cli::DEFAULT_RETRY_ENTER_DELAY_MS,
            shell: None,
            command: vec![
                "zsh".into(),
                "-c".into(),
                "source ~/.zshrc && omx --madmax".into(),
            ],
        };

        assert_eq!(
            build_command_to_send(&args).as_deref(),
            Some("zsh -c 'source ~/.zshrc && omx --madmax'")
        );
    }

    #[test]
    fn build_command_to_send_wraps_joined_command_with_override_shell() {
        let args = TmuxNewArgs {
            session: "dev".into(),
            window_name: None,
            cwd: None,
            channel: None,
            mention: None,
            keywords: Vec::new(),
            stale_minutes: 10,
            format: None,
            attach: false,
            retry_enter: true,
            retry_enter_count: crate::cli::DEFAULT_RETRY_ENTER_COUNT,
            retry_enter_delay_ms: crate::cli::DEFAULT_RETRY_ENTER_DELAY_MS,
            shell: Some("/bin/zsh".into()),
            command: vec!["source ~/.zshrc && omx --madmax".into()],
        };

        assert_eq!(
            build_command_to_send(&args).as_deref(),
            Some("/bin/zsh -c 'source ~/.zshrc && omx --madmax'")
        );
    }

    #[test]
    fn build_command_to_send_leaves_single_shell_snippet_unquoted_without_override() {
        let args = TmuxNewArgs {
            session: "dev".into(),
            window_name: None,
            cwd: None,
            channel: None,
            mention: None,
            keywords: Vec::new(),
            stale_minutes: 10,
            format: None,
            attach: false,
            retry_enter: true,
            retry_enter_count: crate::cli::DEFAULT_RETRY_ENTER_COUNT,
            retry_enter_delay_ms: crate::cli::DEFAULT_RETRY_ENTER_DELAY_MS,
            shell: None,
            command: vec!["source ~/.zshrc && omx --madmax".into()],
        };

        assert_eq!(
            build_command_to_send(&args).as_deref(),
            Some("source ~/.zshrc && omx --madmax")
        );
    }

    #[test]
    fn watch_args_convert_to_monitor_args() {
        let args = TmuxWatchArgs {
            session: "existing".into(),
            channel: Some("alerts".into()),
            mention: Some("<@123>".into()),
            keywords: vec!["error".into(), "complete".into()],
            stale_minutes: 15,
            format: Some(TmuxWrapperFormat::Inline),
            retry_enter: true,
        };

        let monitor_args = TmuxMonitorArgs::from(&args);

        assert_eq!(monitor_args.session, "existing");
        assert_eq!(monitor_args.channel.as_deref(), Some("alerts"));
        assert_eq!(monitor_args.mention.as_deref(), Some("<@123>"));
        assert_eq!(monitor_args.keywords, vec!["error", "complete"]);
        assert_eq!(monitor_args.keyword_window_secs, 30);
        assert_eq!(monitor_args.stale_minutes, 15);
        assert!(matches!(
            monitor_args.format,
            Some(TmuxWrapperFormat::Inline)
        ));
    }

    #[test]
    fn tmux_keyword_event_inherits_channel_format_and_mention() {
        let args = TmuxMonitorArgs {
            session: "issue-24".into(),
            channel: Some("alerts".into()),
            mention: Some("<@123>".into()),
            keywords: vec!["error".into()],
            keyword_window_secs: 30,
            stale_minutes: 15,
            format: Some(TmuxWrapperFormat::Alert),
        };

        let event = tmux_keyword_event(
            &args,
            "issue-24".into(),
            vec![("error".into(), "boom".into())],
        );

        assert_eq!(event.channel.as_deref(), Some("alerts"));
        assert_eq!(event.mention.as_deref(), Some("<@123>"));
        assert!(matches!(
            event.format,
            Some(crate::events::MessageFormat::Alert)
        ));
        assert_eq!(event.payload["session"], "issue-24");
        assert_eq!(event.payload["keyword"], "error");
        assert_eq!(event.payload["line"], "boom");
    }

    #[test]
    fn tmux_stale_event_inherits_channel_format_and_mention() {
        let args = TmuxMonitorArgs {
            session: "issue-24".into(),
            channel: Some("alerts".into()),
            mention: Some("<@123>".into()),
            keywords: vec!["error".into()],
            keyword_window_secs: 30,
            stale_minutes: 15,
            format: Some(TmuxWrapperFormat::Inline),
        };

        let event = tmux_stale_event(&args, "issue-24".into(), "0.0".into(), "waiting".into());

        assert_eq!(event.channel.as_deref(), Some("alerts"));
        assert_eq!(event.mention.as_deref(), Some("<@123>"));
        assert!(matches!(
            event.format,
            Some(crate::events::MessageFormat::Inline)
        ));
        assert_eq!(event.payload["session"], "issue-24");
        assert_eq!(event.payload["pane"], "0.0");
        assert_eq!(event.payload["minutes"], 15);
        assert_eq!(event.payload["last_line"], "waiting");
    }

    #[test]
    fn retry_enter_delays_respect_requested_backoff_limit() {
        assert_eq!(retry_enter_delays(0, 250), vec![Duration::from_millis(250)]);
        assert_eq!(
            retry_enter_delays(2, 250),
            vec![
                Duration::from_millis(250),
                Duration::from_millis(500),
                Duration::from_millis(1_000)
            ]
        );
        assert_eq!(
            retry_enter_delays(4, 250),
            vec![
                Duration::from_millis(250),
                Duration::from_millis(500),
                Duration::from_millis(1_000),
                Duration::from_millis(2_000),
                Duration::from_millis(4_000)
            ]
        );
    }

    #[test]
    fn retry_enter_delays_clamp_zero_delay_to_one_millisecond() {
        assert_eq!(
            retry_enter_delays(2, 0),
            vec![
                Duration::from_millis(1),
                Duration::from_millis(2),
                Duration::from_millis(4)
            ]
        );
    }

    #[test]
    fn flush_pending_keyword_hits_clears_window_after_send_attempt() {
        let args = TmuxMonitorArgs {
            session: "issue-24".into(),
            channel: Some("alerts".into()),
            mention: None,
            keywords: vec!["error".into(), "complete".into()],
            keyword_window_secs: 30,
            stale_minutes: 15,
            format: Some(TmuxWrapperFormat::Compact),
        };
        let config = AppConfig::default();
        let client = DaemonClient::from_config(&config);
        let start = Instant::now();
        let mut pane = PaneState {
            session: "issue-24".into(),
            pane_name: "0.0".into(),
            content_hash: 0,
            snapshot: String::new(),
            last_change: start,
            last_stale_notification: None,
            pending_keyword_hits: Some({
                let mut pending = PendingKeywordHits::new(start);
                pending.push(vec![
                    KeywordHit {
                        keyword: "error".into(),
                        line: "boom".into(),
                    },
                    KeywordHit {
                        keyword: "error".into(),
                        line: "boom".into(),
                    },
                ]);
                pending
            }),
        };

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(flush_pending_keyword_hits(
                &mut pane,
                &args,
                &client,
                start + Duration::from_secs(30),
                Duration::from_secs(30),
                false,
            ));

        assert!(result.is_err());
        assert!(pane.pending_keyword_hits.is_none());
    }
}

fn default_keyword_window_secs() -> u64 {
    30
}
