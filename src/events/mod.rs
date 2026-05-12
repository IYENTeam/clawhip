pub mod normalize;
pub mod constructors;

pub use normalize::normalize_event;

use std::collections::BTreeMap;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::Result;
use crate::render::{DefaultRenderer, Renderer};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum MessageFormat {
    #[default]
    Compact,
    Alert,
    Inline,
    Raw,
}

impl MessageFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Alert => "alert",
            Self::Inline => "inline",
            Self::Raw => "raw",
        }
    }

    pub fn from_label(label: &str) -> Result<Self> {
        match label {
            "compact" => Ok(Self::Compact),
            "alert" => Ok(Self::Alert),
            "inline" => Ok(Self::Inline),
            "raw" => Ok(Self::Raw),
            other => Err(format!("unsupported message format: {other}").into()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct IncomingEvent {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub mention: Option<String>,
    #[serde(default)]
    pub format: Option<MessageFormat>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutingMetadata {
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub repo_name: Option<String>,
    #[serde(default)]
    pub repo_path: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IncomingEventWire {
    #[serde(rename = "type", alias = "kind", alias = "event")]
    kind: String,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    mention: Option<String>,
    #[serde(default)]
    format: Option<MessageFormat>,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    payload: Option<Value>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

impl<'de> Deserialize<'de> for IncomingEvent {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = IncomingEventWire::deserialize(deserializer)?;
        let payload = wire
            .payload
            .unwrap_or_else(|| Value::Object(Map::from_iter(wire.extra)));

        Ok(Self {
            kind: wire.kind,
            channel: wire.channel,
            mention: wire.mention,
            format: wire.format,
            template: wire.template,
            payload,
        })
    }
}

impl IncomingEvent {
    pub fn canonical_kind(&self) -> &str {
        match self.kind.as_str() {
            "issue-opened" => "github.issue-opened",
            "git.pr-status-changed" => "github.pr-status-changed",
            "session-start" | "started" => "session.started",
            "session-idle" | "blocked" => "session.blocked",
            "session-end" | "finished" => "session.finished",
            "failed" => "session.failed",
            "retry-needed" => "session.retry-needed",
            "pr-created" => "session.pr-created",
            "test-started" => "session.test-started",
            "test-finished" => "session.test-finished",
            "test-failed" => "session.test-failed",
            "handoff-needed" => "session.handoff-needed",
            "prompt-submitted" => "session.prompt-submitted",
            "prompt-delivered" => "session.prompt-delivered",
            "prompt-delivery-failed" => "session.prompt-delivery-failed",
            "stopped" => "session.stopped",
            other => other,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn render_default(&self, format: &MessageFormat) -> Result<String> {
        DefaultRenderer.render(self, format)
    }

    pub fn template_context(&self) -> BTreeMap<String, String> {
        let mut context = BTreeMap::new();
        let canonical_kind = self.canonical_kind().to_string();
        if let Some(channel) = self
            .channel
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            context.insert("channel".to_string(), channel.to_string());
            context.insert("channel_hint".to_string(), channel.to_string());
        }
        if let Some(mention) = self
            .mention
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            context.insert("mention".to_string(), mention.to_string());
        }
        if let Some(format) = self.format.as_ref() {
            context.insert("format".to_string(), format.as_str().to_string());
        }
        if let Some(template) = self
            .template
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            context.insert("template".to_string(), template.to_string());
        }
        flatten_json("", &self.payload, &mut context);
        insert_context_aliases(&mut context, &canonical_kind);
        context
    }
}

fn insert_context_aliases(context: &mut BTreeMap<String, String>, canonical_kind: &str) {
    if let Some(payload_event) = context.insert("event".to_string(), canonical_kind.to_string()) {
        context
            .entry("payload_event".to_string())
            .or_insert(payload_event);
    }
    if let Some(payload_contract_event) =
        context.insert("contract_event".to_string(), canonical_kind.to_string())
    {
        context
            .entry("payload_contract_event".to_string())
            .or_insert(payload_contract_event);
    }
    context.insert("kind".to_string(), canonical_kind.to_string());

    insert_context_alias_pair(context, "repo", "repo_name");
    insert_context_alias_pair(context, "session", "session_name");
    insert_context_alias_pair(context, "channel", "channel_hint");

    context
        .entry("route_key".to_string())
        .or_insert_with(|| canonical_kind.to_string());
}

fn insert_context_alias_pair(context: &mut BTreeMap<String, String>, primary: &str, alias: &str) {
    let primary_value = context.get(primary).cloned();
    let alias_value = context.get(alias).cloned();

    match (primary_value, alias_value) {
        (Some(primary_value), None) => {
            context.insert(alias.to_string(), primary_value);
        }
        (None, Some(alias_value)) => {
            context.insert(primary.to_string(), alias_value);
        }
        _ => {}
    }
}

pub fn render_template(template: &str, context: &BTreeMap<String, String>) -> String {
    let mut rendered = template.to_string();
    for (key, value) in context {
        let pattern = format!("{{{key}}}");
        rendered = rendered.replace(&pattern, value);
    }
    rendered
}

fn short_sha(commit: &str) -> String {
    commit.chars().take(7).collect()
}

fn flatten_json(prefix: &str, value: &Value, out: &mut BTreeMap<String, String>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let next = if prefix.is_empty() {
                    key.to_string()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_json(&next, value, out);
            }
        }
        Value::Array(items) => {
            out.insert(
                prefix.to_string(),
                serde_json::to_string(items).unwrap_or_default(),
            );
        }
        Value::String(value) => {
            out.insert(prefix.to_string(), value.clone());
        }
        Value::Bool(value) => {
            out.insert(prefix.to_string(), value.to_string());
        }
        Value::Number(value) => {
            out.insert(prefix.to_string(), value.to_string());
        }
        Value::Null => {
            out.insert(prefix.to_string(), "null".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_template_from_payload() {
        let event = IncomingEvent::github_issue_opened("repo".into(), 42, "broken".into(), None);
        let rendered = render_template("{repo} #{number}: {title}", &event.template_context());
        assert_eq!(rendered, "repo #42: broken");
    }

    #[test]
    fn template_context_backfills_repo_and_session_aliases() {
        let git_event = IncomingEvent::git_commit(
            "clawhip".into(),
            "main".into(),
            "1234567890abcdef".into(),
            "ship it".into(),
            None,
        );
        let git_context = git_event.template_context();
        assert_eq!(git_context.get("repo").map(String::as_str), Some("clawhip"));
        assert_eq!(
            git_context.get("repo_name").map(String::as_str),
            Some("clawhip")
        );
        assert_eq!(
            git_context.get("event").map(String::as_str),
            Some("git.commit")
        );
        assert_eq!(
            git_context.get("contract_event").map(String::as_str),
            Some("git.commit")
        );
        assert_eq!(
            git_context.get("route_key").map(String::as_str),
            Some("git.commit")
        );

        let tmux_event = IncomingEvent::tmux_keyword(
            "issue-132".into(),
            "error".into(),
            "boom".into(),
            Some("alerts".into()),
        );
        let tmux_context = tmux_event.template_context();
        assert_eq!(
            tmux_context.get("session").map(String::as_str),
            Some("issue-132")
        );
        assert_eq!(
            tmux_context.get("session_name").map(String::as_str),
            Some("issue-132")
        );
        assert_eq!(
            tmux_context.get("channel").map(String::as_str),
            Some("alerts")
        );
        assert_eq!(
            tmux_context.get("channel_hint").map(String::as_str),
            Some("alerts")
        );
    }

    #[test]
    fn template_context_preserves_payload_event_without_overwriting_canonical_aliases() {
        let event = normalize_event(IncomingEvent {
            kind: "notify".into(),
            channel: None,
            mention: None,
            format: None,
            template: None,
            payload: json!({
                "event": "test-failed",
                "contract_event": "legacy.test-failed",
                "context": {
                    "normalized_event": "test-failed"
                }
            }),
        });

        let context = event.template_context();
        assert_eq!(event.kind, "session.test-failed");
        assert_eq!(
            context.get("kind").map(String::as_str),
            Some("session.test-failed")
        );
        assert_eq!(
            context.get("event").map(String::as_str),
            Some("session.test-failed")
        );
        assert_eq!(
            context.get("contract_event").map(String::as_str),
            Some("session.test-failed")
        );
        assert_eq!(
            context.get("payload_event").map(String::as_str),
            Some("test-failed")
        );
        assert_eq!(
            context.get("payload_contract_event").map(String::as_str),
            Some("legacy.test-failed")
        );
    }

    #[test]
    fn constructors_default_top_level_mention_to_none() {
        let custom = IncomingEvent::custom(None, "wake up".into());
        assert_eq!(custom.mention, None);

        let keyword = IncomingEvent::tmux_keyword(
            "issue-24".into(),
            "error".into(),
            "boom".into(),
            Some("alerts".into()),
        );
        assert_eq!(keyword.mention, None);
    }

    #[test]
    fn with_mention_sets_top_level_mention() {
        let event = IncomingEvent::tmux_keyword(
            "issue-24".into(),
            "error".into(),
            "boom".into(),
            Some("alerts".into()),
        )
        .with_mention(Some("<@123>".into()));

        assert_eq!(event.mention.as_deref(), Some("<@123>"));
    }

    #[test]
    fn with_repo_context_sets_repo_and_worktree_paths() {
        let event = IncomingEvent::git_commit(
            "repo".into(),
            "main".into(),
            "1234567890abcdef".into(),
            "ship it".into(),
            None,
        )
        .with_repo_context(
            Some("/repo/root".into()),
            Some("/repo/root/.worktrees/issue-115".into()),
        );

        assert_eq!(event.payload["repo_path"], json!("/repo/root"));
        assert_eq!(
            event.payload["worktree_path"],
            json!("/repo/root/.worktrees/issue-115")
        );
    }

    #[test]
    fn deserializes_top_level_mention_field() {
        let event: IncomingEvent = serde_json::from_value(json!({
            "type": "tmux.keyword",
            "channel": "alerts",
            "mention": "<@123>",
            "payload": {
                "session": "issue-24",
                "keyword": "error",
                "line": "boom"
            }
        }))
        .unwrap();

        assert_eq!(event.mention.as_deref(), Some("<@123>"));
        assert_eq!(event.channel.as_deref(), Some("alerts"));
        assert_eq!(event.payload["session"], json!("issue-24"));
    }

    #[test]
    fn constructs_agent_events_with_expected_payload_fields() {
        let started = IncomingEvent::agent_started(
            "worker-1".into(),
            Some("sess-123".into()),
            Some("my-repo".into()),
            None,
            Some("booted".into()),
            Some("<@123>".into()),
            Some("alerts".into()),
        );
        assert_eq!(started.kind, "agent.started");
        assert_eq!(started.channel.as_deref(), Some("alerts"));
        assert_eq!(started.payload["agent_name"], json!("worker-1"));
        assert_eq!(started.payload["session_id"], json!("sess-123"));
        assert_eq!(started.payload["project"], json!("my-repo"));
        assert_eq!(started.payload["status"], json!("started"));
        assert_eq!(started.payload["summary"], json!("booted"));
        assert_eq!(started.payload["mention"], json!("<@123>"));
        assert_eq!(started.payload["elapsed_secs"], json!(null));
        assert_eq!(started.payload["error_message"], json!(null));

        let failed = IncomingEvent::agent_failed(
            "worker-2".into(),
            None,
            Some("my-repo".into()),
            Some(17),
            Some("compile step".into()),
            "build failed".into(),
            None,
            None,
        );
        assert_eq!(failed.kind, "agent.failed");
        assert_eq!(failed.payload["status"], json!("failed"));
        assert_eq!(failed.payload["elapsed_secs"], json!(17));
        assert_eq!(failed.payload["error_message"], json!("build failed"));
    }

    #[test]
    fn renders_agent_started_in_all_formats() {
        let event = IncomingEvent::agent_started(
            "worker-1".into(),
            Some("sess-123".into()),
            Some("my-repo".into()),
            None,
            Some("session began".into()),
            Some("<@123>".into()),
            None,
        );

        assert_eq!(
            event.render_default(&MessageFormat::Compact).unwrap(),
            "<@123> agent worker-1 (started, project=my-repo, session=sess-123, summary=session began)"
        );
        assert_eq!(
            event.render_default(&MessageFormat::Alert).unwrap(),
            "🚨 <@123> agent worker-1 (started, project=my-repo, session=sess-123, summary=session began)"
        );
        assert_eq!(
            event.render_default(&MessageFormat::Inline).unwrap(),
            "<@123> [agent:worker-1] started · project=my-repo · session=sess-123 · session began"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&event.render_default(&MessageFormat::Raw).unwrap())
                .unwrap(),
            json!({
                "agent_name": "worker-1",
                "session_id": "sess-123",
                "project": "my-repo",
                "status": "started",
                "summary": "session began",
                "mention": "<@123>"
            })
        );
    }

    #[test]
    fn renders_agent_blocked_in_all_formats() {
        let event = IncomingEvent::agent_blocked(
            "worker-1".into(),
            Some("sess-123".into()),
            Some("my-repo".into()),
            None,
            Some("waiting for review".into()),
            None,
            None,
        );

        assert_eq!(
            event.render_default(&MessageFormat::Compact).unwrap(),
            "agent worker-1 (blocked, project=my-repo, session=sess-123, summary=waiting for review)"
        );
        assert_eq!(
            event.render_default(&MessageFormat::Alert).unwrap(),
            "🚨 agent worker-1 (blocked, project=my-repo, session=sess-123, summary=waiting for review)"
        );
        assert_eq!(
            event.render_default(&MessageFormat::Inline).unwrap(),
            "[agent:worker-1] blocked · project=my-repo · session=sess-123 · waiting for review"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&event.render_default(&MessageFormat::Raw).unwrap())
                .unwrap(),
            json!({
                "agent_name": "worker-1",
                "session_id": "sess-123",
                "project": "my-repo",
                "status": "blocked",
                "summary": "waiting for review"
            })
        );
    }

    #[test]
    fn renders_agent_finished_in_all_formats() {
        let event = IncomingEvent::agent_finished(
            "worker-1".into(),
            Some("sess-123".into()),
            Some("my-repo".into()),
            Some(300),
            Some("PR created".into()),
            None,
            None,
        );

        assert_eq!(
            event.render_default(&MessageFormat::Compact).unwrap(),
            "agent worker-1 (finished, project=my-repo, session=sess-123, elapsed=300s, summary=PR created)"
        );
        assert_eq!(
            event.render_default(&MessageFormat::Alert).unwrap(),
            "🚨 agent worker-1 (finished, project=my-repo, session=sess-123, elapsed=300s, summary=PR created)"
        );
        assert_eq!(
            event.render_default(&MessageFormat::Inline).unwrap(),
            "[agent:worker-1] finished · project=my-repo · session=sess-123 · elapsed=300s · PR created"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&event.render_default(&MessageFormat::Raw).unwrap())
                .unwrap(),
            json!({
                "agent_name": "worker-1",
                "session_id": "sess-123",
                "project": "my-repo",
                "status": "finished",
                "elapsed_secs": 300,
                "summary": "PR created"
            })
        );
    }

    #[test]
    fn renders_agent_failed_in_all_formats() {
        let event = IncomingEvent::agent_failed(
            "worker-1".into(),
            Some("sess-123".into()),
            Some("my-repo".into()),
            Some(17),
            Some("after test run".into()),
            "build failed".into(),
            None,
            None,
        );

        assert_eq!(
            event.render_default(&MessageFormat::Compact).unwrap(),
            "agent worker-1 (failed, project=my-repo, session=sess-123, elapsed=17s, summary=after test run, error=build failed)"
        );
        assert_eq!(
            event.render_default(&MessageFormat::Alert).unwrap(),
            "🚨 agent worker-1 (failed, project=my-repo, session=sess-123, elapsed=17s, summary=after test run, error=build failed)"
        );
        assert_eq!(
            event.render_default(&MessageFormat::Inline).unwrap(),
            "[agent:worker-1] failed · project=my-repo · session=sess-123 · elapsed=17s · after test run · error: build failed"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&event.render_default(&MessageFormat::Raw).unwrap())
                .unwrap(),
            json!({
                "agent_name": "worker-1",
                "session_id": "sess-123",
                "project": "my-repo",
                "status": "failed",
                "elapsed_secs": 17,
                "summary": "after test run",
                "error_message": "build failed"
            })
        );
    }

    #[test]
    fn renders_github_ci_failed_in_compact_and_alert_formats() {
        let event = IncomingEvent::github_ci(
            "github.ci-failed",
            "clawhip".into(),
            Some(58),
            "CI / test".into(),
            "completed".into(),
            Some("failure".into()),
            "abcdef1234567890".into(),
            "https://github.com/Yeachan-Heo/clawhip/actions/runs/1".into(),
            Some("feat/branch".into()),
            Some("alerts".into()),
        );

        assert_eq!(
            event.render_default(&MessageFormat::Compact).unwrap(),
            "CI failed · clawhip#58 · CI / test · failure · abcdef1 · https://github.com/Yeachan-Heo/clawhip/actions/runs/1"
        );
        assert_eq!(
            event.render_default(&MessageFormat::Alert).unwrap(),
            "🚨 CI failed · clawhip#58 · CI / test · failure · abcdef1 · https://github.com/Yeachan-Heo/clawhip/actions/runs/1"
        );
        assert_eq!(event.channel.as_deref(), Some("alerts"));
    }

    #[test]
    fn renders_github_ci_started_with_status_details() {
        let event = IncomingEvent::github_ci(
            "github.ci-started",
            "clawhip".into(),
            Some(58),
            "CI / test".into(),
            "in_progress".into(),
            None,
            "abcdef1234567890".into(),
            "https://github.com/Yeachan-Heo/clawhip/actions/runs/1".into(),
            None,
            None,
        );

        assert_eq!(
            event.render_default(&MessageFormat::Compact).unwrap(),
            "CI started · clawhip#58 · CI / test · in_progress · abcdef1 · https://github.com/Yeachan-Heo/clawhip/actions/runs/1"
        );
        assert_eq!(
            event.render_default(&MessageFormat::Alert).unwrap(),
            "🚨 CI started · clawhip#58 · CI / test · in_progress · abcdef1 · https://github.com/Yeachan-Heo/clawhip/actions/runs/1"
        );
    }

    #[test]
    fn normalize_event_backfills_agent_emit_status_fields() {
        let event = normalize_event(IncomingEvent {
            kind: "agent.finished".into(),
            channel: None,
            mention: Some("<@123>".into()),
            format: None,
            template: None,
            payload: json!({
                "agent_name": "omc",
                "session_id": "issue-65",
                "project": "clawhip",
                "elapsed_secs": 42
            }),
        });

        assert_eq!(event.kind, "agent.finished");
        assert_eq!(event.payload["status"], json!("finished"));
        assert_eq!(event.payload["tool"], json!("omc"));
        assert_eq!(event.payload["agent_name"], json!("omc"));
    }

    #[test]
    fn normalize_event_adds_ingress_metadata_and_exposes_it_in_template_context() {
        let event = normalize_event(IncomingEvent::agent_started(
            "worker-1".into(),
            Some("sess-123".into()),
            Some("my-repo".into()),
            None,
            Some("booted".into()),
            None,
            None,
        ));
        let context = event.template_context();

        let event_id = event.payload["event_id"].as_str().unwrap();
        assert!(!event_id.is_empty());
        assert_eq!(event.payload["correlation_id"], json!("sess-123"));
        assert!(
            event
                .payload
                .get("first_seen_at")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
        );
        assert_eq!(context.get("event_id").map(String::as_str), Some(event_id));
        assert_eq!(
            context.get("correlation_id").map(String::as_str),
            Some("sess-123")
        );
        assert!(context.contains_key("first_seen_at"));
    }

    #[test]
    fn normalize_event_maps_omx_native_contract_into_session_event() {
        let event = normalize_event(IncomingEvent {
            kind: "notify".into(),
            channel: None,
            mention: None,
            format: None,
            template: None,
            payload: json!({
                "event": "test-failed",
                "timestamp": "2026-03-09T18:07:07.000Z",
                "context": {
                    "normalized_event": "test-failed",
                    "session_name": "issue-65-native-event-contract-polish",
                    "repo_name": "clawhip",
                    "repo_path": "/repo/clawhip",
                    "worktree_path": "/repo/clawhip-worktrees/issue-65",
                    "branch": "feat/issue-65-native-event-contract-polish",
                    "issue_number": 65,
                    "elapsed_secs": 42,
                    "error_summary": "cargo test failed"
                }
            }),
        });

        assert_eq!(event.kind, "session.test-failed");
        assert_eq!(event.payload["tool"], json!("omx"));
        assert_eq!(
            event.payload["session_name"],
            json!("issue-65-native-event-contract-polish")
        );
        assert_eq!(event.payload["repo_name"], json!("clawhip"));
        assert_eq!(event.payload["issue_number"], json!(65));
        assert_eq!(event.payload["elapsed_secs"], json!(42));
        assert_eq!(event.payload["error_message"], json!("cargo test failed"));
        assert_eq!(
            event.payload["event_timestamp"],
            json!("2026-03-09T18:07:07.000Z")
        );
    }

    #[test]
    fn normalize_event_preserves_tmux_pane_metadata_in_payload_and_template_context() {
        let event = normalize_event(IncomingEvent {
            kind: "session-start".into(),
            channel: None,
            mention: None,
            format: None,
            template: None,
            payload: json!({
                "tool": "codex",
                "tmux_session": "issue-180",
                "tmux_window": "2",
                "tmux_pane": "%11",
                "tmux_pane_tty": "/dev/pts/42",
                "tmux_attached": false,
                "tmux_client_count": 0
            }),
        });
        let context = event.template_context();

        assert_eq!(event.kind, "session.started");
        assert_eq!(event.payload["session_name"], json!("issue-180"));
        assert_eq!(event.payload["tmux_session"], json!("issue-180"));
        assert_eq!(event.payload["tmux_window"], json!("2"));
        assert_eq!(event.payload["tmux_pane"], json!("%11"));
        assert_eq!(event.payload["tmux_pane_tty"], json!("/dev/pts/42"));
        assert_eq!(event.payload["tmux_attached"], json!(false));
        assert_eq!(event.payload["tmux_client_count"], json!(0));
        assert_eq!(
            context.get("session").map(String::as_str),
            Some("issue-180")
        );
        assert_eq!(
            context.get("tmux_pane_tty").map(String::as_str),
            Some("/dev/pts/42")
        );
        assert_eq!(
            context.get("tmux_attached").map(String::as_str),
            Some("false")
        );
        assert_eq!(
            context.get("tmux_client_count").map(String::as_str),
            Some("0")
        );
    }

    #[test]
    fn normalize_event_maps_omc_signal_route_key_into_session_event() {
        let event = normalize_event(IncomingEvent {
            kind: "post-tool-use".into(),
            channel: None,
            mention: None,
            format: None,
            template: None,
            payload: json!({
                "timestamp": "2026-03-09T18:01:58.000Z",
                "signal": {
                    "routeKey": "pull-request.created",
                    "phase": "finished",
                    "summary": "https://github.com/Yeachan-Heo/clawhip/pull/67"
                },
                "context": {
                    "sessionId": "issue-65",
                    "projectPath": "/repo/clawhip-worktrees/issue-65",
                    "projectName": "clawhip"
                }
            }),
        });

        assert_eq!(event.kind, "session.pr-created");
        assert_eq!(event.payload["tool"], json!("omc"));
        assert_eq!(event.payload["session_id"], json!("issue-65"));
        assert_eq!(event.payload["project"], json!("clawhip"));
        assert_eq!(event.payload["repo_name"], json!("clawhip"));
        assert_eq!(
            event.payload["repo_path"],
            json!("/repo/clawhip-worktrees/issue-65")
        );
        assert_eq!(
            event.payload["worktree_path"],
            json!("/repo/clawhip-worktrees/issue-65")
        );
        assert_eq!(event.payload["pr_number"], json!(67));
        assert_eq!(
            event.payload["pr_url"],
            json!("https://github.com/Yeachan-Heo/clawhip/pull/67")
        );
        assert_eq!(event.payload["status"], json!("finished"));
    }

    #[test]
    fn normalize_event_maps_omc_native_contract_into_session_event() {
        let event = normalize_event(IncomingEvent {
            kind: "post-tool-use".into(),
            channel: None,
            mention: None,
            format: None,
            template: None,
            payload: json!({
                "timestamp": "2026-03-09T18:01:58.000Z",
                "signal": {
                    "routeKey": "pull-request.created",
                    "toolName": "Bash",
                    "command": "gh pr create",
                    "summary": "https://github.com/Yeachan-Heo/clawhip/pull/71"
                },
                "context": {
                    "sessionId": "issue-65",
                    "projectPath": "/repo/clawhip",
                    "projectName": "clawhip"
                }
            }),
        });

        assert_eq!(event.kind, "session.pr-created");
        assert_eq!(event.payload["tool"], json!("omc"));
        assert_eq!(event.payload["session_id"], json!("issue-65"));
        assert_eq!(event.payload["project"], json!("clawhip"));
        assert_eq!(event.payload["repo_name"], json!("clawhip"));
        assert_eq!(event.payload["repo_path"], json!("/repo/clawhip"));
        assert_eq!(event.payload["worktree_path"], json!("/repo/clawhip"));
        assert_eq!(event.payload["tool_name"], json!("Bash"));
        assert_eq!(event.payload["command"], json!("gh pr create"));
        assert_eq!(
            event.payload["summary"],
            json!("https://github.com/Yeachan-Heo/clawhip/pull/71")
        );
        assert_eq!(event.payload["pr_number"], json!(71));
    }

    #[test]
    fn renders_omc_pr_created_event_using_contract_label() {
        let event = normalize_event(IncomingEvent {
            kind: "post-tool-use".into(),
            channel: None,
            mention: None,
            format: None,
            template: None,
            payload: json!({
                "timestamp": "2026-03-09T18:01:58.000Z",
                "signal": {
                    "routeKey": "pull-request.created",
                    "phase": "finished",
                    "summary": "https://github.com/Yeachan-Heo/clawhip/pull/67"
                },
                "context": {
                    "sessionId": "issue-65",
                    "projectPath": "/repo/clawhip-worktrees/issue-65",
                    "projectName": "clawhip"
                }
            }),
        });

        assert_eq!(
            event.render_default(&MessageFormat::Compact).unwrap(),
            "omc issue-65 pr-created (repo=clawhip, issue=#65, pr=#67, summary=https://github.com/Yeachan-Heo/clawhip/pull/67)"
        );
    }

    #[test]
    fn renders_session_contract_events_in_low_noise_formats() {
        let event = normalize_event(IncomingEvent {
            kind: "pr-created".into(),
            channel: None,
            mention: None,
            format: None,
            template: None,
            payload: json!({
                "context": {
                    "normalized_event": "pr-created",
                    "session_name": "issue-65",
                    "repo_name": "clawhip",
                    "branch": "feat/issue-65-native-event-contract-polish",
                    "issue_number": 65,
                    "pr_number": 71,
                    "pr_url": "https://github.com/Yeachan-Heo/clawhip/pull/71"
                }
            }),
        });

        assert_eq!(
            event.render_default(&MessageFormat::Compact).unwrap(),
            "omx issue-65 pr-created (repo=clawhip, issue=#65, pr=#71, branch=feat/issue-65-native-event-contract-polish)"
        );
        assert_eq!(
            event.render_default(&MessageFormat::Inline).unwrap(),
            "[omx issue-65] pr-created · clawhip · issue #65 · PR #71 · feat/issue-65-native-event-contract-polish"
        );
    }

    #[test]
    fn git_commit_events_keep_single_commit_rendering() {
        let events = IncomingEvent::git_commit_events(
            "repo".into(),
            "main".into(),
            vec![("1234567890abcdef".into(), "ship it".into())],
            Some("alerts".into()),
        );

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].render_default(&MessageFormat::Compact).unwrap(),
            "git:repo@main 1234567 ship it"
        );
        assert_eq!(
            events[0].render_default(&MessageFormat::Alert).unwrap(),
            "🚨 new commit in repo@main: 1234567 ship it"
        );
        assert_eq!(
            events[0].render_default(&MessageFormat::Inline).unwrap(),
            "[git] repo ship it"
        );
        assert_eq!(events[0].channel.as_deref(), Some("alerts"));
    }

    #[test]
    fn git_commit_events_aggregate_multi_commit_pushes() {
        let events = IncomingEvent::git_commit_events(
            "repo".into(),
            "main".into(),
            vec![
                ("1234567890abcdef".into(), "first".into()),
                ("234567890abcdef1".into(), "second".into()),
                ("34567890abcdef12".into(), "third".into()),
            ],
            Some("alerts".into()),
        );

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "git.commit");
        assert_eq!(events[0].payload["summary"], json!("first"));
        assert_eq!(events[0].payload["short_commit"], json!("1234567"));
        assert_eq!(events[0].payload["commit_count"], json!(3));
        assert_eq!(events[0].payload["commits"].as_array().unwrap().len(), 3);
        assert_eq!(
            events[0].render_default(&MessageFormat::Compact).unwrap(),
            "git:repo@main pushed 3 commits:\n- first\n- second\n- third"
        );
    }

    #[test]
    fn aggregated_git_commit_render_truncates_after_first_three_and_last_two() {
        let event = IncomingEvent::git_commit_events(
            "repo".into(),
            "main".into(),
            vec![
                ("1111111111111111".into(), "one".into()),
                ("2222222222222222".into(), "two".into()),
                ("3333333333333333".into(), "three".into()),
                ("4444444444444444".into(), "four".into()),
                ("5555555555555555".into(), "five".into()),
                ("6666666666666666".into(), "six".into()),
            ],
            None,
        )
        .into_iter()
        .next()
        .unwrap();

        assert_eq!(
            event.render_default(&MessageFormat::Compact).unwrap(),
            "git:repo@main pushed 6 commits:\n- one\n- two\n- three\n... and 1 more\n- five\n- six"
        );
        assert_eq!(
            event.render_default(&MessageFormat::Alert).unwrap(),
            "🚨 git:repo@main pushed 6 commits:\n- one\n- two\n- three\n... and 1 more\n- five\n- six"
        );
    }

    #[test]
    fn tmux_keyword_events_aggregate_multi_hit_windows() {
        let event = IncomingEvent::tmux_keywords(
            "issue-24".into(),
            vec![
                ("error".into(), "build failed".into()),
                ("complete".into(), "job complete".into()),
            ],
            Some("alerts".into()),
        );

        assert_eq!(event.kind, "tmux.keyword");
        assert_eq!(event.payload["keyword"], json!("error"));
        assert_eq!(event.payload["line"], json!("build failed"));
        assert_eq!(event.payload["hit_count"], json!(2));
        assert_eq!(event.payload["hits"].as_array().unwrap().len(), 2);
        assert_eq!(
            event.render_default(&MessageFormat::Compact).unwrap(),
            "tmux:issue-24 matched 2 keyword hits:\n- 'error': build failed\n- 'complete': job complete"
        );
        assert_eq!(
            event.render_default(&MessageFormat::Alert).unwrap(),
            "🚨 tmux session issue-24 hit 2 keyword matches:\n- 'error': build failed\n- 'complete': job complete"
        );
        assert_eq!(
            event.render_default(&MessageFormat::Inline).unwrap(),
            "[tmux:issue-24] 'error': build failed · 'complete': job complete"
        );
    }

    #[test]
    fn canonical_kind_maps_prompt_lifecycle_aliases() {
        let cases = [
            ("prompt-submitted", "session.prompt-submitted"),
            ("prompt-delivered", "session.prompt-delivered"),
            ("prompt-delivery-failed", "session.prompt-delivery-failed"),
            ("stopped", "session.stopped"),
        ];

        for (kind, expected) in cases {
            let event = IncomingEvent {
                kind: kind.into(),
                channel: None,
                mention: None,
                format: None,
                template: None,
                payload: json!({}),
            };
            assert_eq!(
                event.canonical_kind(),
                expected,
                "unexpected canonical kind for {kind}"
            );
        }
    }

    #[test]
    fn normalize_event_maps_native_prompt_and_stop_signals() {
        let cases = [
            (
                "user-prompt-submit",
                json!({}),
                "session.prompt-submitted",
                "prompt-submitted",
            ),
            (
                "notify",
                json!({"normalized_event": "prompt-delivered"}),
                "session.prompt-delivered",
                "prompt-delivered",
            ),
            (
                "notify",
                json!({"route_key": "first-prompt-delivery-failed"}),
                "session.prompt-delivery-failed",
                "prompt-delivery-failed",
            ),
            ("stop", json!({}), "session.stopped", "stopped"),
        ];

        for (kind, payload, expected_kind, expected_status) in cases {
            let event = normalize_event(IncomingEvent {
                kind: kind.into(),
                channel: None,
                mention: None,
                format: None,
                template: None,
                payload,
            });
            assert_eq!(event.kind, expected_kind);
            assert_eq!(event.payload["status"], json!(expected_status));
            assert_eq!(event.payload["normalized_event"], json!(expected_status));
        }
    }
}
