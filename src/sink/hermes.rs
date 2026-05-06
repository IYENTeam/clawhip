//! Hermes sink — delivers normalized events to a Hermes Agent gateway as
//! an OpenAI-compatible run request.
//!
//! Hermes plays the same role OpenClawSink plays: it is the *decision*
//! authority for label-driven IYEN workflows. clawhip routes a GitHub
//! event here; Hermes inspects the issue/PR, decides the lane (auto-fix /
//! declined / review / leave-for-human), and applies the GitHub label
//! itself via tool calling. clawhip never receives the decision back —
//! the lane label re-enters the system through GitHub → clawhip
//! GitHubSource → IyenSystemSink, exactly like the OpenClaw flow.
//!
//! Endpoint shape: Hermes exposes an OpenAI-compatible HTTP API. We use
//! `POST /v1/runs` because:
//!   - it returns `{run_id}` immediately (202) and runs the agent in the
//!     background, matching the fire-and-forget contract clawhip sinks
//!     use today (compare: OpenClawSink::send drops the response body);
//!   - it lets Hermes do tool calling (GitHub label apply) inside the
//!     same run rather than forcing clawhip to parse a streaming reply
//!     and apply the label itself — which would push clawhip out of its
//!     "router only" lane;
//!   - it doesn't tie the decision lifetime to the HTTP request — Hermes
//!     can take as long as it needs without clawhip holding a TCP slot.
//!
//! What this sink deliberately does NOT do:
//!   - parse Hermes's reasoning/decision (we never see it; the label
//!     coming back through GitHub *is* the decision)
//!   - apply GitHub labels (Hermes does that with its own bot identity,
//!     same as OpenClaw)
//!   - retry on Hermes errors (best-effort delivery is the dispatch
//!     contract; if Hermes is down the route just drops, like OpenClaw)

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Value, json};
use std::time::Duration;

use crate::Result;

use super::{Sink, SinkMessage, SinkTarget};

/// Default Hermes skill / system prompt name. Hermes uses `instructions`
/// (in Responses API style) or a configured skill to scope toolsets and
/// behavior. We point at an IYEN-specific skill by default; deployments
/// can override via [`HermesSink::with_instructions`].
const DEFAULT_HERMES_INSTRUCTIONS: &str =
    "You are the IYEN triage decider. You receive a normalized GitHub event \
     payload from clawhip. For issue events, decide one of: \
     attach label `iyen:auto-fix` (delegate to IYENsystem to open a PR), \
     attach label `iyen:declined` (post a rejection comment, then close), \
     or take no action (leave for a human). For pull-request events, \
     decide: attach label `iyen:review` (delegate review to IYENsystem) \
     or take no action. Apply the chosen label via your GitHub tool using \
     the hermes-bot identity. Do not paste the user's prompt back; act, \
     then stop.";

#[derive(Clone)]
pub struct HermesSink {
    client: Client,
    /// Base URL of the Hermes gateway (e.g. `http://127.0.0.1:8000`).
    /// Trailing slash is tolerated — see [`Self::endpoint`].
    base_url: String,
    /// Bearer token for the Hermes gateway. Hermes accepts standard
    /// `Authorization: Bearer …` for OpenAI-compatible endpoints.
    auth_token: String,
    /// IYEN-domain instructions / system prompt. Customize per
    /// deployment by calling [`Self::with_instructions`] before
    /// registering with the dispatcher.
    instructions: String,
    /// Optional model id. None ⇒ Hermes uses its configured default
    /// (matching `hermes chat` behavior).
    model: Option<String>,
}

impl HermesSink {
    pub fn new(base_url: String, auth_token: String) -> Self {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            client,
            base_url,
            auth_token,
            instructions: DEFAULT_HERMES_INSTRUCTIONS.to_string(),
            model: None,
        }
    }

    /// Override the IYEN instructions. Useful when the operator ships a
    /// custom Hermes skill or wants to point at a different prompt.
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = instructions.into();
        self
    }

    /// Pin the model id (e.g. `"openai/gpt-4o"`). When unset, Hermes
    /// resolves the default model from its own configuration.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn is_configured(base_url: &Option<String>, auth_token: &Option<String>) -> bool {
        base_url
            .as_ref()
            .map(|u| !u.trim().is_empty())
            .unwrap_or(false)
            && auth_token
                .as_ref()
                .map(|t| !t.trim().is_empty())
                .unwrap_or(false)
    }

    /// Resolve the `/v1/runs` URL, tolerating an optional trailing slash
    /// on `base_url`. Mirrors [`super::iyensystem::IyenSystemSink::endpoint`].
    fn endpoint(&self) -> String {
        format!("{}/v1/runs", self.base_url.trim_end_matches('/'))
    }
}

/// Build the JSON body for `POST /v1/runs`. Shape matches the OpenAI
/// Responses API: an `instructions` field for the system prompt plus
/// an `input` array of typed message parts. The full clawhip payload is
/// embedded as a JSON string so the model — and any tools it calls —
/// see the same data clawhip routed.
///
/// Why a string instead of structured JSON in `input`:
///   - Responses API's `input` only accepts text/image content parts,
///     not arbitrary JSON
///   - Hermes's tool-calling code sees the same raw payload OpenClaw
///     would have seen, so the IYEN domain prompt can refer to keys
///     like `repo`, `number`, `action` consistently
fn build_hermes_body(message: &SinkMessage, instructions: &str, model: Option<&str>) -> Value {
    let payload_text = serde_json::to_string_pretty(&message.payload)
        .unwrap_or_else(|_| message.payload.to_string());

    let user_text = format!(
        "clawhip event: {}\n\nNormalized payload:\n{}\n\nRendered summary:\n{}",
        message.event_kind, payload_text, message.content
    );

    let mut body = json!({
        "instructions": instructions,
        "input": [
            {
                "role": "user",
                "content": [
                    { "type": "input_text", "text": user_text }
                ]
            }
        ],
        // Hermes's /v1/runs returns 202 + run_id and runs the agent in
        // the background. We do not stream events back; the *decision*
        // re-enters clawhip as a GitHub label change.
        "stream": false,
        // Carry the trace metadata so Hermes can log/correlate without
        // having to parse it out of the user text.
        "metadata": {
            "clawhip_event_kind": message.event_kind,
            "source": "clawhip"
        }
    });

    if let Some(model) = model {
        body["model"] = json!(model);
    }

    body
}

#[async_trait]
impl Sink for HermesSink {
    async fn send(&self, _target: &SinkTarget, message: &SinkMessage) -> Result<()> {
        let url = self.endpoint();
        let body = build_hermes_body(message, &self.instructions, self.model.as_deref());

        eprintln!(
            "clawhip hermes sink: event_kind={} url={}",
            message.event_kind, url
        );

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.auth_token))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Hermes request to {url} failed: {e}"))?;

        // Hermes returns 202 for accepted background runs; some configs
        // may return 200. Anything else (4xx/5xx) is a delivery error.
        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read body>".to_string());
            return Err(format!("Hermes POST /v1/runs failed: {status} — {body}").into());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::MessageFormat;

    fn message(kind: &str, payload: Value) -> SinkMessage {
        SinkMessage {
            event_kind: kind.into(),
            format: MessageFormat::Compact,
            content: "rendered summary line".into(),
            payload,
        }
    }

    #[test]
    fn is_configured_requires_both_url_and_token() {
        assert!(!HermesSink::is_configured(&None, &None));
        assert!(!HermesSink::is_configured(
            &Some("http://localhost:8000".into()),
            &None
        ));
        assert!(!HermesSink::is_configured(&None, &Some("tok".into())));
        assert!(HermesSink::is_configured(
            &Some("http://localhost:8000".into()),
            &Some("tok".into())
        ));
        // Whitespace-only values are treated as unset, matching the
        // sibling sink behavior.
        assert!(!HermesSink::is_configured(
            &Some("   ".into()),
            &Some("tok".into())
        ));
        assert!(!HermesSink::is_configured(
            &Some("http://localhost:8000".into()),
            &Some("".into())
        ));
    }

    #[test]
    fn endpoint_strips_trailing_slash_and_appends_v1_runs() {
        let sink = HermesSink::new("http://127.0.0.1:8000/".into(), "tok".into());
        assert_eq!(sink.endpoint(), "http://127.0.0.1:8000/v1/runs");
        let sink2 = HermesSink::new("http://127.0.0.1:8000".into(), "tok".into());
        assert_eq!(sink2.endpoint(), "http://127.0.0.1:8000/v1/runs");
    }

    #[test]
    fn body_carries_instructions_and_input_text_with_payload() {
        let payload = json!({
            "repo": "Org/Repo",
            "number": 42,
            "title": "broken",
            "sender": {"login": "alice"}
        });
        let msg = message("github.issue-opened", payload.clone());
        let body = build_hermes_body(&msg, "test instructions", None);

        assert_eq!(body["instructions"], "test instructions");
        assert_eq!(body["stream"], false);
        assert_eq!(body["metadata"]["clawhip_event_kind"], "github.issue-opened");
        assert_eq!(body["metadata"]["source"], "clawhip");

        // `model` is omitted when the caller did not pin one — Hermes
        // falls back to its configured default.
        assert!(body.get("model").is_none());

        // The user-text MUST contain the event kind, payload, and
        // rendered summary so the agent sees everything the dispatcher
        // had access to.
        let user_text = body["input"][0]["content"][0]["text"]
            .as_str()
            .expect("input[0].content[0].text must be a string");
        assert!(user_text.contains("github.issue-opened"));
        assert!(user_text.contains("\"repo\": \"Org/Repo\""));
        assert!(user_text.contains("rendered summary line"));
    }

    #[test]
    fn body_includes_model_when_pinned() {
        let msg = message("github.issue-opened", json!({}));
        let body = build_hermes_body(&msg, "", Some("openai/gpt-4o"));
        assert_eq!(body["model"], "openai/gpt-4o");
    }

    #[test]
    fn with_instructions_overrides_default_prompt() {
        let sink = HermesSink::new("http://x".into(), "t".into())
            .with_instructions("custom IYEN prompt");
        assert_eq!(sink.instructions, "custom IYEN prompt");
        // The default constant must NOT leak when an override is set —
        // otherwise operators couldn't ship custom skills.
        assert_ne!(sink.instructions, DEFAULT_HERMES_INSTRUCTIONS);
    }

    #[test]
    fn default_instructions_mention_iyen_label_set() {
        // This is a contract test: the default prompt must reference
        // every label IYEN's workflows trigger on. If a new lane label
        // is added (e.g. `iyen:hold`), this test forces an update to
        // the default instructions.
        assert!(DEFAULT_HERMES_INSTRUCTIONS.contains("iyen:auto-fix"));
        assert!(DEFAULT_HERMES_INSTRUCTIONS.contains("iyen:declined"));
        assert!(DEFAULT_HERMES_INSTRUCTIONS.contains("iyen:review"));
    }
}
