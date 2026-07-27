use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::calendar::watch::REQUESTED_WATCH_LIFETIME_SECS;
use crate::events::MessageFormat;
use crate::source::workspace::{default_workspace_debounce_ms, default_workspace_watch_dirs};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default, skip_serializing_if = "ProvidersConfig::is_empty")]
    pub providers: ProvidersConfig,
    #[serde(default)]
    pub dispatch: DispatchConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub defaults: DefaultsConfig,
    #[serde(default)]
    pub routes: Vec<RouteRule>,
    #[serde(default)]
    pub monitors: MonitorConfig,
    #[serde(default, skip_serializing_if = "CronConfig::is_empty")]
    pub cron: CronConfig,
    #[serde(default, skip_serializing_if = "DiscordWatchConfig::is_empty")]
    pub discord_watch: DiscordWatchConfig,
    #[serde(default, skip_serializing_if = "crate::update::UpdateConfig::is_empty")]
    pub update: crate::update::UpdateConfig,
    #[serde(default, skip_serializing_if = "GajaeConfig::is_empty")]
    pub gajae: GajaeConfig,
    #[serde(default, skip_serializing_if = "AwsConfig::is_empty")]
    pub aws: AwsConfig,
    #[serde(default, skip_serializing_if = "CloudflareConfig::is_empty")]
    pub cloudflare: CloudflareConfig,
    #[serde(default, skip_serializing_if = "LinearConfig::is_empty")]
    pub linear: LinearConfig,
    #[serde(default, skip_serializing_if = "GoogleCalendarConfig::is_empty")]
    pub google_calendar: GoogleCalendarConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GajaeConfig {
    #[serde(default)]
    pub handlers_enabled: bool,
    #[serde(default = "default_gajae_handler_timeout_ms")]
    pub handler_timeout_ms: u64,
    #[serde(default = "default_gajae_handler_max_output_bytes")]
    pub handler_max_output_bytes: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_target_channel: Option<String>,
}

impl Default for GajaeConfig {
    fn default() -> Self {
        Self {
            handlers_enabled: false,
            handler_timeout_ms: default_gajae_handler_timeout_ms(),
            handler_max_output_bytes: default_gajae_handler_max_output_bytes(),
            hold_target_channel: None,
        }
    }
}

impl GajaeConfig {
    fn is_empty(&self) -> bool {
        !self.handlers_enabled
            && self.handler_timeout_ms == default_gajae_handler_timeout_ms()
            && self.handler_max_output_bytes == default_gajae_handler_max_output_bytes()
            && self.hold_target_channel.is_none()
    }
}

fn default_gajae_handler_timeout_ms() -> u64 {
    5_000
}

fn default_gajae_handler_max_output_bytes() -> usize {
    16 * 1024
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProvidersConfig {
    #[serde(default)]
    pub discord: DiscordConfig,
    #[serde(default)]
    pub slack: SlackConfig,
    #[serde(default)]
    pub openclaw: Option<OpenClawConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiscordConfig {
    #[serde(alias = "token")]
    pub bot_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SlackConfig {
    #[serde(alias = "token")]
    pub bot_token: Option<String>,
    #[serde(alias = "default_channel")]
    pub default_channel: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AwsConfig {
    #[serde(default)]
    pub topic_allowlist: Vec<String>,
    pub webhook_secret: Option<String>,
}

impl AwsConfig {
    fn is_empty(&self) -> bool {
        self.topic_allowlist.is_empty() && self.webhook_secret.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CloudflareConfig {
    pub webhook_secret: Option<String>,
    pub logpush_secret: Option<String>,
}

impl CloudflareConfig {
    fn is_empty(&self) -> bool {
        self.webhook_secret.is_none() && self.logpush_secret.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct LinearConfig {
    pub webhook_secret: Option<String>,
}

impl LinearConfig {
    fn is_empty(&self) -> bool {
        self.webhook_secret.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenClawConfig {
    pub gateway_url: String,
    pub gateway_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleCalendarConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_file: Option<PathBuf>,
    #[serde(default = "default_google_calendar_id")]
    pub calendar_id: String,
    #[serde(default = "default_google_calendar_api_base")]
    pub api_base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_url: Option<String>,
    #[serde(default = "default_google_calendar_renewal_margin_secs")]
    pub renewal_margin_secs: u64,
}

impl GoogleCalendarConfig {
    fn is_empty(&self) -> bool {
        self.channel_token.is_none()
            && self.credentials_file.is_none()
            && self.state_file.is_none()
            && self.callback_url.is_none()
    }

    pub fn sync_enabled(&self) -> bool {
        self.credentials_file.is_some() && self.state_file.is_some()
    }
}

impl Default for GoogleCalendarConfig {
    fn default() -> Self {
        Self {
            channel_token: None,
            credentials_file: None,
            state_file: None,
            calendar_id: default_google_calendar_id(),
            api_base_url: default_google_calendar_api_base(),
            callback_url: None,
            renewal_margin_secs: default_google_calendar_renewal_margin_secs(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_bind_host")]
    pub bind_host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_base_url")]
    pub base_url: String,
}

impl DiscordConfig {
    fn is_empty(&self) -> bool {
        self.bot_token.is_none()
    }
}

impl ProvidersConfig {
    fn is_empty(&self) -> bool {
        self.discord.is_empty() && self.slack.is_empty()
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            bind_host: default_bind_host(),
            port: default_port(),
            base_url: default_base_url(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchConfig {
    #[serde(default = "default_ci_batch_window_secs")]
    pub ci_batch_window_secs: u64,
    #[serde(default = "default_routine_batch_window_secs")]
    pub routine_batch_window_secs: u64,
}

impl Default for DispatchConfig {
    fn default() -> Self {
        Self {
            ci_batch_window_secs: default_ci_batch_window_secs(),
            routine_batch_window_secs: default_routine_batch_window_secs(),
        }
    }
}

impl DispatchConfig {
    pub fn ci_batch_window(&self) -> Duration {
        Duration::from_secs(self.ci_batch_window_secs.max(1))
    }

    pub fn routine_batch_window(&self) -> Option<Duration> {
        (self.routine_batch_window_secs > 0)
            .then(|| Duration::from_secs(self.routine_batch_window_secs))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultsConfig {
    pub channel: Option<String>,
    /// Human-readable channel name hint for the default channel (binding verification).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_name: Option<String>,
    #[serde(default)]
    pub format: MessageFormat,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            channel: None,
            channel_name: None,
            format: MessageFormat::Compact,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRule {
    pub event: String,
    #[serde(default)]
    pub filter: BTreeMap<String, String>,
    #[serde(default = "default_sink_name")]
    pub sink: String,
    pub channel: Option<String>,
    /// Explicit Discord thread ID target. Discord threads are channel-like
    /// endpoints, but keeping this separate from `channel` preserves operator
    /// intent and avoids hidden ID heuristics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// Human-readable Discord channel name hint for binding verification.
    /// When set, `op_pi config verify-bindings` compares the live channel
    /// name against this value to detect drift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_name: Option<String>,
    pub webhook: Option<String>,
    pub slack_webhook: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    pub mention: Option<String>,
    #[serde(default)]
    pub allow_dynamic_tokens: bool,
    pub format: Option<MessageFormat>,
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gajae: Option<GajaeRouteAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GajaeRouteAction {
    pub subcommand: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub requires_approval: bool,
}

impl Default for RouteRule {
    fn default() -> Self {
        Self {
            event: String::new(),
            filter: BTreeMap::new(),
            sink: default_sink_name(),
            channel: None,
            thread: None,
            channel_name: None,
            webhook: None,
            slack_webhook: None,
            local_path: None,
            mention: None,
            allow_dynamic_tokens: false,
            format: None,
            template: None,
            gajae: None,
        }
    }
}

impl SlackConfig {
    fn is_empty(&self) -> bool {
        self.bot_token.is_none() && self.default_channel.is_none()
    }
}

impl RouteRule {
    pub fn effective_sink(&self) -> &str {
        let sink = self.sink.trim();
        if self.slack_webhook_target().is_some() && (sink.is_empty() || sink == "discord") {
            "slack"
        } else if sink.is_empty() {
            "discord"
        } else {
            sink
        }
    }

    pub fn discord_webhook_target(&self) -> Option<&str> {
        (self.effective_sink() == "discord")
            .then(|| non_empty_trimmed(self.webhook.as_deref()))
            .flatten()
    }

    pub fn discord_thread_target(&self) -> Option<&str> {
        (self.effective_sink() == "discord")
            .then(|| non_empty_trimmed(self.thread.as_deref()))
            .flatten()
    }

    pub fn slack_webhook_target(&self) -> Option<&str> {
        non_empty_trimmed(self.slack_webhook.as_deref()).or_else(|| {
            (self.sink.trim() == "slack").then(|| non_empty_trimmed(self.webhook.as_deref()))?
        })
    }

    pub fn local_file_target(&self) -> Option<&str> {
        (self.effective_sink() == "localfile")
            .then(|| non_empty_trimmed(self.local_path.as_deref()))
            .flatten()
    }

    pub fn slack_channel_target(&self) -> Option<&str> {
        (self.effective_sink() == "slack")
            .then(|| non_empty_trimmed(self.channel.as_deref()))
            .flatten()
    }

    fn has_any_webhook_target(&self) -> bool {
        self.discord_webhook_target().is_some() || self.slack_webhook_target().is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorConfig {
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    pub github_token: Option<String>,
    #[serde(default = "default_github_api_base")]
    pub github_api_base: String,
    #[serde(default)]
    pub github_monitor_state_path: Option<PathBuf>,
    #[serde(default = "default_reconciliation_interval_polls")]
    pub reconciliation_interval_polls: u64,
    #[serde(default)]
    pub git: GitMonitorConfig,
    #[serde(default)]
    pub tmux: TmuxMonitorConfig,
    #[serde(default)]
    pub workspace: Vec<WorkspaceMonitor>,
    #[serde(default)]
    pub discord_threads: Vec<DiscordThreadMonitor>,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: default_poll_interval(),
            github_token: None,
            github_api_base: default_github_api_base(),
            github_monitor_state_path: None,
            reconciliation_interval_polls: default_reconciliation_interval_polls(),
            git: GitMonitorConfig::default(),
            tmux: TmuxMonitorConfig::default(),
            workspace: Vec::new(),
            discord_threads: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitMonitorConfig {
    #[serde(default)]
    pub repos: Vec<GitRepoMonitor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TmuxMonitorConfig {
    #[serde(default)]
    pub sessions: Vec<TmuxSessionMonitor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRepoMonitor {
    pub path: String,
    pub name: Option<String>,
    #[serde(default = "default_remote")]
    pub remote: String,
    pub github_repo: Option<String>,
    #[serde(default = "default_true")]
    pub emit_commits: bool,
    #[serde(default = "default_true")]
    pub emit_branch_changes: bool,
    #[serde(default = "default_true")]
    pub emit_issue_opened: bool,
    #[serde(default)]
    pub emit_pr_status: bool,
    pub channel: Option<String>,
    /// Human-readable channel name hint for binding verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_name: Option<String>,
    pub mention: Option<String>,
    pub format: Option<MessageFormat>,
}

impl Default for GitRepoMonitor {
    fn default() -> Self {
        Self {
            path: String::new(),
            name: None,
            remote: default_remote(),
            github_repo: None,
            emit_commits: true,
            emit_branch_changes: true,
            emit_issue_opened: true,
            emit_pr_status: false,
            channel: None,
            channel_name: None,
            mention: None,
            format: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmuxSessionMonitor {
    pub session: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default = "default_keyword_window_secs")]
    pub keyword_window_secs: u64,
    #[serde(default = "default_stale_minutes")]
    pub stale_minutes: u64,
    pub channel: Option<String>,
    /// Human-readable channel name hint for binding verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_name: Option<String>,
    pub mention: Option<String>,
    pub format: Option<MessageFormat>,
}

impl Default for TmuxSessionMonitor {
    fn default() -> Self {
        Self {
            session: String::new(),
            keywords: Vec::new(),
            keyword_window_secs: default_keyword_window_secs(),
            stale_minutes: default_stale_minutes(),
            channel: None,
            channel_name: None,
            mention: None,
            format: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordThreadMonitor {
    pub parent_channel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_channel_name: Option<String>,
    pub mention: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_interval_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceMonitor {
    pub path: String,
    #[serde(default = "default_workspace_watch_dirs")]
    pub watch_dirs: Vec<String>,
    #[serde(default)]
    pub discover_worktrees: bool,
    pub channel: Option<String>,
    pub mention: Option<String>,
    pub format: Option<MessageFormat>,
    #[serde(default)]
    pub events: Vec<String>,
    pub poll_interval_secs: Option<u64>,
    #[serde(default = "default_workspace_debounce_ms")]
    pub debounce_ms: u64,
}

impl Default for WorkspaceMonitor {
    fn default() -> Self {
        Self {
            path: String::new(),
            watch_dirs: default_workspace_watch_dirs(),
            discover_worktrees: false,
            channel: None,
            mention: None,
            format: None,
            events: Vec::new(),
            poll_interval_secs: None,
            debounce_ms: default_workspace_debounce_ms(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordWatchConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_discord_watch_channels")]
    pub watched_channels: Vec<DiscordWatchChannel>,
    #[serde(default)]
    pub banned_channel_ids: Vec<String>,
    #[serde(default = "default_discord_watch_banned_channel_names")]
    pub banned_channel_names: Vec<String>,
    #[serde(default = "default_gaebal_gajae_user_id")]
    pub gaebal_gajae_user_id: String,
    #[serde(default)]
    pub owner_user_ids: Vec<String>,
    #[serde(default = "default_nudge_target_channel_id")]
    pub nudge_target_channel_id: Option<String>,
    #[serde(default = "default_pending_mentions_threshold")]
    pub pending_mentions_threshold: u64,
    #[serde(default = "default_direct_mention_persist_ms")]
    pub direct_mention_persist_ms: i64,
    #[serde(default = "default_channel_message_threshold")]
    pub channel_message_threshold: u64,
    #[serde(default = "default_discord_watch_global_cooldown_ms")]
    pub global_cooldown_ms: i64,
    #[serde(default = "default_discord_watch_channel_cooldown_ms")]
    pub channel_cooldown_ms: i64,
    #[serde(default = "default_discord_watch_doctrine_template")]
    pub doctrine_template: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscordWatchChannel {
    pub id: String,
    pub name: String,
}

impl Default for DiscordWatchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            watched_channels: default_discord_watch_channels(),
            banned_channel_ids: Vec::new(),
            banned_channel_names: default_discord_watch_banned_channel_names(),
            gaebal_gajae_user_id: default_gaebal_gajae_user_id(),
            owner_user_ids: Vec::new(),
            nudge_target_channel_id: default_nudge_target_channel_id(),
            pending_mentions_threshold: default_pending_mentions_threshold(),
            direct_mention_persist_ms: default_direct_mention_persist_ms(),
            channel_message_threshold: default_channel_message_threshold(),
            global_cooldown_ms: default_discord_watch_global_cooldown_ms(),
            channel_cooldown_ms: default_discord_watch_channel_cooldown_ms(),
            doctrine_template: default_discord_watch_doctrine_template(),
            state_file: None,
            intent_file: None,
        }
    }
}

impl DiscordWatchConfig {
    fn is_empty(&self) -> bool {
        !self.enabled
            && self.watched_channels == default_discord_watch_channels()
            && self.banned_channel_ids.is_empty()
            && self.banned_channel_names == default_discord_watch_banned_channel_names()
            && self.gaebal_gajae_user_id == default_gaebal_gajae_user_id()
            && self.owner_user_ids.is_empty()
            && self.nudge_target_channel_id == default_nudge_target_channel_id()
            && self.pending_mentions_threshold == default_pending_mentions_threshold()
            && self.direct_mention_persist_ms == default_direct_mention_persist_ms()
            && self.channel_message_threshold == default_channel_message_threshold()
            && self.global_cooldown_ms == default_discord_watch_global_cooldown_ms()
            && self.channel_cooldown_ms == default_discord_watch_channel_cooldown_ms()
            && self.doctrine_template == default_discord_watch_doctrine_template()
            && self.state_file.is_none()
            && self.intent_file.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronConfig {
    #[serde(default = "default_cron_poll_interval_secs")]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub jobs: Vec<CronJob>,
}

impl Default for CronConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: default_cron_poll_interval_secs(),
            jobs: Vec::new(),
        }
    }
}

impl CronConfig {
    fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub schedule: String,
    #[serde(default = "default_cron_timezone")]
    pub timezone: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub channel: Option<String>,
    pub mention: Option<String>,
    pub format: Option<MessageFormat>,
    /// Optional path to a JSON GAJAE receipt/state file that gates this job's emissions.
    ///
    /// When set, the cron scheduler reads the file before emitting. Validated
    /// GAJAE zero-backlog/follow-up receipts with zero open issues, zero open
    /// PRs, green dev CI, no action-needed sessions, and no holds suppress
    /// repeated notifications only while the same public-safe key remains within
    /// `zero_backlog_suppression_ttl_secs`. New public events, non-zero backlog,
    /// CI failures, branch head or check-summary changes, stale sessions, holds,
    /// missing files, or malformed JSON fail
    /// open and emit normally.
    ///
    /// GitHub API/rate-limit failures are detected separately from an empty
    /// backlog via `github_api_status` (e.g. `rate_limited`, `error`,
    /// `unavailable`). Such a degraded fallback is marked with an
    /// `observation_source`/`observation_confidence` pair on the emitted event
    /// and never counts as zero-backlog merge/close authority unless the receipt
    /// also sets `fallback_evidence: true` to assert the required corroborating
    /// evidence was gathered from a safe source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_file: Option<PathBuf>,
    /// Maximum seconds that a validated zero-backlog GAJAE receipt may suppress
    /// repeated follow-up notifications with the same public-safe suppression key.
    /// Set to `0` to disable suppression.
    #[serde(default = "default_zero_backlog_suppression_ttl_secs")]
    pub zero_backlog_suppression_ttl_secs: u64,
    #[serde(flatten)]
    pub kind: CronJobKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CronJobKind {
    CustomMessage { message: String },
}

pub fn default_config_path() -> PathBuf {
    op_pi::brand::default_config_path()
}

fn default_bind_host() -> String {
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    25294
}
fn default_base_url() -> String {
    format!("http://127.0.0.1:{}", default_port())
}
fn default_google_calendar_id() -> String {
    "primary".to_string()
}
fn default_google_calendar_api_base() -> String {
    "https://www.googleapis.com/calendar/v3".to_string()
}
fn default_google_calendar_renewal_margin_secs() -> u64 {
    24 * 60 * 60
}
fn default_poll_interval() -> u64 {
    5
}
fn default_github_api_base() -> String {
    "https://api.github.com".to_string()
}
fn default_remote() -> String {
    "origin".to_string()
}
fn default_stale_minutes() -> u64 {
    10
}
fn default_zero_backlog_suppression_ttl_secs() -> u64 {
    60 * 60
}
fn default_ci_batch_window_secs() -> u64 {
    30
}
fn default_routine_batch_window_secs() -> u64 {
    5
}
fn default_keyword_window_secs() -> u64 {
    30
}
fn default_cron_poll_interval_secs() -> u64 {
    30
}
fn default_cron_timezone() -> String {
    "UTC".to_string()
}

fn default_discord_watch_channels() -> Vec<DiscordWatchChannel> {
    Vec::new()
}
fn default_discord_watch_banned_channel_names() -> Vec<String> {
    vec!["omo".into(), "omo-help".into()]
}
fn default_gaebal_gajae_user_id() -> String {
    String::new()
}
fn default_nudge_target_channel_id() -> Option<String> {
    None
}
fn default_pending_mentions_threshold() -> u64 {
    5
}
fn default_direct_mention_persist_ms() -> i64 {
    180_000
}
fn default_channel_message_threshold() -> u64 {
    100
}
fn default_discord_watch_global_cooldown_ms() -> i64 {
    300_000
}
fn default_discord_watch_channel_cooldown_ms() -> i64 {
    300_000
}
fn default_discord_watch_doctrine_template() -> String {
    "UltraWorkers: <#{channel_id}> / {channel_name} 스윕하라. 기존 크론 독트린 기준으로 최근 메시지를 읽고 필요한 답변/액션만 수행하라.".into()
}
fn default_reconciliation_interval_polls() -> u64 {
    10
}

fn default_true() -> bool {
    true
}

pub fn default_sink_name() -> String {
    "discord".to_string()
}

const DISCORD_TOKEN_ENV_VARS: [&str; 2] = ["OP_PI_DISCORD_BOT_TOKEN", "DISCORD_TOKEN"];
const SLACK_TOKEN_ENV_VARS: [&str; 2] = ["OP_PI_SLACK_BOT_TOKEN", "SLACK_BOT_TOKEN"];
const DAEMON_URL_ENV_VARS: [&str; 1] = ["OP_PI_DAEMON_URL"];
const GITHUB_TOKEN_ENV_VARS: [&str; 2] = ["OP_PI_GITHUB_TOKEN", "GITHUB_TOKEN"];
pub const CONFIG_EDITOR_MENU_ITEMS: [&str; 8] = [
    "Set Discord bot token",
    "Set daemon base URL",
    "Set default channel",
    "Set default format",
    "Set Discord webhook quickstart route",
    "Save and exit",
    "Exit without saving",
    "Print manual config template hint",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetupEdits {
    pub webhook: Option<String>,
    pub bot_token: Option<String>,
    pub default_channel: Option<String>,
    pub default_format: Option<MessageFormat>,
    pub daemon_base_url: Option<String>,
}

impl SetupEdits {
    pub fn is_empty(&self) -> bool {
        self.webhook.is_none()
            && self.bot_token.is_none()
            && self.default_channel.is_none()
            && self.default_format.is_none()
            && self.daemon_base_url.is_none()
    }
}

fn normalize_secret(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn non_empty_trimmed(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn text_from_env_with<F>(env_vars: &[&str], mut get_env: F) -> Option<String>
where
    F: FnMut(&str) -> Option<String>,
{
    env_vars
        .iter()
        .find_map(|name| normalize_text(get_env(name)))
}

fn secret_from_env_with<F>(env_vars: &[&str], mut get_env: F) -> Option<String>
where
    F: FnMut(&str) -> Option<String>,
{
    env_vars
        .iter()
        .find_map(|name| normalize_secret(get_env(name)))
}

fn discord_token_from_env_with<F>(get_env: F) -> Option<String>
where
    F: FnMut(&str) -> Option<String>,
{
    secret_from_env_with(&DISCORD_TOKEN_ENV_VARS, get_env)
}

impl AppConfig {
    pub fn load_or_default(path: &Path) -> Result<Self> {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error.into()),
        };
        let mut config: Self = toml::from_str(&raw)?;
        config.normalize();
        if config.google_calendar.channel_token.is_some() || config.linear.webhook_secret.is_some()
        {
            validate_private_regular_config_file(path)?;
        }
        Ok(config)
    }

    pub fn to_pretty_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    pub fn to_redacted_pretty_toml(&self) -> Result<String> {
        let mut redacted = self.clone();
        for value in [
            &mut redacted.providers.discord.bot_token,
            &mut redacted.providers.slack.bot_token,
            &mut redacted.aws.webhook_secret,
            &mut redacted.cloudflare.webhook_secret,
            &mut redacted.cloudflare.logpush_secret,
            &mut redacted.google_calendar.channel_token,
            &mut redacted.linear.webhook_secret,
            &mut redacted.monitors.github_token,
        ] {
            if value.is_some() {
                *value = Some("[REDACTED]".into());
            }
        }
        for route in &mut redacted.routes {
            if route.webhook.is_some() {
                route.webhook = Some("[REDACTED]".into());
            }
            if route.slack_webhook.is_some() {
                route.slack_webhook = Some("[REDACTED]".into());
            }
        }
        if let Some(openclaw) = redacted.providers.openclaw.as_mut()
            && openclaw.gateway_token.is_some()
        {
            openclaw.gateway_token = Some("[REDACTED]".into());
        }
        redacted.to_pretty_toml()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let contents = self.to_pretty_toml()?;
        if self.google_calendar.channel_token.is_some() || self.linear.webhook_secret.is_some() {
            write_private_config(path, contents.as_bytes())
        } else {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, contents)?;
            Ok(())
        }
    }

    pub fn effective_slack_token(&self) -> Option<String> {
        self.effective_slack_token_with(|name| env::var(name).ok())
    }

    fn effective_slack_token_with<F>(&self, get_env: F) -> Option<String>
    where
        F: FnMut(&str) -> Option<String>,
    {
        secret_from_env_with(&SLACK_TOKEN_ENV_VARS, get_env)
            .or_else(|| normalize_secret(self.providers.slack.bot_token.clone()))
    }

    pub fn default_slack_channel(&self) -> Option<String> {
        normalize_text(self.providers.slack.default_channel.clone())
    }

    fn has_slack_channel_routes(&self) -> bool {
        self.routes.iter().any(|route| {
            route.slack_channel_target().is_some()
                || (route.effective_sink() == "slack"
                    && route.slack_webhook_target().is_none()
                    && self.default_slack_channel().is_some())
        })
    }

    pub fn effective_token(&self) -> Option<String> {
        self.effective_token_with(|name| env::var(name).ok())
    }

    fn effective_token_with<F>(&self, get_env: F) -> Option<String>
    where
        F: FnMut(&str) -> Option<String>,
    {
        discord_token_from_env_with(get_env)
            .or_else(|| normalize_secret(self.providers.discord.bot_token.clone()))
    }

    pub fn discord_token_source(&self) -> &'static str {
        self.discord_token_source_with(|name| env::var(name).ok())
    }

    fn discord_token_source_with<F>(&self, get_env: F) -> &'static str
    where
        F: FnMut(&str) -> Option<String>,
    {
        if discord_token_from_env_with(get_env).is_some() {
            "env"
        } else if normalize_secret(self.providers.discord.bot_token.clone()).is_some() {
            "config"
        } else {
            "missing"
        }
    }

    /// Returns the name of the environment variable whose Discord token shadows
    /// a token that is also present in the config file. Returns `None` when env
    /// does not win or when no config token is set, so a `Some(_)` value always
    /// means env precedence is silently overriding a configured token.
    ///
    /// Only the precedence source is exposed — never the token value itself.
    pub fn discord_token_env_shadow(&self) -> Option<&'static str> {
        self.discord_token_env_shadow_with(|name| env::var(name).ok())
    }

    fn discord_token_env_shadow_with<F>(&self, mut get_env: F) -> Option<&'static str>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let env_var = DISCORD_TOKEN_ENV_VARS
            .iter()
            .copied()
            .find(|name| normalize_secret(get_env(name)).is_some())?;
        let config_token_present =
            normalize_secret(self.providers.discord.bot_token.clone()).is_some();
        config_token_present.then_some(env_var)
    }

    pub fn webhook_route_count(&self) -> usize {
        self.routes
            .iter()
            .filter(|route| route.has_any_webhook_target())
            .count()
    }

    pub fn has_webhook_routes(&self) -> bool {
        self.webhook_route_count() > 0
    }

    fn has_localfile_routes(&self) -> bool {
        self.routes.iter().any(|route| {
            route.effective_sink() == "localfile" && route.local_file_target().is_some()
        })
    }

    fn has_drop_routes(&self) -> bool {
        self.routes
            .iter()
            .any(|route| route.effective_sink() == "drop")
    }

    fn has_discord_delivery_requiring_bot_token(&self) -> bool {
        self.default_channel_can_fallback_to_discord()
            || self.routes.iter().any(|route| {
                route.effective_sink() == "discord" && route.discord_webhook_target().is_none()
            })
            || self.monitors.git.repos.iter().any(|repo| {
                repo.channel
                    .as_ref()
                    .is_some_and(|channel| !channel.trim().is_empty())
            })
            || self.monitors.tmux.sessions.iter().any(|session| {
                session
                    .channel
                    .as_ref()
                    .is_some_and(|channel| !channel.trim().is_empty())
            })
            || self.monitors.workspace.iter().any(|workspace| {
                workspace
                    .channel
                    .as_ref()
                    .is_some_and(|channel| !channel.trim().is_empty())
            })
            || self.cron.jobs.iter().any(|job| {
                job.channel
                    .as_ref()
                    .is_some_and(|channel| !channel.trim().is_empty())
            })
    }

    fn default_channel_can_fallback_to_discord(&self) -> bool {
        self.defaults
            .channel
            .as_ref()
            .is_some_and(|channel| !channel.trim().is_empty())
            && !self
                .routes
                .iter()
                .any(|route| route.event.trim() == "*" && route.filter.is_empty())
    }

    pub fn validate(&self) -> Result<()> {
        if self.dispatch.ci_batch_window_secs == 0 {
            return Err("dispatch.ci_batch_window_secs must be at least 1".into());
        }
        if self.google_calendar.credentials_file.is_some()
            || self.google_calendar.state_file.is_some()
            || self.google_calendar.callback_url.is_some()
        {
            let mut missing = Vec::new();
            if self.google_calendar.credentials_file.is_none() {
                missing.push("credentials_file");
            }
            if self.google_calendar.state_file.is_none() {
                missing.push("state_file");
            }
            if self.google_calendar.callback_url.is_none() {
                missing.push("callback_url");
            }
            if self.google_calendar.channel_token.is_none() {
                missing.push("channel_token");
            }
            if !missing.is_empty() {
                return Err(format!(
                    "google_calendar sync configuration requires: {}",
                    missing.join(", ")
                )
                .into());
            }
        }
        if self.google_calendar.sync_enabled()
            && let Some(channel_token) = self.google_calendar.channel_token.as_deref()
            && channel_token.len() < 32
        {
            return Err(
                "google_calendar.channel_token must be at least 32 bytes when durable sync is enabled"
                    .into(),
            );
        }
        if self.google_calendar.renewal_margin_secs >= REQUESTED_WATCH_LIFETIME_SECS {
            return Err(format!(
                "google_calendar.renewal_margin_secs must be less than the requested watch lifetime of {REQUESTED_WATCH_LIFETIME_SECS} seconds"
            )
            .into());
        }
        if let Some(callback_url) = self.google_calendar.callback_url.as_deref() {
            validate_google_calendar_callback_url(callback_url)?;
        }
        validate_google_calendar_api_base_url(&self.google_calendar.api_base_url)?;

        if self.cron.poll_interval_secs == 0 {
            return Err("cron.poll_interval_secs must be at least 1".into());
        }
        if self.discord_watch.enabled {
            if self.discord_watch.gaebal_gajae_user_id.trim().is_empty() {
                return Err(
                    "discord_watch.gaebal_gajae_user_id is required when discord_watch is enabled"
                        .into(),
                );
            }
            for (index, channel) in self.discord_watch.watched_channels.iter().enumerate() {
                if channel.id.trim().is_empty() || channel.name.trim().is_empty() {
                    return Err(format!(
                        "discord_watch.watched_channels[{index}] requires non-empty id and name"
                    )
                    .into());
                }
            }
        }
        if self.discord_watch.pending_mentions_threshold == 0 {
            return Err("discord_watch.pending_mentions_threshold must be at least 1".into());
        }
        if self.discord_watch.direct_mention_persist_ms < 0 {
            return Err("discord_watch.direct_mention_persist_ms must be non-negative".into());
        }
        if self.discord_watch.channel_message_threshold == 0 {
            return Err("discord_watch.channel_message_threshold must be at least 1".into());
        }
        if self.discord_watch.global_cooldown_ms < 0 || self.discord_watch.channel_cooldown_ms < 0 {
            return Err("discord_watch cooldowns must be non-negative".into());
        }

        if self.gajae.handlers_enabled {
            if self.gajae.handler_timeout_ms == 0 {
                return Err(
                    "gajae.handler_timeout_ms must be at least 1 when GAJAE handlers are enabled"
                        .into(),
                );
            }
            if self.gajae.handler_max_output_bytes == 0 {
                return Err("gajae.handler_max_output_bytes must be at least 1 when GAJAE handlers are enabled".into());
            }
        }

        for (index, route) in self.routes.iter().enumerate() {
            let sink = route.effective_sink();
            let has_channel = normalize_secret(route.channel.clone()).is_some();
            let has_thread = route.discord_thread_target().is_some();
            let has_discord_webhook = route.discord_webhook_target().is_some();
            let has_slack_webhook = route.slack_webhook_target().is_some();
            if let Some(gajae) = &route.gajae {
                let subcommand = gajae.subcommand.trim();
                if subcommand.is_empty() {
                    return Err(format!(
                        "route #{} ({}) GAJAE handler must set subcommand",
                        index + 1,
                        route.event
                    )
                    .into());
                }
            }
            if route.sink.trim().is_empty() && !has_slack_webhook {
                return Err(
                    format!("route #{} ({}) must set a sink", index + 1, route.event).into(),
                );
            }
            if !matches!(sink, "discord" | "slack" | "localfile" | "drop") {
                return Err(format!(
                    "route #{} ({}) uses unsupported sink '{}'",
                    index + 1,
                    route.event,
                    sink
                )
                .into());
            }

            match sink {
                "discord" => {
                    let configured_targets = usize::from(has_channel)
                        + usize::from(has_thread)
                        + usize::from(has_discord_webhook);
                    if configured_targets > 1 {
                        return Err(format!(
                            "route #{} ({}) must set only one Discord target: channel, thread, or webhook",
                            index + 1,
                            route.event
                        )
                        .into());
                    }
                }
                "slack" => {
                    let has_slack_channel = route.slack_channel_target().is_some();
                    let configured_targets = usize::from(has_slack_channel)
                        + usize::from(normalize_secret(route.webhook.clone()).is_some())
                        + usize::from(normalize_secret(route.slack_webhook.clone()).is_some());
                    if configured_targets > 1 {
                        return Err(format!(
                            "route #{} ({}) must set only one Slack target: channel, webhook, or slack_webhook",
                            index + 1,
                            route.event
                        )
                        .into());
                    }
                    if configured_targets == 0 && self.default_slack_channel().is_none() {
                        return Err(format!(
                            "route #{} ({}) must set channel, webhook, or slack_webhook when sink = \"slack\" (or configure [providers.slack].default_channel)",
                            index + 1,
                            route.event
                        )
                        .into());
                    }
                }
                "localfile" => {
                    if has_channel || has_discord_webhook || has_slack_webhook {
                        return Err(format!(
                            "route #{} ({}) cannot set channel/webhook fields when sink = \"localfile\"",
                            index + 1,
                            route.event
                        )
                        .into());
                    }
                    if route.local_file_target().is_none() {
                        return Err(format!(
                            "route #{} ({}) must set local_path when sink = \"localfile\"",
                            index + 1,
                            route.event
                        )
                        .into());
                    }
                }
                "drop" => {}
                _ => unreachable!(),
            }
        }

        for (index, monitor) in self.monitors.discord_threads.iter().enumerate() {
            if monitor.parent_channel.trim().is_empty() {
                return Err(format!(
                    "discord thread monitor #{} must set parent_channel",
                    index + 1
                )
                .into());
            }
            if monitor.mention.trim().is_empty() {
                return Err(
                    format!("discord thread monitor #{} must set mention", index + 1).into(),
                );
            }
            if let Some(secs) = monitor.poll_interval_secs
                && secs == 0
            {
                return Err(format!(
                    "discord thread monitor #{} poll_interval_secs must be at least 1",
                    index + 1
                )
                .into());
            }
        }

        for (index, workspace) in self.monitors.workspace.iter().enumerate() {
            if workspace.path.trim().is_empty() {
                return Err(format!("workspace monitor #{} must set path", index + 1).into());
            }
            if workspace.watch_dirs.is_empty() {
                return Err(format!(
                    "workspace monitor #{} must set at least one watch_dirs entry",
                    index + 1
                )
                .into());
            }
            if workspace.channel.is_none()
                && self.defaults.channel.is_none()
                && !self.has_webhook_routes()
            {
                return Err(format!(
                    "workspace monitor #{} has no channel and no default Discord destination",
                    index + 1
                )
                .into());
            }
        }

        let mut cron_ids = std::collections::BTreeSet::new();
        for (index, job) in self.cron.jobs.iter().enumerate() {
            crate::cron::validate_job(job)
                .map_err(|error| format!("cron job #{}: {error}", index + 1))?;
            if !cron_ids.insert(job.id.as_str()) {
                return Err(format!("duplicate cron job id '{}'", job.id).into());
            }
        }

        if self.has_slack_channel_routes() && self.effective_slack_token().is_none() {
            return Err(
                "missing Slack bot token for configured Slack channel delivery; configure [providers.slack].token or OP_PI_SLACK_BOT_TOKEN, use route webhooks, or remove Slack channel routes"
                    .into(),
            );
        }

        if self.effective_token().is_none() {
            if self.has_discord_delivery_requiring_bot_token() {
                return Err(
                    "missing Discord bot token for configured Discord channel delivery; configure [providers.discord].token or OP_PI_DISCORD_BOT_TOKEN, use route webhooks, or remove Discord channel routes"
                        .into(),
                );
            }

            if !self.has_webhook_routes()
                && !self.has_localfile_routes()
                && !self.has_drop_routes()
                && !self.has_slack_channel_routes()
                && !self.discord_watch.enabled
            {
                return Err(
                    "missing Discord delivery config: configure [providers.discord].token or OP_PI_DISCORD_BOT_TOKEN, at least one route webhook, a localfile route, or a drop route"
                        .into(),
                );
            }
        }

        Ok(())
    }

    pub fn apply_setup_edits(&mut self, edits: SetupEdits) -> Result<()> {
        let normalized = SetupEdits {
            webhook: normalize_text(edits.webhook),
            bot_token: normalize_secret(edits.bot_token),
            default_channel: normalize_text(edits.default_channel),
            default_format: edits.default_format,
            daemon_base_url: normalize_text(edits.daemon_base_url),
        };

        if normalized.is_empty() {
            return Err("setup requires at least one non-empty setup flag".into());
        }

        let SetupEdits {
            webhook,
            bot_token,
            default_channel,
            default_format,
            daemon_base_url,
        } = normalized;

        if let Some(webhook) = webhook {
            self.scaffold_webhook_quickstart(webhook)?;
        }
        if let Some(bot_token) = bot_token {
            self.providers.discord.bot_token = Some(bot_token);
        }
        if let Some(default_channel) = default_channel {
            self.defaults.channel = Some(default_channel);
        }
        if let Some(default_format) = default_format {
            self.defaults.format = default_format;
        }
        if let Some(daemon_base_url) = daemon_base_url {
            self.daemon.base_url = daemon_base_url;
        }

        Ok(())
    }

    pub fn scaffold_webhook_quickstart(&mut self, webhook: String) -> Result<()> {
        let webhook = normalize_text(Some(webhook)).ok_or_else(|| {
            "setup requires a non-empty webhook URL when --webhook is supplied".to_string()
        })?;

        let matches = self
            .routes
            .iter()
            .enumerate()
            .filter(|(_, route)| is_canonical_quickstart_route(route))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();

        match matches.as_slice() {
            [] => {
                self.routes.push(RouteRule {
                    event: "*".to_string(),
                    filter: BTreeMap::new(),
                    sink: default_sink_name(),
                    channel: None,
                    thread: None,
                    channel_name: None,
                    webhook: Some(webhook),
                    slack_webhook: None,
                    local_path: None,
                    mention: None,
                    allow_dynamic_tokens: false,
                    format: None,
                    template: None,
                    gajae: None,
                });
                Ok(())
            }
            [index] => {
                self.routes[*index].webhook = Some(webhook);
                Ok(())
            }
            _ => Err(
                "multiple canonical quickstart routes found; clean up manual config before updating the webhook quickstart route"
                    .into(),
            ),
        }
    }

    /// Scaffold or update a repo→channel route with a binding-verify hint.
    ///
    /// Creates a `[[routes]]` entry shaped as:
    ///
    /// ```toml
    /// [[routes]]
    /// event = "*"
    /// filter = { repo = "<repo>" }
    /// sink = "discord"
    /// channel = "<channel_id>"
    /// channel_name = "<live_name>"  # hint, used by verify-bindings
    /// ```
    ///
    /// If an existing route matches the exact `(event="*", filter={repo=...},
    /// sink="discord")` shape, its channel and channel_name are updated in place
    /// instead of appending a duplicate.
    pub fn apply_repo_binding(
        &mut self,
        repo: &str,
        channel_id: &str,
        channel_name: Option<&str>,
    ) -> Result<()> {
        let repo = normalize_text(Some(repo.to_string()))
            .ok_or_else(|| "repo binding requires a non-empty repo name".to_string())?;
        let channel_id = normalize_text(Some(channel_id.to_string()))
            .ok_or_else(|| "repo binding requires a non-empty channel id".to_string())?;
        let channel_name = channel_name.and_then(|value| normalize_text(Some(value.to_string())));

        let existing = self
            .routes
            .iter_mut()
            .find(|route| is_repo_binding_route(route, &repo));

        match existing {
            Some(route) => {
                route.channel = Some(channel_id);
                route.thread = None;
                route.channel_name = channel_name;
                route.webhook = None;
            }
            None => {
                let mut filter = BTreeMap::new();
                filter.insert("repo".to_string(), repo);
                self.routes.push(RouteRule {
                    event: "*".to_string(),
                    filter,
                    sink: default_sink_name(),
                    channel: Some(channel_id),
                    thread: None,
                    channel_name,
                    webhook: None,
                    slack_webhook: None,
                    local_path: None,
                    mention: None,
                    allow_dynamic_tokens: false,
                    format: None,
                    template: None,
                    gajae: None,
                });
            }
        }
        Ok(())
    }

    pub fn set_discord_bot_token(&mut self, bot_token: String) {
        self.providers.discord.bot_token = normalize_secret(Some(bot_token));
    }

    pub fn set_default_channel(&mut self, channel: String) {
        self.defaults.channel = normalize_text(Some(channel));
    }

    pub fn set_default_format(&mut self, format: MessageFormat) {
        self.defaults.format = format;
    }

    pub fn set_daemon_base_url(&mut self, base_url: String) {
        self.daemon.base_url = normalize_text(Some(base_url)).unwrap_or_else(default_base_url);
    }

    fn canonical_quickstart_webhook(&self) -> Option<&str> {
        self.routes
            .iter()
            .find(|route| is_canonical_quickstart_route(route))
            .and_then(|route| route.webhook.as_deref())
    }

    pub fn daemon_base_url(&self) -> String {
        self.daemon_base_url_with(|name| env::var(name).ok())
    }

    fn daemon_base_url_with<F>(&self, get_env: F) -> String
    where
        F: FnMut(&str) -> Option<String>,
    {
        text_from_env_with(&DAEMON_URL_ENV_VARS, get_env)
            .unwrap_or_else(|| self.daemon.base_url.clone())
    }

    pub fn monitor_github_token(&self) -> Option<String> {
        self.monitor_github_token_with(|name| env::var(name).ok())
    }

    fn monitor_github_token_with<F>(&self, get_env: F) -> Option<String>
    where
        F: FnMut(&str) -> Option<String>,
    {
        secret_from_env_with(&GITHUB_TOKEN_ENV_VARS, get_env)
            .or_else(|| self.monitors.github_token.clone())
    }

    pub fn run_interactive_editor(&mut self, path: &Path) -> Result<()> {
        println!("op_pi config editor");
        println!("Path: {}", path.display());
        println!();
        loop {
            self.print_summary();
            println!("Choose an action:");
            for (index, item) in CONFIG_EDITOR_MENU_ITEMS.iter().enumerate() {
                println!("  {}) {}", index + 1, item);
            }
            match prompt("Selection")?.trim() {
                "1" => self.set_discord_bot_token(prompt("Bot token")?),
                "2" => self.set_daemon_base_url(prompt_with_default(
                    "Daemon base URL",
                    Some(&self.daemon.base_url),
                )?),
                "3" => self.set_default_channel(prompt("Default channel")?),
                "4" => self.set_default_format(prompt_format(Some(self.defaults.format.clone()))?),
                "5" => {
                    let webhook = prompt_with_default(
                        "Discord webhook quickstart route",
                        self.canonical_quickstart_webhook(),
                    )?;
                    self.scaffold_webhook_quickstart(webhook)?;
                }
                "6" => {
                    self.save(path)?;
                    println!("Saved {}", path.display());
                    break;
                }
                "7" => {
                    println!("Discarded changes.");
                    break;
                }
                "8" => self.print_template_hint(),
                _ => println!("Unknown selection."),
            }
            println!();
        }
        Ok(())
    }

    fn print_summary(&self) {
        println!("Current config summary:");
        println!("  Discord token source: {}", self.discord_token_source());
        println!("  Daemon base URL: {}", self.daemon.base_url);
        println!(
            "  Bind host/port: {}:{}",
            self.daemon.bind_host, self.daemon.port
        );
        println!("  CI batch window: {}s", self.dispatch.ci_batch_window_secs);
        println!(
            "  Routine batch window: {}",
            self.dispatch
                .routine_batch_window()
                .map(|window| format!("{}s", window.as_secs()))
                .unwrap_or_else(|| "disabled".to_string())
        );
        println!(
            "  Default channel: {}",
            self.defaults.channel.as_deref().unwrap_or("<unset>")
        );
        println!("  Webhook routes: {}", self.routes_with_webhooks());
        println!("  Default format: {}", self.defaults.format.as_str());
        println!("  Routes: {}", self.routes.len());
        println!("  Git monitors: {}", self.monitors.git.repos.len());
        println!("  Tmux monitors: {}", self.monitors.tmux.sessions.len());
        println!("  Workspace monitors: {}", self.monitors.workspace.len());
        println!("  Cron jobs: {}", self.cron.jobs.len());
    }

    fn print_template_hint(&self) {
        println!("Advanced routes and monitors are still edited manually in the config file.");
        println!(
            "Sections: [providers.discord], [dispatch], [daemon], [cron], [[cron.jobs]], [[routes]], [[monitors.git.repos]], [[monitors.tmux.sessions]], [[monitors.workspace]]"
        );
        println!(
            "Routes may set either channel = \"...\" or webhook = \"https://discord.com/api/webhooks/...\"."
        );
        println!(
            r#"Webhook example: [[routes]] event = "tmux.keyword" webhook = "https://discord.com/api/webhooks/...""#
        );
    }

    fn normalize(&mut self) {
        self.google_calendar.channel_token =
            normalize_secret(self.google_calendar.channel_token.take());
        self.google_calendar.calendar_id =
            normalize_text(Some(self.google_calendar.calendar_id.clone()))
                .unwrap_or_else(default_google_calendar_id);
        self.google_calendar.api_base_url =
            normalize_text(Some(self.google_calendar.api_base_url.clone()))
                .unwrap_or_else(default_google_calendar_api_base);
        self.google_calendar.callback_url =
            normalize_text(self.google_calendar.callback_url.take());
        self.providers.discord.bot_token =
            normalize_secret(self.providers.discord.bot_token.clone());
        self.linear.webhook_secret = normalize_secret(self.linear.webhook_secret.clone());
        self.defaults.channel = normalize_text(self.defaults.channel.clone());
        self.monitors.github_token = normalize_secret(self.monitors.github_token.clone());

        for route in &mut self.routes {
            route.sink = normalize_text(Some(route.sink.clone())).unwrap_or_else(default_sink_name);
            route.channel = normalize_text(route.channel.clone());
            route.channel_name = normalize_text(route.channel_name.clone());
            route.webhook = normalize_text(route.webhook.clone());
            route.slack_webhook = normalize_text(route.slack_webhook.clone());
            route.mention = normalize_text(route.mention.clone());
            route.template = normalize_text(route.template.clone());
            if let Some(gajae) = &mut route.gajae {
                gajae.subcommand =
                    normalize_text(Some(gajae.subcommand.clone())).unwrap_or_default();
                gajae.args = gajae
                    .args
                    .iter()
                    .filter_map(|arg| normalize_text(Some(arg.clone())))
                    .collect();
            }
        }

        for repo in &mut self.monitors.git.repos {
            repo.channel = normalize_text(repo.channel.clone());
            repo.channel_name = normalize_text(repo.channel_name.clone());
            repo.mention = normalize_text(repo.mention.clone());
            repo.name = normalize_text(repo.name.clone());
            repo.github_repo = normalize_text(repo.github_repo.clone());
        }

        for session in &mut self.monitors.tmux.sessions {
            session.channel = normalize_text(session.channel.clone());
            session.channel_name = normalize_text(session.channel_name.clone());
            session.mention = normalize_text(session.mention.clone());
        }

        for monitor in &mut self.monitors.discord_threads {
            monitor.parent_channel =
                normalize_text(Some(monitor.parent_channel.clone())).unwrap_or_default();
            monitor.parent_channel_name = normalize_text(monitor.parent_channel_name.clone());
            monitor.mention = normalize_text(Some(monitor.mention.clone())).unwrap_or_default();
            monitor.message = normalize_text(monitor.message.clone());
            monitor.poll_interval_secs = monitor.poll_interval_secs.map(|secs| secs.max(1));
        }

        for workspace in &mut self.monitors.workspace {
            workspace.path = normalize_text(Some(workspace.path.clone())).unwrap_or_default();
            workspace.channel = normalize_text(workspace.channel.clone());
            workspace.mention = normalize_text(workspace.mention.clone());
            workspace.watch_dirs = workspace
                .watch_dirs
                .iter()
                .filter_map(|dir| normalize_text(Some(dir.clone())))
                .collect();
            if workspace.watch_dirs.is_empty() {
                workspace.watch_dirs = default_workspace_watch_dirs();
            }
            workspace.events = workspace
                .events
                .iter()
                .filter_map(|event| normalize_text(Some(event.clone())))
                .collect();
            workspace.debounce_ms = workspace.debounce_ms.max(1);
            workspace.poll_interval_secs = workspace.poll_interval_secs.map(|secs| secs.max(1));
        }

        self.discord_watch.gaebal_gajae_user_id =
            normalize_text(Some(self.discord_watch.gaebal_gajae_user_id.clone()))
                .unwrap_or_else(default_gaebal_gajae_user_id);
        self.discord_watch.owner_user_ids = self
            .discord_watch
            .owner_user_ids
            .iter()
            .filter_map(|id| normalize_text(Some(id.clone())))
            .collect();
        self.discord_watch.banned_channel_ids = self
            .discord_watch
            .banned_channel_ids
            .iter()
            .filter_map(|id| normalize_text(Some(id.clone())))
            .collect();
        self.discord_watch.banned_channel_names = self
            .discord_watch
            .banned_channel_names
            .iter()
            .filter_map(|name| {
                normalize_text(Some(name.trim_start_matches('#').to_ascii_lowercase()))
            })
            .collect();

        for job in &mut self.cron.jobs {
            job.id = normalize_text(Some(job.id.clone())).unwrap_or_default();
            job.schedule = normalize_text(Some(job.schedule.clone())).unwrap_or_default();
            job.timezone =
                normalize_text(Some(job.timezone.clone())).unwrap_or_else(default_cron_timezone);
            job.channel = normalize_text(job.channel.clone());
            job.mention = normalize_text(job.mention.clone());
            match &mut job.kind {
                CronJobKind::CustomMessage { message } => {
                    *message = normalize_text(Some(message.clone())).unwrap_or_default();
                }
            }
        }
    }

    fn routes_with_webhooks(&self) -> usize {
        self.routes
            .iter()
            .filter(|route| route.has_any_webhook_target())
            .count()
    }
}

fn is_repo_binding_route(route: &RouteRule, repo: &str) -> bool {
    route.event == "*"
        && route.sink.trim() == "discord"
        && route.slack_webhook.is_none()
        && route.filter.len() == 1
        && route
            .filter
            .get("repo")
            .map(|value| value == repo)
            .unwrap_or(false)
}

fn is_canonical_quickstart_route(route: &RouteRule) -> bool {
    route.event == "*"
        && route.filter.is_empty()
        && route.sink.trim() == "discord"
        && route.channel.is_none()
        && route.slack_webhook.is_none()
        && route.mention.is_none()
        && route.template.is_none()
        && !route.allow_dynamic_tokens
        && route.format.is_none()
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}: ");
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(value.trim_end().to_string())
}

fn prompt_with_default(label: &str, default: Option<&str>) -> Result<String> {
    let value = match default {
        Some(default) => prompt(&format!("{label} [{default}]"))?,
        None => prompt(label)?,
    };

    if value.trim().is_empty() {
        Ok(default.unwrap_or_default().to_string())
    } else {
        Ok(value)
    }
}

fn prompt_format(default: Option<MessageFormat>) -> Result<MessageFormat> {
    let default_value = default.unwrap_or(MessageFormat::Compact);
    let input = prompt(&format!(
        "Format [{}] (compact/alert/inline/raw)",
        default_value.as_str()
    ))?;
    if input.trim().is_empty() {
        return Ok(default_value);
    }
    MessageFormat::from_label(input.trim())
}

const GOOGLE_CALENDAR_API_BASE_URL: &str = "https://www.googleapis.com/calendar/v3";

fn validate_google_calendar_callback_url(callback_url: &str) -> Result<()> {
    let url = Url::parse(callback_url)
        .map_err(|_| "google_calendar.callback_url must be a valid HTTPS URL")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "google_calendar.callback_url must use HTTPS and must not contain userinfo, a query, or a fragment"
                .into(),
        );
    }
    Ok(())
}

fn validate_google_calendar_api_base_url(api_base_url: &str) -> Result<()> {
    let url = Url::parse(api_base_url)
        .map_err(|_| "google_calendar.api_base_url must be a valid Google Calendar API URL")?;
    if is_official_google_calendar_api_url(&url) || is_debug_loopback_http_url(&url, "/calendar/v3")
    {
        return Ok(());
    }
    Err("google_calendar.api_base_url must be https://www.googleapis.com/calendar/v3 (loopback HTTP is allowed only in debug builds)".into())
}

fn is_official_google_calendar_api_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("www.googleapis.com")
        && url.path() == "/calendar/v3"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.port().is_none()
        && url.as_str() == GOOGLE_CALENDAR_API_BASE_URL
}

fn is_debug_loopback_http_url(url: &Url, path: &str) -> bool {
    #[cfg(debug_assertions)]
    {
        url.scheme() == "http"
            && url.path() == path
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.host_str().is_some_and(|host| {
                host.parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
            })
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = (url, path);
        false
    }
}

fn require_regular_config_file(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(format!(
            "config file {} must be a regular file before loading secrets",
            path.display()
        )
        .into());
    }
    Ok(())
}

fn validate_private_regular_config_file(path: &Path) -> Result<()> {
    require_regular_config_file(path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "config file {} must not grant group or other permissions; set mode 0600 before loading secrets",
                path.display()
            )
            .into());
        }
    }
    Ok(())
}

fn write_private_config(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    match fs::symlink_metadata(path) {
        Ok(_) => require_regular_config_file(path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("config path {} must have a valid file name", path.display()))?,
        uuid::Uuid::new_v4()
    ));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn normalize_text(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_private_config_fixture(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
        fs::write(path, contents)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    #[test]
    fn discord_token_source_prefers_env_over_config() {
        let mut config = AppConfig::default();
        config.providers.discord.bot_token = Some("config-token".into());

        assert_eq!(config.discord_token_source_with(|_| None), "config");
        assert_eq!(
            config.effective_token_with(|_| None).as_deref(),
            Some("config-token")
        );

        let token = config.effective_token_with(|name| {
            (name == "DISCORD_TOKEN").then(|| "env-token".to_string())
        });
        assert_eq!(token.as_deref(), Some("env-token"));
        assert_eq!(
            config.discord_token_source_with(|name| {
                (name == "DISCORD_TOKEN").then(|| "env-token".to_string())
            }),
            "env"
        );
    }

    #[test]
    fn discord_token_source_reports_missing_when_unset() {
        let config = AppConfig::default();

        assert_eq!(config.discord_token_source_with(|_| None), "missing");
        assert_eq!(config.effective_token_with(|_| None), None);
    }

    #[test]
    fn provider_discord_token_is_used_when_present() {
        let mut config = AppConfig::default();
        config.providers.discord.bot_token = Some("config-token".into());

        assert_eq!(config.discord_token_source_with(|_| None), "config");
        assert_eq!(
            config.effective_token_with(|_| None).as_deref(),
            Some("config-token")
        );
    }

    #[test]
    fn discord_token_env_shadow_detected_when_env_overrides_config() {
        let mut config = AppConfig::default();
        config.providers.discord.bot_token = Some("config-token".into());

        assert_eq!(
            config.discord_token_env_shadow_with(|name| {
                (name == "DISCORD_TOKEN").then(|| "env-token".to_string())
            }),
            Some("DISCORD_TOKEN")
        );
    }

    #[test]
    fn discord_token_env_shadow_none_without_conflict() {
        let mut config = AppConfig::default();

        // No config token: env wins but nothing is shadowed.
        assert_eq!(
            config.discord_token_env_shadow_with(|name| {
                (name == "DISCORD_TOKEN").then(|| "env-token".to_string())
            }),
            None
        );

        // Config token present but no env token: config wins, no shadow.
        config.providers.discord.bot_token = Some("config-token".into());
        assert_eq!(config.discord_token_env_shadow_with(|_| None), None);
    }

    #[test]
    fn load_or_default_parses_discord_thread_route_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_private_config_fixture(
            &path,
            r#"
[providers.discord]
token = "bot-token"

[[routes]]
event = "session.*"
sink = "discord"
thread = "123456789012345678"
"#,
        )
        .unwrap();

        let config = AppConfig::load_or_default(&path).unwrap();

        assert_eq!(
            config.routes[0].thread.as_deref(),
            Some("123456789012345678")
        );
        assert_eq!(config.routes[0].channel, None);
    }

    #[test]
    fn webhook_route_satisfies_delivery_validation_without_bot_token() {
        let config = AppConfig {
            routes: vec![RouteRule {
                event: "tmux.keyword".into(),
                webhook: Some("https://discord.com/api/webhooks/123/abc".into()),
                ..RouteRule::default()
            }],
            ..AppConfig::default()
        };

        assert!(config.validate().is_ok(), "{:?}", config.validate().err());
    }

    #[test]
    fn catch_all_webhook_with_default_channel_validates_without_bot_token() {
        let config = AppConfig {
            defaults: DefaultsConfig {
                channel: Some("default".into()),
                channel_name: None,
                format: MessageFormat::Compact,
            },
            routes: vec![RouteRule {
                event: "*".into(),
                webhook: Some("https://discord.com/api/webhooks/123/abc".into()),
                ..RouteRule::default()
            }],
            ..AppConfig::default()
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn slack_webhook_route_satisfies_delivery_validation_without_bot_token() {
        let config = AppConfig {
            routes: vec![RouteRule {
                event: "tmux.keyword".into(),
                slack_webhook: Some("https://hooks.slack.com/services/T/B/abc".into()),
                ..RouteRule::default()
            }],
            ..AppConfig::default()
        };

        assert!(config.validate().is_ok());
        assert_eq!(config.webhook_route_count(), 1);
    }

    #[test]
    fn localfile_only_route_satisfies_delivery_validation_without_bot_token() {
        let config = AppConfig {
            routes: vec![RouteRule {
                event: "tmux.keyword".into(),
                sink: "localfile".into(),
                local_path: Some("/tmp/op_pi/events.jsonl".into()),
                ..RouteRule::default()
            }],
            ..AppConfig::default()
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn localfile_route_does_not_bypass_missing_token_for_discord_channel_route() {
        let config = AppConfig {
            routes: vec![
                RouteRule {
                    event: "tmux.keyword".into(),
                    sink: "localfile".into(),
                    local_path: Some("/tmp/op_pi/events.jsonl".into()),
                    ..RouteRule::default()
                },
                RouteRule {
                    event: "git.commit".into(),
                    sink: "discord".into(),
                    channel: Some("ops".into()),
                    ..RouteRule::default()
                },
            ],
            ..AppConfig::default()
        };

        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("missing Discord bot token"));
    }

    #[test]
    fn localfile_route_can_mix_with_discord_webhook_without_bot_token() {
        let config = AppConfig {
            routes: vec![
                RouteRule {
                    event: "tmux.keyword".into(),
                    sink: "localfile".into(),
                    local_path: Some("/tmp/op_pi/events.jsonl".into()),
                    ..RouteRule::default()
                },
                RouteRule {
                    event: "git.commit".into(),
                    sink: "discord".into(),
                    webhook: Some("https://discord.com/api/webhooks/123/abc".into()),
                    ..RouteRule::default()
                },
            ],
            ..AppConfig::default()
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn discord_route_cannot_set_multiple_targets() {
        let config = AppConfig {
            providers: ProvidersConfig {
                discord: DiscordConfig {
                    bot_token: Some("token".into()),
                },
                slack: SlackConfig::default(),
                openclaw: None,
            },
            routes: vec![RouteRule {
                event: "tmux.keyword".into(),
                sink: default_sink_name(),
                channel: Some("123".into()),
                thread: Some("456".into()),
                slack_webhook: None,
                ..RouteRule::default()
            }],
            ..AppConfig::default()
        };

        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("only one Discord target"));
    }

    #[test]
    fn slack_route_cannot_set_channel() {
        let config = AppConfig {
            routes: vec![RouteRule {
                event: "tmux.keyword".into(),
                sink: "slack".into(),
                channel: Some("123".into()),
                webhook: Some("https://hooks.slack.com/services/T/B/abc".into()),
                ..RouteRule::default()
            }],
            ..AppConfig::default()
        };

        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("must set only one Slack target"));
    }

    #[test]
    fn slack_route_can_use_generic_webhook_field() {
        let config = AppConfig {
            routes: vec![RouteRule {
                event: "tmux.keyword".into(),
                sink: "slack".into(),
                webhook: Some("https://hooks.slack.com/services/T/B/abc".into()),
                ..RouteRule::default()
            }],
            ..AppConfig::default()
        };

        assert!(config.validate().is_ok());
        assert_eq!(config.webhook_route_count(), 1);
    }

    #[test]
    fn setup_scaffold_adds_canonical_quickstart_route() {
        let mut config = AppConfig::default();
        config
            .scaffold_webhook_quickstart(" https://discord.com/api/webhooks/123/abc ".into())
            .unwrap();

        assert_eq!(config.routes.len(), 1);
        assert_eq!(config.routes[0].event, "*");
        assert_eq!(
            config.routes[0].webhook.as_deref(),
            Some("https://discord.com/api/webhooks/123/abc")
        );
        assert_eq!(config.routes[0].sink, "discord");
        assert_eq!(config.routes[0].channel, None);
    }

    #[test]
    fn setup_mixed_flag_edits_update_only_owned_nodes() {
        let mut config = AppConfig {
            providers: ProvidersConfig {
                discord: DiscordConfig {
                    bot_token: Some("old-token".into()),
                },
                slack: SlackConfig::default(),
                openclaw: None,
            },
            daemon: DaemonConfig {
                base_url: "http://127.0.0.1:25294".into(),
                ..DaemonConfig::default()
            },
            defaults: DefaultsConfig {
                channel: Some("general".into()),
                channel_name: None,
                format: MessageFormat::Compact,
            },
            routes: vec![RouteRule {
                event: "git.commit".into(),
                channel: Some("eng".into()),
                ..RouteRule::default()
            }],
            monitors: MonitorConfig {
                github_token: Some("gh-token".into()),
                ..MonitorConfig::default()
            },
            ..AppConfig::default()
        };

        config
            .apply_setup_edits(SetupEdits {
                webhook: Some("https://discord.com/api/webhooks/123/new".into()),
                bot_token: Some("new-token".into()),
                default_channel: Some("alerts".into()),
                default_format: Some(MessageFormat::Alert),
                daemon_base_url: Some("http://127.0.0.1:9999".into()),
            })
            .unwrap();

        assert_eq!(
            config.providers.discord.bot_token.as_deref(),
            Some("new-token")
        );
        assert_eq!(config.defaults.channel.as_deref(), Some("alerts"));
        assert_eq!(config.defaults.format, MessageFormat::Alert);
        assert_eq!(config.daemon.base_url, "http://127.0.0.1:9999");
        assert_eq!(config.routes.len(), 2);
        assert_eq!(config.routes[0].event, "git.commit");
        assert_eq!(config.routes[0].channel.as_deref(), Some("eng"));
        assert_eq!(config.monitors.github_token.as_deref(), Some("gh-token"));
        assert_eq!(
            config.routes[1].webhook.as_deref(),
            Some("https://discord.com/api/webhooks/123/new")
        );
    }

    #[test]
    fn setup_non_webhook_edits_do_not_touch_routes() {
        let mut config = AppConfig {
            routes: vec![RouteRule {
                event: "tmux.keyword".into(),
                webhook: Some("https://discord.com/api/webhooks/123/original".into()),
                mention: Some("<@1>".into()),
                ..RouteRule::default()
            }],
            ..AppConfig::default()
        };

        config
            .apply_setup_edits(SetupEdits {
                bot_token: Some("discord-token".into()),
                default_channel: Some("alerts".into()),
                default_format: Some(MessageFormat::Raw),
                daemon_base_url: Some("http://127.0.0.1:4444".into()),
                ..SetupEdits::default()
            })
            .unwrap();

        assert_eq!(config.routes.len(), 1);
        assert_eq!(config.routes[0].event, "tmux.keyword");
        assert_eq!(
            config.routes[0].webhook.as_deref(),
            Some("https://discord.com/api/webhooks/123/original")
        );
        assert_eq!(config.routes[0].mention.as_deref(), Some("<@1>"));
    }

    #[test]
    fn setup_webhook_rerun_updates_only_canonical_quickstart_route() {
        let mut config = AppConfig {
            routes: vec![
                RouteRule {
                    event: "*".into(),
                    webhook: Some("https://discord.com/api/webhooks/123/old".into()),
                    ..RouteRule::default()
                },
                RouteRule {
                    event: "git.commit".into(),
                    webhook: Some("https://discord.com/api/webhooks/123/other".into()),
                    mention: Some("<@1>".into()),
                    ..RouteRule::default()
                },
            ],
            ..AppConfig::default()
        };

        config
            .scaffold_webhook_quickstart("https://discord.com/api/webhooks/123/new".into())
            .unwrap();

        assert_eq!(config.routes.len(), 2);
        assert_eq!(
            config.routes[0].webhook.as_deref(),
            Some("https://discord.com/api/webhooks/123/new")
        );
        assert_eq!(
            config.routes[1].webhook.as_deref(),
            Some("https://discord.com/api/webhooks/123/other")
        );
    }

    #[test]
    fn ambiguous_quickstart_routes_fail_without_mutating_config() {
        let mut config = AppConfig {
            routes: vec![
                RouteRule {
                    event: "*".into(),
                    webhook: Some("https://discord.com/api/webhooks/123/a".into()),
                    ..RouteRule::default()
                },
                RouteRule {
                    event: "*".into(),
                    webhook: Some("https://discord.com/api/webhooks/123/b".into()),
                    ..RouteRule::default()
                },
            ],
            ..AppConfig::default()
        };

        let error = config
            .scaffold_webhook_quickstart("https://discord.com/api/webhooks/123/new".into())
            .unwrap_err()
            .to_string();

        assert!(error.contains("multiple canonical quickstart routes"));
        assert_eq!(config.routes.len(), 2);
        assert_eq!(
            config.routes[0].webhook.as_deref(),
            Some("https://discord.com/api/webhooks/123/a")
        );
        assert_eq!(
            config.routes[1].webhook.as_deref(),
            Some("https://discord.com/api/webhooks/123/b")
        );
    }

    #[test]
    fn setup_edits_require_at_least_one_non_empty_value() {
        let mut config = AppConfig::default();

        let error = config
            .apply_setup_edits(SetupEdits {
                webhook: Some("   ".into()),
                bot_token: Some(" ".into()),
                default_channel: Some(" ".into()),
                daemon_base_url: Some(" ".into()),
                ..SetupEdits::default()
            })
            .unwrap_err()
            .to_string();

        assert!(error.contains("at least one non-empty setup flag"));
    }

    #[test]
    fn config_editor_menu_matches_bounded_preset_contract() {
        assert_eq!(
            CONFIG_EDITOR_MENU_ITEMS,
            [
                "Set Discord bot token",
                "Set daemon base URL",
                "Set default channel",
                "Set default format",
                "Set Discord webhook quickstart route",
                "Save and exit",
                "Exit without saving",
                "Print manual config template hint",
            ]
        );
    }

    #[test]
    fn tmux_session_monitor_defaults_keyword_window_to_thirty_seconds() {
        let session = TmuxSessionMonitor::default();
        assert_eq!(session.keyword_window_secs, 30);
    }

    #[test]
    fn dispatch_config_defaults_ci_batch_window_to_thirty_seconds() {
        let config = AppConfig::default();
        assert_eq!(config.dispatch.ci_batch_window_secs, 30);
    }

    #[test]
    fn dispatch_config_defaults_routine_batch_window_to_five_seconds() {
        let config = AppConfig::default();
        assert_eq!(config.dispatch.routine_batch_window_secs, 5);
        assert_eq!(
            config.dispatch.routine_batch_window(),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn cron_config_defaults_are_backward_compatible() {
        let config = AppConfig::default();
        assert_eq!(config.cron.poll_interval_secs, 30);
        assert!(config.cron.jobs.is_empty());
    }

    #[test]
    fn load_or_default_parses_dispatch_ci_batch_window_secs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_private_config_fixture(
            &path,
            "[providers.discord]\ntoken = \"abc\"\n[dispatch]\nci_batch_window_secs = 90\n",
        )
        .unwrap();

        let config = AppConfig::load_or_default(&path).unwrap();

        assert_eq!(config.dispatch.ci_batch_window_secs, 90);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn load_or_default_parses_dispatch_routine_batch_window_secs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_private_config_fixture(
            &path,
            "[providers.discord]\ntoken = \"abc\"\n[dispatch]\nroutine_batch_window_secs = 9\n",
        )
        .unwrap();

        let config = AppConfig::load_or_default(&path).unwrap();

        assert_eq!(config.dispatch.routine_batch_window_secs, 9);
        assert_eq!(
            config.dispatch.routine_batch_window(),
            Some(Duration::from_secs(9))
        );
        assert!(config.validate().is_ok());
    }

    #[test]
    fn load_or_default_defaults_dispatch_ci_batch_window_when_omitted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_private_config_fixture(&path, "[providers.discord]\ntoken = \"abc\"\n")
            .expect("write config fixture");

        let config = AppConfig::load_or_default(&path).unwrap();

        assert_eq!(config.dispatch.ci_batch_window_secs, 30);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn load_or_default_defaults_routine_batch_window_when_omitted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_private_config_fixture(&path, "[providers.discord]\ntoken = \"abc\"\n")
            .expect("write config fixture");

        let config = AppConfig::load_or_default(&path).unwrap();

        assert_eq!(config.dispatch.routine_batch_window_secs, 5);
        assert_eq!(
            config.dispatch.routine_batch_window(),
            Some(Duration::from_secs(5))
        );
        assert!(config.validate().is_ok());
    }

    #[test]
    fn load_or_default_preserves_zero_dispatch_ci_batch_window_secs_until_validation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_private_config_fixture(
            &path,
            "[providers.discord]\ntoken = \"abc\"\n[dispatch]\nci_batch_window_secs = 0\n",
        )
        .unwrap();

        let config = AppConfig::load_or_default(&path).unwrap();
        assert_eq!(config.dispatch.ci_batch_window_secs, 0);
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("dispatch.ci_batch_window_secs must be at least 1"));
    }

    #[test]
    fn load_or_default_allows_zero_dispatch_routine_batch_window_secs_to_disable_batching() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_private_config_fixture(
            &path,
            "[providers.discord]\ntoken = \"abc\"\n[dispatch]\nroutine_batch_window_secs = 0\n",
        )
        .unwrap();

        let config = AppConfig::load_or_default(&path).unwrap();
        assert_eq!(config.dispatch.routine_batch_window_secs, 0);
        assert_eq!(config.dispatch.routine_batch_window(), None);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn load_or_default_parses_cron_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_private_config_fixture(
            &path,
            r#"[providers.discord]
token = "abc"

[cron]
poll_interval_secs = 15

[[cron.jobs]]
id = "dev-followup"
schedule = "*/30 * * * *"
channel = "ops"
mention = " <@1> "
kind = "custom-message"
message = " ping "
"#,
        )
        .unwrap();

        let config = AppConfig::load_or_default(&path).unwrap();

        assert_eq!(config.cron.poll_interval_secs, 15);
        assert_eq!(config.cron.jobs.len(), 1);
        let job = &config.cron.jobs[0];
        assert_eq!(job.id, "dev-followup");
        assert_eq!(job.schedule, "*/30 * * * *");
        assert_eq!(job.channel.as_deref(), Some("ops"));
        assert_eq!(job.mention.as_deref(), Some("<@1>"));
        assert_eq!(job.timezone, "UTC");
        assert_eq!(job.zero_backlog_suppression_ttl_secs, 60 * 60);
        match &job.kind {
            CronJobKind::CustomMessage { message } => assert_eq!(message, "ping"),
        }
        assert!(config.validate().is_ok());
    }

    #[test]
    fn cron_validation_rejects_duplicate_ids() {
        let config = AppConfig {
            providers: ProvidersConfig {
                discord: DiscordConfig {
                    bot_token: Some("token".into()),
                },
                slack: SlackConfig::default(),
                openclaw: None,
            },
            cron: CronConfig {
                poll_interval_secs: 30,
                jobs: vec![
                    CronJob {
                        id: "dup".into(),
                        schedule: "*/5 * * * *".into(),
                        timezone: "UTC".into(),
                        enabled: true,
                        channel: Some("ops".into()),
                        mention: None,
                        format: None,
                        state_file: None,
                        zero_backlog_suppression_ttl_secs:
                            default_zero_backlog_suppression_ttl_secs(),
                        kind: CronJobKind::CustomMessage {
                            message: "first".into(),
                        },
                    },
                    CronJob {
                        id: "dup".into(),
                        schedule: "0 * * * *".into(),
                        timezone: "UTC".into(),
                        enabled: true,
                        channel: Some("ops".into()),
                        mention: None,
                        format: None,
                        state_file: None,
                        zero_backlog_suppression_ttl_secs:
                            default_zero_backlog_suppression_ttl_secs(),
                        kind: CronJobKind::CustomMessage {
                            message: "second".into(),
                        },
                    },
                ],
            },
            ..AppConfig::default()
        };

        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("duplicate cron job id 'dup'"));
    }

    #[test]
    fn workspace_monitor_defaults_are_backward_compatible() {
        let config: AppConfig = toml::from_str(
            "
[providers.discord]
token = 'discord-token'

[[monitors.workspace]]
path = '/tmp/repo'
",
        )
        .unwrap();

        assert_eq!(config.monitors.workspace.len(), 1);
        let monitor = &config.monitors.workspace[0];
        assert_eq!(monitor.watch_dirs, default_workspace_watch_dirs());
        assert_eq!(monitor.debounce_ms, default_workspace_debounce_ms());
        assert_eq!(monitor.poll_interval_secs, None);
        assert!(!monitor.discover_worktrees);
    }

    #[test]
    fn normalize_trims_workspace_monitor_fields() {
        let mut config = AppConfig::default();
        config.monitors.workspace.push(WorkspaceMonitor {
            path: " /tmp/repo ".into(),
            watch_dirs: vec![" .omx/state ".into(), "".into(), " .omc/state ".into()],
            discover_worktrees: true,
            channel: Some(" 123 ".into()),
            mention: Some(" <@1> ".into()),
            format: Some(MessageFormat::Compact),
            events: vec!["workspace.*".into()],
            poll_interval_secs: Some(5),
            debounce_ms: 2000,
        });

        config.normalize();
        let monitor = &config.monitors.workspace[0];
        assert_eq!(monitor.path, "/tmp/repo");
        assert_eq!(monitor.watch_dirs, vec![".omx/state", ".omc/state"]);
        assert_eq!(monitor.channel.as_deref(), Some("123"));
        assert_eq!(monitor.mention.as_deref(), Some("<@1>"));
    }

    #[test]
    fn workspace_monitor_config_parses_and_normalizes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_private_config_fixture(
            &path,
            format!(
                r#"[providers.discord]
token = "abc"

[[monitors.workspace]]
path = " {} "
watch_dirs = [" .omx/state ", " .omc/state "]
channel = " ops "
mention = " <@1> "
discover_worktrees = true
events = [" workspace.skill.* "]
debounce_ms = 1500
poll_interval_secs = 9
"#,
                dir.path().display()
            ),
        )
        .unwrap();

        let config = AppConfig::load_or_default(&path).unwrap();
        let monitor = &config.monitors.workspace[0];
        assert_eq!(monitor.path, dir.path().display().to_string());
        assert_eq!(monitor.watch_dirs, vec![".omx/state", ".omc/state"]);
        assert_eq!(monitor.channel.as_deref(), Some("ops"));
        assert_eq!(monitor.mention.as_deref(), Some("<@1>"));
        assert!(monitor.discover_worktrees);
        assert_eq!(monitor.events, vec!["workspace.skill.*"]);
        assert_eq!(monitor.debounce_ms, 1500);
        assert_eq!(monitor.poll_interval_secs, Some(9));
    }

    #[test]
    fn config_without_workspace_monitor_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_private_config_fixture(&path, "[providers.discord]\ntoken = \"abc\"\n")
            .expect("write config fixture");

        let config = AppConfig::load_or_default(&path).unwrap();
        assert!(config.monitors.workspace.is_empty());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn discord_thread_monitor_config_parses_and_normalizes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_private_config_fixture(
            &path,
            r#"[providers.discord]
token = "abc"

[[monitors.discord_threads]]
parent_channel = " 1506217431465463929 "
parent_channel_name = " task-request "
mention = " <@1486621536520769547> "
message = " {mention} thread={thread_id} parent={parent_channel} "
poll_interval_secs = 3
"#,
        )
        .unwrap();

        let config = AppConfig::load_or_default(&path).unwrap();
        let monitor = &config.monitors.discord_threads[0];
        assert_eq!(monitor.parent_channel, "1506217431465463929");
        assert_eq!(monitor.parent_channel_name.as_deref(), Some("task-request"));
        assert_eq!(monitor.mention, "<@1486621536520769547>");
        assert_eq!(
            monitor.message.as_deref(),
            Some("{mention} thread={thread_id} parent={parent_channel}")
        );
        assert_eq!(monitor.poll_interval_secs, Some(3));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn discord_thread_monitor_requires_parent_and_mention() {
        let mut config = AppConfig::default();
        config.monitors.discord_threads.push(DiscordThreadMonitor {
            parent_channel: "".into(),
            parent_channel_name: None,
            mention: "".into(),
            message: None,
            poll_interval_secs: Some(1),
        });

        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("discord thread monitor #1 must set parent_channel"));

        config.monitors.discord_threads[0].parent_channel = "1506217431465463929".into();
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("discord thread monitor #1 must set mention"));
    }

    #[test]
    fn default_discord_watch_config_is_empty_and_omitted_from_pretty_toml() {
        let config = AppConfig::default();

        assert!(config.discord_watch.is_empty());
        let toml = config.to_pretty_toml().expect("serialize default config");
        assert!(
            !toml.contains("[discord_watch]"),
            "default local-only watch config should not change generated config shape"
        );
        let round_tripped: AppConfig = toml::from_str(&toml).expect("round-trip default config");
        assert!(round_tripped.discord_watch.is_empty());
    }

    #[test]
    fn linear_characterization_old_toml_without_section_parses() {
        let config: AppConfig = toml::from_str(
            "[[routes]]\nevent = \"custom\"\nsink = \"localfile\"\nlocal_path = \"/tmp/op_pi/events.jsonl\"\n",
        )
        .expect("old TOML without [linear] parses");

        assert!(config.validate().is_ok(), "{:?}", config.validate().err());
    }

    #[test]
    fn linear_characterization_default_pretty_toml_omits_empty_provider_sections() {
        let toml = AppConfig::default()
            .to_pretty_toml()
            .expect("serialize default config");
        assert!(!toml.contains("[aws]"));
        assert!(!toml.contains("[cloudflare]"));
        assert!(!toml.contains("[providers]"));
    }

    #[test]
    fn linear_default_config_is_empty_and_omitted_from_pretty_toml() {
        let config = AppConfig::default();
        assert_eq!(config.linear, LinearConfig::default());
        assert!(config.linear.is_empty());
        assert!(
            !config
                .to_pretty_toml()
                .expect("serialize default config")
                .contains("[linear]")
        );
    }

    #[test]
    fn linear_config_parses_and_normalizes_nonempty_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_private_config_fixture(&path, "[[routes]]\nevent = \"custom\"\nsink = \"localfile\"\nlocal_path = \"/tmp/op_pi/events.jsonl\"\n\n[linear]\nwebhook_secret = \" qa-secret \"\n").unwrap();
        let config = AppConfig::load_or_default(&path).unwrap();
        assert_eq!(config.linear.webhook_secret.as_deref(), Some("qa-secret"));
        assert!(config.validate().is_ok(), "{:?}", config.validate().err());
    }

    #[test]
    fn linear_config_normalizes_blank_secret_to_none_and_omits_from_pretty_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[[routes]]\nevent = \"custom\"\nsink = \"localfile\"\nlocal_path = \"/tmp/op_pi/events.jsonl\"\n\n[linear]\nwebhook_secret = \"   \"\n").unwrap();
        let config = AppConfig::load_or_default(&path).unwrap();
        assert_eq!(config.linear.webhook_secret, None);
        assert!(
            !config
                .to_pretty_toml()
                .expect("serialize normalized config")
                .contains("[linear]")
        );
    }

    #[test]
    fn linear_config_rejects_non_string_secret() {
        let error = toml::from_str::<AppConfig>("[linear]\nwebhook_secret = 42\n")
            .expect_err("non-string Linear webhook_secret must fail");
        assert!(error.to_string().contains("invalid type"));
    }

    #[cfg(unix)]
    #[test]
    fn linear_secret_requires_private_regular_file_and_redacts_display() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "[linear]\nwebhook_secret = \"test-linear-secret\"\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(AppConfig::load_or_default(&path).is_err());

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let config = AppConfig::load_or_default(&path).unwrap();
        let display = config.to_redacted_pretty_toml().unwrap();
        assert!(display.contains("[REDACTED]"));
        assert!(!display.contains("test-linear-secret"));
        assert!(
            config
                .to_pretty_toml()
                .unwrap()
                .contains("test-linear-secret")
        );

        let link = directory.path().join("config-link.toml");
        symlink(&path, &link).unwrap();
        assert!(AppConfig::load_or_default(&link).is_err());
    }

    #[test]
    fn redacted_pretty_toml_hides_all_known_credentials() {
        let mut config = AppConfig::default();
        config.providers.discord.bot_token = Some("discord-fixture".into());
        config.providers.slack.bot_token = Some("slack-fixture".into());
        config.aws.webhook_secret = Some("aws-fixture".into());
        config.cloudflare.webhook_secret = Some("cloudflare-fixture".into());
        config.cloudflare.logpush_secret = Some("logpush-fixture".into());
        config.google_calendar.channel_token = Some("calendar-fixture".into());
        config.linear.webhook_secret = Some("linear-fixture".into());
        config.monitors.github_token = Some("github-fixture".into());
        config.routes.push(RouteRule {
            webhook: Some("https://example.test/bearer-discord".into()),
            slack_webhook: Some("https://example.test/bearer-slack".into()),
            ..RouteRule::default()
        });
        let display = config.to_redacted_pretty_toml().unwrap();
        for value in [
            "discord-fixture",
            "slack-fixture",
            "aws-fixture",
            "cloudflare-fixture",
            "logpush-fixture",
            "calendar-fixture",
            "linear-fixture",
            "github-fixture",
            "bearer-discord",
            "bearer-slack",
        ] {
            assert!(!display.contains(value));
        }
    }

    #[cfg(unix)]
    #[test]
    fn saving_linear_secret_uses_private_atomic_path() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let mut config = AppConfig::default();
        config.linear.webhook_secret = Some("test-linear-secret".into());
        config.save(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn discord_watch_defaults_are_backward_compatible_and_local_only() {
        let config: AppConfig =
            toml::from_str("[[routes]]\nevent = \"custom\"\nsink = \"localfile\"\nlocal_path = \"/tmp/op_pi/events.jsonl\"\n").expect("old config parses");
        assert!(!config.discord_watch.enabled);
        assert!(config.discord_watch.watched_channels.is_empty());
        assert!(config.discord_watch.gaebal_gajae_user_id.is_empty());
        assert!(config.discord_watch.nudge_target_channel_id.is_none());
        assert!(
            config
                .discord_watch
                .banned_channel_names
                .contains(&"omo".to_string())
        );
        assert!(
            config
                .discord_watch
                .banned_channel_names
                .contains(&"omo-help".to_string())
        );
        assert_eq!(config.discord_watch.pending_mentions_threshold, 5);
        assert_eq!(config.discord_watch.direct_mention_persist_ms, 180_000);
        assert_eq!(config.discord_watch.channel_message_threshold, 100);
        assert!(config.validate().is_ok(), "{:?}", config.validate().err());
    }

    #[test]
    fn discord_watch_config_parses_without_discord_delivery_requirements() {
        let config: AppConfig = toml::from_str(
            r#"
[discord_watch]
enabled = true
gaebal_gajae_user_id = "fixture-gaebal"
owner_user_ids = ["fixture-owner"]
channel_cooldown_ms = 60000
global_cooldown_ms = 60000
[[discord_watch.watched_channels]]
id = "fixture-general"
name = "general"
"#,
        )
        .expect("discord_watch config");
        assert!(config.discord_watch.enabled);
        assert_eq!(config.discord_watch.owner_user_ids, vec!["fixture-owner"]);
        assert!(
            config.validate().is_ok(),
            "local-only watch must not require a bot token or route"
        );
    }

    #[test]
    fn discord_watch_custom_tuning_is_preserved_in_pretty_toml() {
        let mut config = AppConfig::default();
        config.discord_watch.pending_mentions_threshold = 7;
        config.discord_watch.doctrine_template = "Sweep <#{channel_id}>".into();

        let toml = config.to_pretty_toml().expect("serialize config");

        assert!(toml.contains("[discord_watch]"));
        assert!(toml.contains("pending_mentions_threshold = 7"));
        assert!(toml.contains("doctrine_template = \"Sweep <#{channel_id}>\""));
    }

    #[test]
    fn google_calendar_channel_token_is_trimmed_and_empty_values_are_removed() {
        let mut config = AppConfig::default();
        config.google_calendar.channel_token = Some("  calendar-secret  ".into());
        config.normalize();
        assert_eq!(
            config.google_calendar.channel_token.as_deref(),
            Some("calendar-secret")
        );

        config.google_calendar.channel_token = Some(" \n ".into());
        config.normalize();
        assert_eq!(config.google_calendar.channel_token, None);
    }

    #[test]
    fn google_calendar_rejects_unsafe_callback_urls_and_api_bases() {
        let mut config = AppConfig {
            google_calendar: GoogleCalendarConfig {
                channel_token: Some("calendar-channel-token-secret-1234".into()),
                credentials_file: Some("/tmp/calendar-oauth.json".into()),
                state_file: Some("/tmp/calendar-state.json".into()),
                callback_url: Some("http://calendar.example.test/google/calendar".into()),
                ..GoogleCalendarConfig::default()
            },
            routes: vec![RouteRule {
                event: "calendar.*".into(),
                sink: "localfile".into(),
                local_path: Some("/tmp/calendar-events.jsonl".into()),
                ..RouteRule::default()
            }],
            ..AppConfig::default()
        };

        for callback_url in [
            "http://calendar.example.test/google/calendar",
            "https://user@calendar.example.test/google/calendar",
            "https://calendar.example.test/google/calendar?token=leak",
            "https://calendar.example.test/google/calendar#fragment",
        ] {
            config.google_calendar.callback_url = Some(callback_url.into());
            let error = config
                .validate()
                .expect_err("unsafe Calendar callback URL must fail");
            assert!(error.to_string().contains("callback_url"));
        }

        config.google_calendar.callback_url =
            Some("https://calendar.example.test/google/calendar".into());
        for api_base_url in [
            "https://www.googleapis.com/calendar/v3/other",
            "https://attacker.example/calendar/v3",
            "https://www.googleapis.com/calendar/v3?redirect=attacker",
            "https://user@www.googleapis.com/calendar/v3",
        ] {
            config.google_calendar.api_base_url = api_base_url.into();
            let error = config
                .validate()
                .expect_err("unsafe Calendar API base URL must fail");
            assert!(error.to_string().contains("api_base_url"));
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn google_calendar_allows_loopback_api_base_only_in_debug_builds() {
        let config = AppConfig {
            google_calendar: GoogleCalendarConfig {
                channel_token: Some("calendar-channel-token-secret-1234".into()),
                credentials_file: Some("/tmp/calendar-oauth.json".into()),
                state_file: Some("/tmp/calendar-state.json".into()),
                callback_url: Some("https://calendar.example.test/google/calendar".into()),
                api_base_url: "http://127.0.0.1:3000/calendar/v3".into(),
                ..GoogleCalendarConfig::default()
            },
            routes: vec![RouteRule {
                event: "calendar.*".into(),
                sink: "localfile".into(),
                local_path: Some("/tmp/calendar-events.jsonl".into()),
                ..RouteRule::default()
            }],
            ..AppConfig::default()
        };

        assert!(config.validate().is_ok());
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn google_calendar_rejects_loopback_api_base_in_release_builds() {
        let config = AppConfig {
            google_calendar: GoogleCalendarConfig {
                channel_token: Some("calendar-channel-token-secret-1234".into()),
                credentials_file: Some("/tmp/calendar-oauth.json".into()),
                state_file: Some("/tmp/calendar-state.json".into()),
                callback_url: Some("https://calendar.example.test/google/calendar".into()),
                api_base_url: "http://127.0.0.1:3000/calendar/v3".into(),
                ..GoogleCalendarConfig::default()
            },
            routes: vec![RouteRule {
                event: "calendar.*".into(),
                sink: "localfile".into(),
                local_path: Some("/tmp/calendar-events.jsonl".into()),
                ..RouteRule::default()
            }],
            ..AppConfig::default()
        };

        let error = config
            .validate()
            .expect_err("release builds must reject loopback Calendar API URLs");
        assert!(error.to_string().contains("api_base_url"));
    }

    #[cfg(unix)]
    #[test]
    fn google_calendar_channel_token_requires_private_permissions_before_load() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = tempfile::tempdir().expect("temporary config directory");
        let path = temp.path().join("config.toml");
        std::fs::write(
            &path,
            "[google_calendar]\nchannel_token = \"calendar-channel-token-secret-1234\"\n",
        )
        .expect("write config fixture");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("set unsafe config permissions");

        let error = AppConfig::load_or_default(&path)
            .expect_err("Calendar channel token config with unsafe permissions must fail");
        assert!(error.to_string().contains("set mode 0600"));
        assert_eq!(
            std::fs::metadata(&path)
                .expect("read config metadata")
                .permissions()
                .mode()
                & 0o777,
            0o644,
            "loading must not chmod a deployed config"
        );

        let invalid_path = temp.path().join("invalid-config.toml");
        std::fs::write(&invalid_path, "[").expect("write invalid config fixture");
        std::fs::set_permissions(&invalid_path, std::fs::Permissions::from_mode(0o644))
            .expect("set invalid fixture permissions");
        let error = AppConfig::load_or_default(&invalid_path)
            .expect_err("invalid non-secret config must report its parse error");
        assert!(!error.to_string().contains("set mode 0600"));

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))
            .expect("make config read-only and private");
        AppConfig::load_or_default(&path)
            .expect("private read-only Calendar config must remain loadable");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("read config metadata")
                .permissions()
                .mode()
                & 0o777,
            0o400,
            "loading must preserve read-only deployment permissions"
        );

        let symlink_path = temp.path().join("config-link.toml");
        symlink(&path, &symlink_path).expect("create config symlink");
        let error = AppConfig::load_or_default(&symlink_path)
            .expect_err("Calendar channel token config symlink must fail");
        assert!(error.to_string().contains("regular file"));
    }

    #[test]
    fn google_calendar_webhook_only_config_keeps_short_token_compatibility() {
        let mut config = AppConfig::default();
        config.google_calendar.channel_token = Some("legacy-webhook-token".into());
        config.routes.push(RouteRule {
            event: "google.calendar.*".into(),
            webhook: Some("https://discord.com/api/webhooks/123/token".into()),
            ..RouteRule::default()
        });

        config
            .validate()
            .expect("PR #17 webhook-only Calendar tokens remain compatible");
    }

    #[test]
    fn google_calendar_durable_sync_channel_token_must_be_at_least_32_bytes() {
        let mut config = AppConfig::default();
        config.google_calendar.channel_token = Some("a".repeat(31));
        config.google_calendar.credentials_file = Some("/tmp/calendar-oauth.json".into());
        config.google_calendar.state_file = Some("/tmp/calendar-state.json".into());
        config.google_calendar.callback_url =
            Some("https://calendar.example.test/google/calendar".into());
        config.routes.push(RouteRule {
            event: "calendar.*".into(),
            sink: "localfile".into(),
            local_path: Some("/tmp/calendar-events.jsonl".into()),
            ..RouteRule::default()
        });

        let error = config
            .validate()
            .expect_err("short durable-sync Calendar channel token must fail validation");
        assert!(error.to_string().contains(
            "google_calendar.channel_token must be at least 32 bytes when durable sync is enabled"
        ));

        config.google_calendar.channel_token = Some("a".repeat(32));
        config
            .validate()
            .expect("32-byte durable-sync Calendar channel token must validate");
    }

    #[test]
    fn google_calendar_renewal_margin_must_be_shorter_than_requested_watch_lifetime() {
        let mut config = AppConfig::default();
        config.google_calendar.renewal_margin_secs = REQUESTED_WATCH_LIFETIME_SECS;

        let error = config
            .validate()
            .expect_err("a full watch lifetime renewal margin must fail");
        assert!(error.to_string().contains(
            "google_calendar.renewal_margin_secs must be less than the requested watch lifetime"
        ));

        config.google_calendar.renewal_margin_secs = REQUESTED_WATCH_LIFETIME_SECS - 1;
        let error = config
            .validate()
            .expect_err("unconfigured delivery should be the next validation error");
        assert!(!error.to_string().contains("renewal_margin_secs"));
    }

    #[cfg(unix)]
    #[test]
    fn non_secret_mode_0644_config_loads_and_saves_without_permission_changes() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temporary config directory");
        let path = temp.path().join("config.toml");
        std::fs::write(&path, "[daemon]\nport = 25295\n").expect("write config");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("make non-secret config readable");

        let config = AppConfig::load_or_default(&path).expect("load non-secret config");
        assert_eq!(config.daemon.port, 25295);
        config.save(&path).expect("save non-secret config");

        assert_eq!(
            std::fs::metadata(&path)
                .expect("read saved config metadata")
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }

    #[cfg(unix)]
    #[test]
    fn saving_calendar_channel_token_uses_mode_0600() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temporary config directory");
        let path = temp.path().join("config.toml");
        std::fs::write(&path, "old config").expect("write existing config");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("make existing config public");
        let mut config = AppConfig::default();
        config.google_calendar.channel_token = Some("calendar-channel-token-secret-1234".into());

        config.save(&path).expect("save private Calendar config");

        assert_eq!(
            std::fs::metadata(&path)
                .expect("read saved config metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn google_calendar_sync_requires_complete_external_credentials_configuration() {
        let mut config = AppConfig::default();
        config.google_calendar.credentials_file = Some("/tmp/calendar-oauth.json".into());
        config.routes.push(RouteRule {
            event: "calendar.*".into(),
            sink: "localfile".into(),
            local_path: Some("/tmp/calendar-events.jsonl".into()),
            ..RouteRule::default()
        });

        let error = config
            .validate()
            .expect_err("incomplete Calendar source config must fail");
        assert!(error.to_string().contains("state_file"));
        assert!(error.to_string().contains("callback_url"));
        assert!(error.to_string().contains("channel_token"));
    }

    fn slack_channel_route(event: &str, channel: Option<&str>) -> RouteRule {
        RouteRule {
            event: event.into(),
            sink: "slack".into(),
            channel: channel.map(str::to_string),
            ..RouteRule::default()
        }
    }

    #[test]
    fn slack_channel_route_valid_with_bot_token() {
        let mut config = AppConfig::default();
        config.providers.slack.bot_token = Some("xoxb-test".into());
        config.routes = vec![slack_channel_route("github.*", Some("C123OPS"))];

        config
            .validate()
            .expect("slack channel route with bot token should validate");
    }

    #[test]
    fn drop_route_satisfies_delivery_validation_without_target() {
        let mut config = AppConfig::default();
        config.providers.discord.bot_token = None;
        config.routes = vec![RouteRule {
            event: "internal.*".to_string(),
            sink: "drop".to_string(),
            ..RouteRule::default()
        }];

        assert!(config.validate().is_ok());
    }

    #[test]
    fn slack_channel_route_requires_bot_token() {
        let config = AppConfig {
            routes: vec![slack_channel_route("github.*", Some("C123OPS"))],
            ..AppConfig::default()
        };

        let error = config
            .validate()
            .expect_err("slack channel route without bot token must fail");
        assert!(error.to_string().contains("missing Slack bot token"));
    }

    #[test]
    fn slack_route_rejects_channel_plus_webhook() {
        let mut config = AppConfig::default();
        config.providers.slack.bot_token = Some("xoxb-test".into());
        let mut route = slack_channel_route("github.*", Some("C123OPS"));
        route.webhook = Some("https://hooks.slack.com/services/T/B/xxx".into());
        config.routes = vec![route];

        let error = config
            .validate()
            .expect_err("channel plus webhook must fail");
        assert!(error.to_string().contains("only one Slack target"));
    }

    #[test]
    fn slack_webhook_route_remains_valid_without_bot_token() {
        let mut config = AppConfig::default();
        let route = RouteRule {
            event: "ci.failed-twice".into(),
            sink: "slack".into(),
            slack_webhook: Some("https://hooks.slack.com/services/T/B/xxx".into()),
            ..RouteRule::default()
        };
        config.routes = vec![route];

        config
            .validate()
            .expect("existing slack webhook routes must keep validating (back-compat)");
    }

    #[test]
    fn slack_route_uses_default_channel_when_configured() {
        let mut config = AppConfig::default();
        config.providers.slack.bot_token = Some("xoxb-test".into());
        config.providers.slack.default_channel = Some("C-DEFAULT".into());
        config.routes = vec![slack_channel_route("github.*", None)];

        config
            .validate()
            .expect("slack route without explicit target should validate via default_channel");
    }

    #[test]
    fn slack_route_without_target_or_default_channel_fails() {
        let mut config = AppConfig::default();
        config.providers.slack.bot_token = Some("xoxb-test".into());
        config.routes = vec![slack_channel_route("github.*", None)];

        let error = config
            .validate()
            .expect_err("slack route with no target and no default channel must fail");
        assert!(error.to_string().contains("default_channel"));
    }

    #[test]
    fn slack_default_channel_route_still_requires_bot_token() {
        let mut config = AppConfig::default();
        config.providers.slack.default_channel = Some("C-DEFAULT".into());
        config.routes = vec![slack_channel_route("github.*", None)];

        let error = config
            .validate()
            .expect_err("default-channel slack route without bot token must fail");
        assert!(error.to_string().contains("missing Slack bot token"));
    }
}
