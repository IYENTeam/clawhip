use serde_json::{Map, Value, json};

use super::{IncomingEvent, MessageFormat, RoutingMetadata};

impl IncomingEvent {
    pub fn workspace(kind: String, payload: Value, channel: Option<String>) -> Self {
        Self {
            kind,
            channel,
            mention: None,
            format: None,
            template: None,
            payload,
        }
    }

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
                "short_commit": super::short_sha(&commit),
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
                let short_commit = super::short_sha(&commit);
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
                "short_commit": super::short_sha(&first_commit),
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

    #[allow(clippy::too_many_arguments)]
    pub fn github_release(
        action: &str,
        repo: String,
        tag: String,
        name: String,
        is_prerelease: bool,
        url: String,
        actor: Option<String>,
        channel: Option<String>,
    ) -> Self {
        let kind = match action {
            "prereleased" => "github.release-prereleased",
            "edited" => "github.release-edited",
            _ => "github.release-published",
        };
        let mut payload = Map::new();
        payload.insert("repo".to_string(), json!(repo));
        payload.insert("tag".to_string(), json!(tag));
        payload.insert("name".to_string(), json!(name));
        payload.insert("action".to_string(), json!(action));
        payload.insert("is_prerelease".to_string(), json!(is_prerelease));
        payload.insert("url".to_string(), json!(url));
        if let Some(actor) = actor {
            payload.insert("actor".to_string(), json!(actor));
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

    pub fn with_repo_context(
        mut self,
        repo_path: Option<String>,
        worktree_path: Option<String>,
    ) -> Self {
        if let Some(payload) = self.payload.as_object_mut() {
            if let Some(repo_path) = repo_path.filter(|value| !value.trim().is_empty()) {
                payload.insert("repo_path".to_string(), json!(repo_path));
            }
            if let Some(worktree_path) = worktree_path.filter(|value| !value.trim().is_empty()) {
                payload.insert("worktree_path".to_string(), json!(worktree_path));
            }
        }
        self
    }

    pub fn with_routing_metadata(mut self, routing: &RoutingMetadata) -> Self {
        let Some(payload) = self.payload.as_object_mut() else {
            return self;
        };

        for (key, value) in [
            ("tool", routing.tool.as_deref()),
            ("project", routing.project.as_deref()),
            ("repo_name", routing.repo_name.as_deref()),
            ("repo_path", routing.repo_path.as_deref()),
            ("worktree_path", routing.worktree_path.as_deref()),
            ("session_id", routing.session_id.as_deref()),
            ("branch", routing.branch.as_deref()),
        ] {
            if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
                payload.insert(key.to_string(), json!(value));
            }
        }

        self
    }
}
