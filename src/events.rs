use std::collections::BTreeMap;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::Result;

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
    pub format: Option<MessageFormat>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Deserialize)]
struct IncomingEventWire {
    #[serde(rename = "type", alias = "kind", alias = "event")]
    kind: String,
    #[serde(default)]
    channel: Option<String>,
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
            format: wire.format,
            template: wire.template,
            payload,
        })
    }
}

impl IncomingEvent {
    pub fn custom(channel: Option<String>, message: String) -> Self {
        Self {
            kind: "custom".to_string(),
            channel,
            format: None,
            template: None,
            payload: json!({ "message": message }),
        }
    }

    pub fn github_issue_opened(
        repo: String,
        number: u64,
        title: String,
        channel: Option<String>,
    ) -> Self {
        Self {
            kind: "github.issue-opened".to_string(),
            channel,
            format: None,
            template: None,
            payload: json!({ "repo": repo, "number": number, "title": title }),
        }
    }

    pub fn github_issue_commented(
        repo: String,
        number: u64,
        title: String,
        comments: u64,
        channel: Option<String>,
    ) -> Self {
        Self {
            kind: "github.issue-commented".to_string(),
            channel,
            format: None,
            template: None,
            payload: json!({ "repo": repo, "number": number, "title": title, "comments": comments }),
        }
    }

    pub fn github_issue_closed(
        repo: String,
        number: u64,
        title: String,
        channel: Option<String>,
    ) -> Self {
        Self {
            kind: "github.issue-closed".to_string(),
            channel,
            format: None,
            template: None,
            payload: json!({ "repo": repo, "number": number, "title": title }),
        }
    }

    pub fn git_commit(
        repo: String,
        branch: String,
        commit: String,
        summary: String,
        channel: Option<String>,
    ) -> Self {
        Self {
            kind: "git.commit".to_string(),
            channel,
            format: None,
            template: None,
            payload: json!({
                "repo": repo,
                "branch": branch,
                "commit": commit,
                "short_commit": short_sha(&commit),
                "summary": summary,
            }),
        }
    }

    pub fn git_branch_changed(
        repo: String,
        old_branch: String,
        new_branch: String,
        channel: Option<String>,
    ) -> Self {
        Self {
            kind: "git.branch-changed".to_string(),
            channel,
            format: None,
            template: None,
            payload: json!({
                "repo": repo,
                "old_branch": old_branch,
                "new_branch": new_branch,
            }),
        }
    }

    pub fn github_pr_status_changed(
        repo: String,
        number: u64,
        title: String,
        old_status: String,
        new_status: String,
        url: String,
        channel: Option<String>,
    ) -> Self {
        Self {
            kind: "github.pr-status-changed".to_string(),
            channel,
            format: None,
            template: None,
            payload: json!({
                "repo": repo,
                "number": number,
                "title": title,
                "old_status": old_status,
                "new_status": new_status,
                "url": url,
            }),
        }
    }

    pub fn tmux_keyword(
        session: String,
        keyword: String,
        line: String,
        channel: Option<String>,
    ) -> Self {
        Self {
            kind: "tmux.keyword".to_string(),
            channel,
            format: None,
            template: None,
            payload: json!({ "session": session, "keyword": keyword, "line": line }),
        }
    }

    pub fn tmux_stale(
        session: String,
        pane: String,
        minutes: u64,
        last_line: String,
        channel: Option<String>,
    ) -> Self {
        Self {
            kind: "tmux.stale".to_string(),
            channel,
            format: None,
            template: None,
            payload: json!({
                "session": session,
                "pane": pane,
                "minutes": minutes,
                "last_line": last_line,
            }),
        }
    }

    pub fn with_format(mut self, format: Option<MessageFormat>) -> Self {
        self.format = format;
        self
    }

    pub fn canonical_kind(&self) -> &str {
        match self.kind.as_str() {
            "issue-opened" => "github.issue-opened",
            "git.pr-status-changed" => "github.pr-status-changed",
            other => other,
        }
    }

    pub fn render_default(&self, format: &MessageFormat) -> Result<String> {
        let payload = &self.payload;
        let text = match (self.canonical_kind(), format) {
            ("custom", MessageFormat::Compact | MessageFormat::Inline) => {
                string_field(payload, "message")?
            }
            ("custom", MessageFormat::Alert) => format!("🚨 {}", string_field(payload, "message")?),
            ("custom", MessageFormat::Raw) => serde_json::to_string_pretty(payload)?,

            ("github.issue-opened", MessageFormat::Compact) => format!(
                "{}#{} opened: {}",
                string_field(payload, "repo")?,
                payload.field_u64("number")?,
                string_field(payload, "title")?
            ),
            ("github.issue-opened", MessageFormat::Alert) => format!(
                "🚨 GitHub issue opened in {}: #{} {}",
                string_field(payload, "repo")?,
                payload.field_u64("number")?,
                string_field(payload, "title")?
            ),
            ("github.issue-opened", MessageFormat::Inline) => format!(
                "[GitHub] {}#{} {}",
                string_field(payload, "repo")?,
                payload.field_u64("number")?,
                string_field(payload, "title")?
            ),
            ("github.issue-opened", MessageFormat::Raw) => serde_json::to_string_pretty(payload)?,
            ("github.issue-commented", MessageFormat::Compact) => format!(
                "{}#{} commented ({} comments): {}",
                string_field(payload, "repo")?,
                payload.field_u64("number")?,
                payload.field_u64("comments")?,
                string_field(payload, "title")?
            ),
            ("github.issue-commented", MessageFormat::Alert) => format!(
                "🚨 GitHub issue commented in {}: #{} {}",
                string_field(payload, "repo")?,
                payload.field_u64("number")?,
                string_field(payload, "title")?
            ),
            ("github.issue-commented", MessageFormat::Inline) => format!(
                "[GitHub comment] {}#{} {}",
                string_field(payload, "repo")?,
                payload.field_u64("number")?,
                string_field(payload, "title")?
            ),
            ("github.issue-commented", MessageFormat::Raw) => {
                serde_json::to_string_pretty(payload)?
            }
            ("github.issue-closed", MessageFormat::Compact) => format!(
                "{}#{} closed: {}",
                string_field(payload, "repo")?,
                payload.field_u64("number")?,
                string_field(payload, "title")?
            ),
            ("github.issue-closed", MessageFormat::Alert) => format!(
                "🚨 GitHub issue closed in {}: #{} {}",
                string_field(payload, "repo")?,
                payload.field_u64("number")?,
                string_field(payload, "title")?
            ),
            ("github.issue-closed", MessageFormat::Inline) => format!(
                "[GitHub closed] {}#{} {}",
                string_field(payload, "repo")?,
                payload.field_u64("number")?,
                string_field(payload, "title")?
            ),
            ("github.issue-closed", MessageFormat::Raw) => serde_json::to_string_pretty(payload)?,

            ("git.commit", MessageFormat::Compact) => format!(
                "git:{}@{} {} {}",
                string_field(payload, "repo")?,
                string_field(payload, "branch")?,
                string_field(payload, "short_commit")?,
                string_field(payload, "summary")?
            ),
            ("git.commit", MessageFormat::Alert) => format!(
                "🚨 new commit in {}@{}: {} {}",
                string_field(payload, "repo")?,
                string_field(payload, "branch")?,
                string_field(payload, "short_commit")?,
                string_field(payload, "summary")?
            ),
            ("git.commit", MessageFormat::Inline) => format!(
                "[git] {} {}",
                string_field(payload, "repo")?,
                string_field(payload, "summary")?
            ),
            ("git.commit", MessageFormat::Raw) => serde_json::to_string_pretty(payload)?,

            ("git.branch-changed", MessageFormat::Compact) => format!(
                "git:{} branch changed {} -> {}",
                string_field(payload, "repo")?,
                string_field(payload, "old_branch")?,
                string_field(payload, "new_branch")?
            ),
            ("git.branch-changed", MessageFormat::Alert) => format!(
                "🚨 git repo {} branch changed {} -> {}",
                string_field(payload, "repo")?,
                string_field(payload, "old_branch")?,
                string_field(payload, "new_branch")?
            ),
            ("git.branch-changed", MessageFormat::Inline) => format!(
                "[git:{}] {} -> {}",
                string_field(payload, "repo")?,
                string_field(payload, "old_branch")?,
                string_field(payload, "new_branch")?
            ),
            ("git.branch-changed", MessageFormat::Raw) => serde_json::to_string_pretty(payload)?,

            ("github.pr-status-changed", MessageFormat::Compact) => format!(
                "PR {}#{} {} -> {}: {}",
                string_field(payload, "repo")?,
                payload.field_u64("number")?,
                string_field(payload, "old_status")?,
                string_field(payload, "new_status")?,
                string_field(payload, "title")?
            ),
            ("github.pr-status-changed", MessageFormat::Alert) => format!(
                "🚨 PR status changed in {}: #{} {} -> {} ({})",
                string_field(payload, "repo")?,
                payload.field_u64("number")?,
                string_field(payload, "old_status")?,
                string_field(payload, "new_status")?,
                string_field(payload, "title")?
            ),
            ("github.pr-status-changed", MessageFormat::Inline) => format!(
                "[PR {}#{}] {} -> {}",
                string_field(payload, "repo")?,
                payload.field_u64("number")?,
                string_field(payload, "old_status")?,
                string_field(payload, "new_status")?
            ),
            ("github.pr-status-changed", MessageFormat::Raw) => {
                serde_json::to_string_pretty(payload)?
            }

            ("tmux.keyword", MessageFormat::Compact) => format!(
                "tmux:{} matched '{}' => {}",
                string_field(payload, "session")?,
                string_field(payload, "keyword")?,
                string_field(payload, "line")?
            ),
            ("tmux.keyword", MessageFormat::Alert) => format!(
                "🚨 tmux session {} hit keyword '{}': {}",
                string_field(payload, "session")?,
                string_field(payload, "keyword")?,
                string_field(payload, "line")?
            ),
            ("tmux.keyword", MessageFormat::Inline) => format!(
                "[tmux:{}] {}",
                string_field(payload, "session")?,
                string_field(payload, "line")?
            ),
            ("tmux.keyword", MessageFormat::Raw) => serde_json::to_string_pretty(payload)?,

            ("tmux.stale", MessageFormat::Compact) => format!(
                "tmux:{} pane {} stale for {}m (last: {})",
                string_field(payload, "session")?,
                string_field(payload, "pane")?,
                payload.field_u64("minutes")?,
                string_field(payload, "last_line")?
            ),
            ("tmux.stale", MessageFormat::Alert) => format!(
                "🚨 tmux session {} pane {} stale for {}m (last: {})",
                string_field(payload, "session")?,
                string_field(payload, "pane")?,
                payload.field_u64("minutes")?,
                string_field(payload, "last_line")?
            ),
            ("tmux.stale", MessageFormat::Inline) => format!(
                "[tmux stale:{} {}] {}m",
                string_field(payload, "session")?,
                string_field(payload, "pane")?,
                payload.field_u64("minutes")?
            ),
            ("tmux.stale", MessageFormat::Raw) => serde_json::to_string_pretty(payload)?,

            (_, MessageFormat::Raw) => serde_json::to_string_pretty(payload)?,
            (_, _) => serde_json::to_string(payload)?,
        };
        Ok(text)
    }

    pub fn template_context(&self) -> BTreeMap<String, String> {
        let mut context = BTreeMap::new();
        context.insert("kind".to_string(), self.canonical_kind().to_string());
        flatten_json("", &self.payload, &mut context);
        context
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

pub fn normalize_event(mut event: IncomingEvent) -> IncomingEvent {
    event.kind = event.canonical_kind().to_string();
    if !event.payload.is_object() {
        event.payload = json!({ "value": event.payload });
    }
    event
}

fn string_field(payload: &Value, key: &str) -> Result<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| format!("missing string field '{key}'").into())
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

trait ValueExt {
    fn field_u64(&self, key: &str) -> Result<u64>;
}

impl ValueExt for Value {
    fn field_u64(&self, key: &str) -> Result<u64> {
        self.get(key)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("missing integer field '{key}'").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_template_from_payload() {
        let event = IncomingEvent::github_issue_opened("repo".into(), 42, "broken".into(), None);
        let rendered = render_template("{repo} #{number}: {title}", &event.template_context());
        assert_eq!(rendered, "repo #42: broken");
    }
}
