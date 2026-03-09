use std::collections::BTreeMap;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

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
    pub fn custom(channel: Option<String>, message: String) -> Self {
        Self {
            kind: "custom".to_string(),
            channel,
            mention: None,
            format: None,
            template: None,
            payload: json!({ "message": message }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn agent_event(
        kind: &str,
        status: &str,
        agent_name: String,
        session_id: Option<String>,
        project: Option<String>,
        elapsed_secs: Option<u64>,
        summary: Option<String>,
        error_message: Option<String>,
        mention: Option<String>,
        channel: Option<String>,
    ) -> Self {
        let mut payload = Map::new();
        payload.insert("agent_name".to_string(), json!(agent_name));
        payload.insert("status".to_string(), json!(status));
        if let Some(session_id) = session_id {
            payload.insert("session_id".to_string(), json!(session_id));
        }
        if let Some(project) = project {
            payload.insert("project".to_string(), json!(project));
        }
        if let Some(elapsed_secs) = elapsed_secs {
            payload.insert("elapsed_secs".to_string(), json!(elapsed_secs));
        }
        if let Some(summary) = summary {
            payload.insert("summary".to_string(), json!(summary));
        }
        if let Some(error_message) = error_message {
            payload.insert("error_message".to_string(), json!(error_message));
        }
        if let Some(mention) = mention {
            payload.insert("mention".to_string(), json!(mention));
        }

        Self {
            kind: kind.to_string(),
            channel,
            mention: None,
            format: None,
            template: None,
            payload: Value::Object(payload),
        }
    }

    pub fn agent_started(
        agent_name: String,
        session_id: Option<String>,
        project: Option<String>,
        elapsed_secs: Option<u64>,
        summary: Option<String>,
        mention: Option<String>,
        channel: Option<String>,
    ) -> Self {
        Self::agent_event(
            "agent.started",
            "started",
            agent_name,
            session_id,
            project,
            elapsed_secs,
            summary,
            None,
            mention,
            channel,
        )
    }

    pub fn agent_blocked(
        agent_name: String,
        session_id: Option<String>,
        project: Option<String>,
        elapsed_secs: Option<u64>,
        summary: Option<String>,
        mention: Option<String>,
        channel: Option<String>,
    ) -> Self {
        Self::agent_event(
            "agent.blocked",
            "blocked",
            agent_name,
            session_id,
            project,
            elapsed_secs,
            summary,
            None,
            mention,
            channel,
        )
    }

    pub fn agent_finished(
        agent_name: String,
        session_id: Option<String>,
        project: Option<String>,
        elapsed_secs: Option<u64>,
        summary: Option<String>,
        mention: Option<String>,
        channel: Option<String>,
    ) -> Self {
        Self::agent_event(
            "agent.finished",
            "finished",
            agent_name,
            session_id,
            project,
            elapsed_secs,
            summary,
            None,
            mention,
            channel,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn agent_failed(
        agent_name: String,
        session_id: Option<String>,
        project: Option<String>,
        elapsed_secs: Option<u64>,
        summary: Option<String>,
        error_message: String,
        mention: Option<String>,
        channel: Option<String>,
    ) -> Self {
        Self::agent_event(
            "agent.failed",
            "failed",
            agent_name,
            session_id,
            project,
            elapsed_secs,
            summary,
            Some(error_message),
            mention,
            channel,
        )
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
            mention: None,
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
            mention: None,
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
            mention: None,
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
            mention: None,
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

    pub fn git_commit_events(
        repo: String,
        branch: String,
        commits: Vec<(String, String)>,
        channel: Option<String>,
    ) -> Vec<Self> {
        let commit_count = commits.len();
        if commit_count == 0 {
            return Vec::new();
        }

        if commit_count == 1 {
            let Some((commit, summary)) = commits.into_iter().next() else {
                return Vec::new();
            };
            return vec![Self::git_commit(repo, branch, commit, summary, channel)];
        }

        let (first_commit, first_summary) = commits[0].clone();
        let commits = commits
            .into_iter()
            .map(|(commit, summary)| {
                let short_commit = short_sha(&commit);
                json!({
                    "commit": commit,
                    "short_commit": short_commit,
                    "summary": summary,
                })
            })
            .collect::<Vec<_>>();

        vec![Self {
            kind: "git.commit".to_string(),
            channel,
            mention: None,
            format: None,
            template: None,
            payload: json!({
                "repo": repo,
                "branch": branch,
                "commit": first_commit.clone(),
                "short_commit": short_sha(&first_commit),
                "summary": first_summary,
                "commit_count": commit_count,
                "commits": commits,
            }),
        }]
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
            mention: None,
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
            mention: None,
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

    #[allow(clippy::too_many_arguments)]
    pub fn github_ci(
        kind: &str,
        repo: String,
        number: Option<u64>,
        workflow: String,
        status: String,
        conclusion: Option<String>,
        sha: String,
        url: String,
        branch: Option<String>,
        channel: Option<String>,
    ) -> Self {
        let mut payload = Map::new();
        payload.insert("repo".to_string(), json!(repo));
        payload.insert("workflow".to_string(), json!(workflow));
        payload.insert("status".to_string(), json!(status));
        payload.insert("sha".to_string(), json!(sha));
        payload.insert("url".to_string(), json!(url));
        if let Some(number) = number {
            payload.insert("number".to_string(), json!(number));
        }
        if let Some(conclusion) = conclusion {
            payload.insert("conclusion".to_string(), json!(conclusion));
        }
        if let Some(branch) = branch {
            payload.insert("branch".to_string(), json!(branch));
        }

        Self {
            kind: kind.to_string(),
            channel,
            mention: None,
            format: None,
            template: None,
            payload: Value::Object(payload),
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
            mention: None,
            format: None,
            template: None,
            payload: json!({
                "session": session,
                "keyword": keyword,
                "line": line,
            }),
        }
    }

    pub fn tmux_keywords(
        session: String,
        hits: Vec<(String, String)>,
        channel: Option<String>,
    ) -> Self {
        if hits.len() <= 1 {
            let Some((keyword, line)) = hits.into_iter().next() else {
                return Self::tmux_keyword(session, String::new(), String::new(), channel);
            };
            return Self::tmux_keyword(session, keyword, line, channel);
        }

        Self::tmux_keyword_aggregated(session, hits, channel)
    }

    pub fn tmux_keyword_aggregated(
        session: String,
        hits: Vec<(String, String)>,
        channel: Option<String>,
    ) -> Self {
        let hit_count = hits.len();
        let (keyword, line) = hits
            .first()
            .cloned()
            .unwrap_or_else(|| (String::new(), String::new()));
        Self {
            kind: "tmux.keyword".to_string(),
            channel,
            mention: None,
            format: None,
            template: None,
            payload: json!({
                "session": session,
                "keyword": keyword,
                "line": line,
                "hit_count": hit_count,
                "hits": hits
                    .into_iter()
                    .map(|(keyword, line)| json!({ "keyword": keyword, "line": line }))
                    .collect::<Vec<_>>(),
            }),
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
            mention: None,
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

    pub fn with_mention(mut self, mention: Option<String>) -> Self {
        self.mention = mention;
        self
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

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn render_default(&self, format: &MessageFormat) -> Result<String> {
        DefaultRenderer.render(self, format)
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
}
