//! Event normalization — payload metadata extraction and canonical kind mapping.
//!
//! This module handles the "ingress" side of the event pipeline: taking raw
//! `IncomingEvent` values (possibly with abbreviated/legacy kinds and
//! deeply-nested payloads) and enriching them with canonical metadata fields
//! (tool, session_name, repo_name, branch, issue_number, …) stored in the
//! top-level payload object.

use std::path::Path;

use serde_json::{Map, Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use super::IncomingEvent;

// ── Public entry-point ────────────────────────────────────────────────

/// Normalize an incoming event: canonicalize its kind and enrich its payload
/// with ingress metadata (tool, session, repo, branch, …).
pub fn normalize_event(mut event: IncomingEvent) -> IncomingEvent {
    if !event.payload.is_object() {
        event.payload = json!({ "value": event.payload });
    }

    let raw_kind = event.kind.clone();
    let canonical_kind = canonical_event_kind(&event.kind, &event.payload);
    normalize_native_metadata(&mut event.payload, &raw_kind, &canonical_kind);
    event.kind = canonical_kind;
    event
}

// ── Canonical kind resolution ─────────────────────────────────────────

fn canonical_event_kind(kind: &str, payload: &Value) -> String {
    match kind {
        "issue-opened" => "github.issue-opened".to_string(),
        "git.pr-status-changed" => "github.pr-status-changed".to_string(),
        other => native_contract_kind(other, payload).unwrap_or_else(|| other.to_string()),
    }
}

fn native_contract_kind(kind: &str, payload: &Value) -> Option<String> {
    if let Some(route_key) = first_string(
        payload,
        &["/signal/routeKey", "/route_key", "/context/route_key"],
    ) && let Some(mapped) = map_native_signal(route_key.as_str())
    {
        return Some(mapped.to_string());
    }

    if let Some(normalized_event) =
        first_string(payload, &["/context/normalized_event", "/normalized_event"])
        && let Some(mapped) = map_native_signal(normalized_event.as_str())
    {
        return Some(mapped.to_string());
    }

    map_native_signal(kind).map(ToString::to_string)
}

fn map_native_signal(raw: &str) -> Option<&'static str> {
    let normalized = raw.trim().replace('_', "-").to_ascii_lowercase();
    match normalized.as_str() {
        "session-start" | "started" | "session.started" => Some("session.started"),
        "session-idle" | "blocked" | "session.blocked" | "session.idle" | "question.requested" => {
            Some("session.blocked")
        }
        "session-end" | "finished" | "session.finished" => Some("session.finished"),
        "failed" | "session.failed" | "tool.failed" | "pull-request.failed" => {
            Some("session.failed")
        }
        "retry-needed" | "session.retry-needed" => Some("session.retry-needed"),
        "pr-created" | "session.pr-created" | "pull-request.created" => Some("session.pr-created"),
        "test-started" | "session.test-started" | "test.started" => Some("session.test-started"),
        "test-finished" | "session.test-finished" | "test.finished" => {
            Some("session.test-finished")
        }
        "test-failed" | "session.test-failed" | "test.failed" => Some("session.test-failed"),
        "handoff-needed" | "session.handoff-needed" => Some("session.handoff-needed"),
        "stop" | "stopped" | "session.stopped" => Some("session.stopped"),
        "userpromptsubmit"
        | "user-prompt-submit"
        | "user-prompt-submitted"
        | "prompt-submitted"
        | "prompt.submitted"
        | "session.prompt-submitted" => Some("session.prompt-submitted"),
        "prompt-delivered" | "session.prompt-delivered" | "first-prompt-delivered" => {
            Some("session.prompt-delivered")
        }
        "prompt-delivery-failed"
        | "session.prompt-delivery-failed"
        | "first-prompt-delivery-failed" => Some("session.prompt-delivery-failed"),
        _ => None,
    }
}

// ── Payload metadata extraction ───────────────────────────────────────

fn normalize_native_metadata(payload: &mut Value, raw_kind: &str, canonical_kind: &str) {
    let first_seen_at = now_rfc3339();
    let tool = infer_tool(payload);
    let session_name = first_string(
        payload,
        &[
            "/session_name",
            "/context/session_name",
            "/session",
            "/tmuxSession",
            "/tmux_session",
            "/context/tmuxSession",
            "/context/tmux_session",
            "/session_id",
            "/sessionId",
            "/context/session_id",
            "/context/sessionId",
        ],
    );
    let session_id = first_string(
        payload,
        &[
            "/session_id",
            "/sessionId",
            "/context/session_id",
            "/context/sessionId",
            "/sessionId",
            "/session_name",
            "/context/session_name",
        ],
    );
    let project = first_string(
        payload,
        &[
            "/project",
            "/projectName",
            "/project_name",
            "/context/project",
            "/context/projectName",
            "/context/project_name",
        ],
    );
    let repo_name = first_string(
        payload,
        &[
            "/repo_name",
            "/context/repo_name",
            "/projectName",
            "/context/projectName",
        ],
    )
    .or_else(|| {
        first_string(payload, &["/repo_path", "/context/repo_path"]).and_then(|path| {
            Path::new(path.as_str())
                .file_name()
                .and_then(|value| value.to_str())
                .map(ToString::to_string)
        })
    });
    let repo_path = first_string(
        payload,
        &[
            "/repo_path",
            "/context/repo_path",
            "/projectPath",
            "/context/projectPath",
        ],
    );
    let worktree_path = first_string(
        payload,
        &[
            "/worktree_path",
            "/context/worktree_path",
            "/projectPath",
            "/context/projectPath",
        ],
    );
    let branch = first_string(payload, &["/branch", "/context/branch"]);
    let command = first_string(
        payload,
        &["/command", "/context/command", "/signal/command"],
    );
    let tool_name = first_string(
        payload,
        &["/tool_name", "/context/tool_name", "/signal/toolName"],
    );
    let test_runner =
        first_string(payload, &["/test_runner", "/signal/testRunner"]).or_else(|| {
            command
                .as_deref()
                .and_then(infer_test_runner)
                .map(ToString::to_string)
        });
    let elapsed_secs = first_u64(payload, &["/elapsed_secs", "/context/elapsed_secs"]);
    let status = first_string(payload, &["/status", "/context/status", "/signal/phase"])
        .or_else(|| event_status_from_kind(canonical_kind).map(ToString::to_string));
    let summary = first_string(
        payload,
        &[
            "/summary",
            "/signal/summary",
            "/reason",
            "/context/summary",
            "/context/contextSummary",
            "/context/reason",
            "/context/question",
        ],
    );
    let error_message = first_string(
        payload,
        &[
            "/error_message",
            "/error_summary",
            "/context/error_summary",
            "/context/error_message",
        ],
    )
    .or_else(|| {
        canonical_kind
            .ends_with(".failed")
            .then(|| summary.clone())
            .flatten()
    });
    let event_timestamp = first_string(payload, &["/event_timestamp", "/timestamp"]);
    let event_id =
        first_string(payload, &["/event_id"]).unwrap_or_else(|| Uuid::new_v4().to_string());
    let correlation_id = first_string(payload, &["/correlation_id"])
        .or_else(|| {
            [
                session_id.as_deref(),
                session_name.as_deref(),
                project.as_deref(),
                repo_name.as_deref(),
            ]
            .into_iter()
            .flatten()
            .find(|value| !value.trim().is_empty())
            .map(ToString::to_string)
        })
        .unwrap_or_else(|| event_id.clone());
    let route_key = first_string(
        payload,
        &["/route_key", "/signal/routeKey", "/context/route_key"],
    );
    let source = first_string(payload, &["/source"]);
    let tmux_session = first_string(
        payload,
        &[
            "/tmux_session",
            "/tmuxSession",
            "/context/tmux_session",
            "/context/tmuxSession",
            "/tmux/session",
            "/context/tmux/session",
            "/payload/tmux_session",
            "/payload/tmuxSession",
            "/payload/tmux/session",
        ],
    );
    let tmux_window = first_string(
        payload,
        &[
            "/tmux_window",
            "/tmuxWindow",
            "/context/tmux_window",
            "/context/tmuxWindow",
            "/tmux/window",
            "/context/tmux/window",
            "/payload/tmux_window",
            "/payload/tmuxWindow",
            "/payload/tmux/window",
        ],
    );
    let tmux_pane = first_string(
        payload,
        &[
            "/tmux_pane",
            "/tmuxPane",
            "/context/tmux_pane",
            "/context/tmuxPane",
            "/tmux/pane",
            "/context/tmux/pane",
            "/payload/tmux_pane",
            "/payload/tmuxPane",
            "/payload/tmux/pane",
        ],
    );
    let tmux_pane_tty = first_string(
        payload,
        &[
            "/tmux_pane_tty",
            "/tmuxPaneTty",
            "/context/tmux_pane_tty",
            "/context/tmuxPaneTty",
            "/tmux/pane_tty",
            "/tmux/paneTty",
            "/context/tmux/pane_tty",
            "/context/tmux/paneTty",
            "/payload/tmux_pane_tty",
            "/payload/tmuxPaneTty",
            "/payload/tmux/pane_tty",
            "/payload/tmux/paneTty",
        ],
    );
    let tmux_attached = first_boolish(
        payload,
        &[
            "/tmux_attached",
            "/tmuxAttached",
            "/context/tmux_attached",
            "/context/tmuxAttached",
            "/tmux/attached",
            "/context/tmux/attached",
            "/payload/tmux_attached",
            "/payload/tmuxAttached",
            "/payload/tmux/attached",
        ],
    );
    let tmux_client_count = first_u64ish(
        payload,
        &[
            "/tmux_client_count",
            "/tmuxClientCount",
            "/context/tmux_client_count",
            "/context/tmuxClientCount",
            "/tmux/client_count",
            "/tmux/clientCount",
            "/context/tmux/client_count",
            "/context/tmux/clientCount",
            "/payload/tmux_client_count",
            "/payload/tmuxClientCount",
            "/payload/tmux/client_count",
            "/payload/tmux/clientCount",
        ],
    );
    let mut issue_number =
        first_u64(payload, &["/issue_number", "/context/issue_number"]).or_else(|| {
            [
                session_name.as_deref(),
                branch.as_deref(),
                worktree_path.as_deref(),
                command.as_deref(),
            ]
            .into_iter()
            .flatten()
            .find_map(extract_issue_number)
        });
    let mut pr_number = first_u64(payload, &["/pr_number", "/context/pr_number"]);
    let pr_url =
        first_string(payload, &["/pr_url", "/context/pr_url", "/signal/prUrl"]).or_else(|| {
            summary
                .as_ref()
                .filter(|value| extract_pr_number_from_url(value).is_some())
                .cloned()
        });
    if pr_number.is_none() {
        pr_number = pr_url.as_deref().and_then(extract_pr_number_from_url);
    }
    if issue_number.is_none() {
        issue_number = pr_number;
    }

    let Some(object) = payload.as_object_mut() else {
        return;
    };

    if raw_kind != canonical_kind {
        object
            .entry("raw_event".to_string())
            .or_insert_with(|| json!(raw_kind));
    }
    object
        .entry("contract_event".to_string())
        .or_insert_with(|| json!(canonical_kind));

    insert_string_if_missing(object, "tool", tool);
    insert_string_if_missing(object, "event_id", Some(event_id));
    insert_string_if_missing(object, "correlation_id", Some(correlation_id));
    insert_string_if_missing(object, "first_seen_at", Some(first_seen_at));
    if (canonical_kind.starts_with("agent.") || canonical_kind.starts_with("session."))
        && object.get("agent_name").is_none()
        && let Some(tool) = object
            .get("tool")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    {
        object.insert("agent_name".to_string(), json!(tool));
    }
    insert_string_if_missing(object, "session_name", session_name);
    insert_string_if_missing(object, "session_id", session_id);
    insert_string_if_missing(object, "project", project);
    insert_string_if_missing(object, "repo_name", repo_name);
    insert_string_if_missing(object, "repo_path", repo_path);
    insert_string_if_missing(object, "worktree_path", worktree_path);
    insert_string_if_missing(object, "branch", branch);
    insert_u64_if_missing(object, "issue_number", issue_number);
    insert_u64_if_missing(object, "pr_number", pr_number);
    insert_string_if_missing(object, "pr_url", pr_url);
    insert_string_if_missing(object, "command", command);
    insert_string_if_missing(object, "tool_name", tool_name);
    insert_string_if_missing(object, "test_runner", test_runner);
    insert_u64_if_missing(object, "elapsed_secs", elapsed_secs);
    insert_string_if_missing(object, "status", status.clone());
    insert_string_if_missing(object, "normalized_event", status);
    insert_string_if_missing(object, "summary", summary);
    insert_string_if_missing(object, "error_message", error_message);
    insert_string_if_missing(object, "event_timestamp", event_timestamp);
    insert_string_if_missing(object, "route_key", route_key);
    insert_string_if_missing(object, "source", source);
    insert_string_if_missing(object, "tmux_session", tmux_session);
    insert_string_if_missing(object, "tmux_window", tmux_window);
    insert_string_if_missing(object, "tmux_pane", tmux_pane);
    insert_string_if_missing(object, "tmux_pane_tty", tmux_pane_tty);
    insert_bool_if_missing(object, "tmux_attached", tmux_attached);
    insert_u64_if_missing(object, "tmux_client_count", tmux_client_count);
}

// ── Helper functions ──────────────────────────────────────────────────

fn now_rfc3339() -> String {
    let now = OffsetDateTime::now_utc();
    now.format(&Rfc3339)
        .unwrap_or_else(|_| now.unix_timestamp().to_string())
}

fn infer_tool(payload: &Value) -> Option<String> {
    if let Some(tool) = first_string(payload, &["/tool"]) {
        return Some(tool);
    }

    match first_string(payload, &["/agent_name"])
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "omc" | "openclaw" => return Some("omc".to_string()),
        "omx" => return Some("omx".to_string()),
        _ => {}
    }

    if payload.pointer("/signal/routeKey").is_some() {
        return Some("omc".to_string());
    }
    if payload.pointer("/context/normalized_event").is_some() {
        return Some("omx".to_string());
    }

    None
}

fn infer_test_runner(command: &str) -> Option<&'static str> {
    let command = command.to_ascii_lowercase();
    if command.contains("cargo test") {
        Some("cargo-test")
    } else if command.contains("pytest") {
        Some("pytest")
    } else if command.contains("vitest") {
        Some("vitest")
    } else if command.contains("jest") {
        Some("jest")
    } else if command.contains("go test") {
        Some("go-test")
    } else if command.contains("npm test")
        || command.contains("pnpm test")
        || command.contains("yarn test")
        || command.contains("bun test")
    {
        Some("package-test")
    } else {
        None
    }
}

fn event_status_from_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "agent.started" | "session.started" => Some("started"),
        "agent.blocked" | "session.blocked" => Some("blocked"),
        "agent.finished" | "session.finished" => Some("finished"),
        "agent.failed" | "session.failed" => Some("failed"),
        "session.retry-needed" => Some("retry-needed"),
        "session.pr-created" => Some("pr-created"),
        "session.test-started" => Some("test-started"),
        "session.test-finished" => Some("test-finished"),
        "session.test-failed" => Some("test-failed"),
        "session.handoff-needed" => Some("handoff-needed"),
        "session.prompt-submitted" => Some("prompt-submitted"),
        "session.prompt-delivered" => Some("prompt-delivered"),
        "session.prompt-delivery-failed" => Some("prompt-delivery-failed"),
        "session.stopped" => Some("stopped"),
        _ => None,
    }
}

fn first_string(payload: &Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        payload
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn first_u64(payload: &Value, pointers: &[&str]) -> Option<u64> {
    pointers
        .iter()
        .find_map(|pointer| payload.pointer(pointer).and_then(Value::as_u64))
}

fn first_boolish(payload: &Value, pointers: &[&str]) -> Option<bool> {
    pointers.iter().find_map(|pointer| {
        let value = payload.pointer(pointer)?;
        match value {
            Value::Bool(value) => Some(*value),
            Value::Number(value) => value.as_u64().map(|number| number != 0),
            Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "attached" => Some(true),
                "0" | "false" | "no" | "detached" => Some(false),
                _ => None,
            },
            _ => None,
        }
    })
}

fn first_u64ish(payload: &Value, pointers: &[&str]) -> Option<u64> {
    pointers.iter().find_map(|pointer| {
        let value = payload.pointer(pointer)?;
        match value {
            Value::Number(value) => value.as_u64(),
            Value::String(value) => value.trim().parse::<u64>().ok(),
            _ => None,
        }
    })
}

fn insert_string_if_missing(object: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if object.get(key).is_none()
        && let Some(value) = value
    {
        object.insert(key.to_string(), json!(value));
    }
}

fn insert_u64_if_missing(object: &mut Map<String, Value>, key: &str, value: Option<u64>) {
    if object.get(key).is_none()
        && let Some(value) = value
    {
        object.insert(key.to_string(), json!(value));
    }
}

fn insert_bool_if_missing(object: &mut Map<String, Value>, key: &str, value: Option<bool>) {
    if object.get(key).is_none()
        && let Some(value) = value
    {
        object.insert(key.to_string(), json!(value));
    }
}

fn extract_issue_number(text: &str) -> Option<u64> {
    extract_number_after(text, "issue-")
        .or_else(|| extract_number_after(text, "issue/"))
        .or_else(|| extract_number_after(text, "issue#"))
        .or_else(|| extract_number_after(text, "#"))
}

fn extract_pr_number_from_url(url: &str) -> Option<u64> {
    url.split("/pull/").nth(1)?.split('/').next()?.parse().ok()
}

fn extract_number_after(text: &str, marker: &str) -> Option<u64> {
    let text = text.to_ascii_lowercase();
    let marker = marker.to_ascii_lowercase();
    let start = text.find(marker.as_str())? + marker.len();
    let digits = text[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}
