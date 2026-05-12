use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::sleep;

use crate::Result;
use crate::config::{AppConfig, GitRepoMonitor};
use crate::events::IncomingEvent;
use crate::source::Source;
use crate::source::git::{GitSnapshot, repo_display_name, snapshot_git_repo};
use crate::telemetry;

pub struct GitHubSource {
    config: Arc<AppConfig>,
}

impl GitHubSource {
    pub fn new(config: Arc<AppConfig>) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl Source for GitHubSource {
    fn name(&self) -> &str {
        "github"
    }

    async fn run(&self, tx: mpsc::Sender<IncomingEvent>) -> Result<()> {
        let github_client = match build_github_client(self.config.monitor_github_token()) {
            Ok(client) => Some(client),
            Err(error) => {
                eprintln!("clawhip source github: failed to build GitHub client: {error}");
                None
            }
        };

        let state_path = self.config.monitors.github_monitor_state_path.clone();
        let reconciliation_interval = self.config.monitors.reconciliation_interval_polls;

        let mut state = load_state(&state_path).await;
        let state_was_restored = !state.is_empty() && state_path.is_some();
        let mut poll_count: u64 = 0;

        loop {
            run_github_poll_cycle(
                self.config.as_ref(),
                github_client.as_ref(),
                &tx,
                &mut state,
                state_was_restored,
            )
            .await;

            poll_count += 1;

            if reconciliation_interval > 0 && poll_count % reconciliation_interval == 0 {
                if let Some(client) = github_client.as_ref() {
                    if let Err(error) =
                        run_reconciliation(self.config.as_ref(), client, &tx, &state).await
                    {
                        eprintln!("clawhip source github reconciliation failed: {error}");
                    }
                }
            }

            if let Err(error) = save_state(&state_path, &state).await {
                eprintln!("clawhip source github failed to save state: {error}");
            }

            sleep(Duration::from_secs(
                self.config.monitors.poll_interval_secs.max(1),
            ))
            .await;
        }
    }
}

/// Persist the GitHub monitor state to a JSON file.
async fn save_state(
    path: &Option<PathBuf>,
    state: &HashMap<String, GitHubRepoState>,
) -> Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let json = serde_json::to_string_pretty(state)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, json).await?;
    Ok(())
}

/// Load the GitHub monitor state from a JSON file.
async fn load_state(path: &Option<PathBuf>) -> HashMap<String, GitHubRepoState> {
    let Some(path) = path else {
        return HashMap::new();
    };
    match tokio::fs::read_to_string(path).await {
        Ok(content) => {
            if content.trim().is_empty() {
                return HashMap::new();
            }
            serde_json::from_str(&content).unwrap_or_else(|error| {
                eprintln!(
                    "clawhip source github: failed to parse state file at {}: {error}",
                    path.display()
                );
                HashMap::new()
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(error) => {
            eprintln!(
                "clawhip source github: failed to read state file at {}: {error}",
                path.display()
            );
            HashMap::new()
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct GitHubRepoState {
    issues: HashMap<u64, IssueSnapshot>,
    prs: HashMap<u64, PullRequestSnapshot>,
    ci: HashMap<String, GitHubCISnapshot>,
}

#[derive(Clone, Serialize, Deserialize)]
struct IssueSnapshot {
    title: String,
    state: String,
    comments: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct PullRequestSnapshot {
    title: String,
    status: String,
    url: String,
    head_branch: String,
    head_sha: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct GitHubCISnapshot {
    pr_number: Option<u64>,
    workflow: String,
    status: String,
    conclusion: Option<String>,
    sha: String,
    url: String,
    branch: Option<String>,
    run_id: Option<String>,
    run_job_count: usize,
    run_all_terminal: bool,
}

impl GitHubCISnapshot {
    fn dedupe_key(&self) -> String {
        if let Some(run_id) = &self.run_id {
            return format!("run:{run_id}:{}", self.workflow);
        }
        format!(
            "{}:{}:{}",
            self.pr_number
                .map(|number| number.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.sha,
            self.workflow
        )
    }

    fn event_kind(&self) -> &'static str {
        classify_ci_event_kind(&self.status, self.conclusion.as_deref())
    }
}

async fn run_github_poll_cycle(
    config: &AppConfig,
    github_client: Option<&reqwest::Client>,
    tx: &mpsc::Sender<IncomingEvent>,
    state: &mut HashMap<String, GitHubRepoState>,
    state_was_restored: bool,
) {
    if let Err(error) = poll_github(config, github_client, tx, state, state_was_restored).await {
        telemetry::emit(source_record(
            telemetry::event_name::SOURCE_DEGRADED,
            "source_poll_failed",
            None,
            Some(error.to_string()),
        ));
        eprintln!("clawhip source github poll failed: {error}");
    }
}

async fn snapshot_github_repo(repo: &GitRepoMonitor) -> Result<GitSnapshot> {
    match snapshot_git_repo(repo).await {
        Ok(snapshot) => Ok(snapshot),
        Err(error) => match repo.github_repo.clone() {
            Some(github_repo) => {
                telemetry::emit(source_record(
                    telemetry::event_name::SOURCE_INVENTORY,
                    "source_snapshot_fallback",
                    Some(&repo.path),
                    Some(error.to_string()),
                ));
                eprintln!(
                    "clawhip source github snapshot failed for {}: {error}; using configured github_repo={github_repo}",
                    repo.path
                );
                Ok(GitSnapshot {
                    repo_name: repo_display_name(repo),
                    repo_path: repo.path.clone(),
                    worktree_path: repo.path.clone(),
                    branch: String::new(),
                    head: String::new(),
                    commits: Vec::new(),
                    github_repo: Some(github_repo),
                })
            }
            None => Err(error),
        },
    }
}

async fn poll_github(
    config: &AppConfig,
    github_client: Option<&reqwest::Client>,
    tx: &mpsc::Sender<IncomingEvent>,
    state: &mut HashMap<String, GitHubRepoState>,
    state_was_restored: bool,
) -> Result<()> {
    for repo in &config.monitors.git.repos {
        if !repo.emit_issue_opened && !repo.emit_pr_status {
            continue;
        }

        let snapshot = match snapshot_github_repo(repo).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                telemetry::emit(source_record(
                    telemetry::event_name::SOURCE_DEGRADED,
                    "source_snapshot_failed",
                    Some(&repo.path),
                    Some(error.to_string()),
                ));
                eprintln!(
                    "clawhip source github snapshot failed for {}: {error}",
                    repo.path
                );
                continue;
            }
        };

        let previous = state.get(&repo.path);
        let is_new_repo = state_was_restored && previous.is_none();

        let issues = match poll_issues(config, github_client, repo, &snapshot, previous, tx).await {
            Ok(issues) => issues,
            Err(error) => {
                eprintln!(
                    "clawhip source GitHub issue processing failed for {}: {error}",
                    repo.path
                );
                previous
                    .map(|entry| entry.issues.clone())
                    .unwrap_or_default()
            }
        };

        if is_new_repo {
            backfill_issues(repo, &snapshot, &issues, tx).await?;
        }

        let prs =
            match poll_pull_requests(config, github_client, repo, &snapshot, previous, tx).await {
                Ok(prs) => prs,
                Err(error) => {
                    eprintln!(
                        "clawhip source GitHub pull request processing failed for {}: {error}",
                        repo.path
                    );
                    previous.map(|entry| entry.prs.clone()).unwrap_or_default()
                }
            };

        if is_new_repo {
            backfill_prs(repo, &snapshot, &prs, tx).await?;
        }

        let ci = match poll_ci_statuses(config, github_client, repo, &snapshot, previous, &prs, tx)
            .await
        {
            Ok(ci) => ci,
            Err(error) => {
                eprintln!(
                    "clawhip source GitHub CI processing failed for {}: {error}",
                    repo.path
                );
                previous.map(|entry| entry.ci.clone()).unwrap_or_default()
            }
        };

        state.insert(repo.path.clone(), GitHubRepoState { issues, prs, ci });
    }

    Ok(())
}

async fn backfill_issues(
    repo: &GitRepoMonitor,
    snapshot: &GitSnapshot,
    issues: &HashMap<u64, IssueSnapshot>,
    tx: &mpsc::Sender<IncomingEvent>,
) -> Result<()> {
    for (number, issue) in issues.iter().filter(|(_, issue)| issue.state == "open") {
        let mut event = IncomingEvent::github_issue_opened(
            snapshot.repo_name.clone(),
            *number,
            issue.title.clone(),
            repo.channel.clone(),
        )
        .with_mention(repo.mention.clone())
        .with_format(repo.format.clone());
        if let Some(payload) = event.payload.as_object_mut() {
            payload.insert("source".to_string(), json!("backfill"));
        }
        send_event(tx, event).await?;
        eprintln!(
            "clawhip source github backfill: issued issue #{} for {}",
            number, snapshot.repo_name
        );
    }
    Ok(())
}

async fn backfill_prs(
    repo: &GitRepoMonitor,
    snapshot: &GitSnapshot,
    prs: &HashMap<u64, PullRequestSnapshot>,
    tx: &mpsc::Sender<IncomingEvent>,
) -> Result<()> {
    for (number, pr) in prs.iter().filter(|(_, pr)| pr.status == "open") {
        let mut event = IncomingEvent::github_pr_status_changed(
            snapshot.repo_name.clone(),
            *number,
            pr.title.clone(),
            "<backfill>".to_string(),
            pr.status.clone(),
            pr.url.clone(),
            repo.channel.clone(),
        )
        .with_mention(repo.mention.clone())
        .with_format(repo.format.clone());
        if let Some(payload) = event.payload.as_object_mut() {
            payload.insert("source".to_string(), json!("backfill"));
        }
        send_event(tx, event).await?;
        eprintln!(
            "clawhip source github backfill: issued PR #{} for {}",
            number, snapshot.repo_name
        );
    }
    Ok(())
}

async fn poll_issues(
    config: &AppConfig,
    github_client: Option<&reqwest::Client>,
    repo: &GitRepoMonitor,
    snapshot: &GitSnapshot,
    previous: Option<&GitHubRepoState>,
    tx: &mpsc::Sender<IncomingEvent>,
) -> Result<HashMap<u64, IssueSnapshot>> {
    if !repo.emit_issue_opened {
        return Ok(previous
            .map(|entry| entry.issues.clone())
            .unwrap_or_default());
    }

    let Some(client) = github_client else {
        return Ok(previous
            .map(|entry| entry.issues.clone())
            .unwrap_or_default());
    };

    match fetch_issues(client, &config.monitors.github_api_base, repo, snapshot).await {
        Ok(issues) => {
            if let Some(previous) = previous {
                for event in
                    collect_issue_events(repo, &snapshot.repo_name, &previous.issues, &issues)
                {
                    send_event(tx, event).await?;
                }
            }
            Ok(issues)
        }
        Err(error) => {
            telemetry::emit(source_record(
                telemetry::event_name::SOURCE_DEGRADED,
                "source_poll_failed",
                Some(&repo.path),
                Some(error.to_string()),
            ));
            eprintln!(
                "clawhip source GitHub issue polling failed for {}: {error}",
                repo.path
            );
            Ok(previous
                .map(|entry| entry.issues.clone())
                .unwrap_or_default())
        }
    }
}

async fn poll_pull_requests(
    config: &AppConfig,
    github_client: Option<&reqwest::Client>,
    repo: &GitRepoMonitor,
    snapshot: &GitSnapshot,
    previous: Option<&GitHubRepoState>,
    tx: &mpsc::Sender<IncomingEvent>,
) -> Result<HashMap<u64, PullRequestSnapshot>> {
    if !repo.emit_pr_status {
        return Ok(previous.map(|entry| entry.prs.clone()).unwrap_or_default());
    }

    let Some(client) = github_client else {
        return Ok(previous.map(|entry| entry.prs.clone()).unwrap_or_default());
    };

    match fetch_pull_requests(client, &config.monitors.github_api_base, repo, snapshot).await {
        Ok(prs) => {
            if let Some(previous) = previous {
                for (number, pr) in &prs {
                    match previous.prs.get(number) {
                        Some(old) if old.status == pr.status => {}
                        old => {
                            send_event(
                                tx,
                                IncomingEvent::github_pr_status_changed(
                                    snapshot.repo_name.clone(),
                                    *number,
                                    pr.title.clone(),
                                    old.map(|value| value.status.clone())
                                        .unwrap_or_else(|| "<new>".to_string()),
                                    pr.status.clone(),
                                    pr.url.clone(),
                                    repo.channel.clone(),
                                )
                                .with_mention(repo.mention.clone())
                                .with_format(repo.format.clone()),
                            )
                            .await?;
                        }
                    }
                }
            }
            Ok(prs)
        }
        Err(error) => {
            telemetry::emit(source_record(
                telemetry::event_name::SOURCE_DEGRADED,
                "source_poll_failed",
                Some(&repo.path),
                Some(error.to_string()),
            ));
            eprintln!(
                "clawhip source GitHub polling failed for {}: {error}",
                repo.path
            );
            Ok(previous.map(|entry| entry.prs.clone()).unwrap_or_default())
        }
    }
}

async fn poll_ci_statuses(
    config: &AppConfig,
    github_client: Option<&reqwest::Client>,
    repo: &GitRepoMonitor,
    snapshot: &GitSnapshot,
    previous: Option<&GitHubRepoState>,
    prs: &HashMap<u64, PullRequestSnapshot>,
    tx: &mpsc::Sender<IncomingEvent>,
) -> Result<HashMap<String, GitHubCISnapshot>> {
    if !repo.emit_pr_status {
        return Ok(previous.map(|entry| entry.ci.clone()).unwrap_or_default());
    }

    let Some(client) = github_client else {
        return Ok(previous.map(|entry| entry.ci.clone()).unwrap_or_default());
    };

    let open_prs = prs
        .iter()
        .filter(|(_, pr)| pr.status == "open")
        .map(|(number, pr)| (*number, pr))
        .collect::<Vec<_>>();

    match fetch_ci_statuses(
        client,
        &config.monitors.github_api_base,
        repo,
        snapshot,
        &open_prs,
    )
    .await
    {
        Ok(ci) => {
            if let Some(previous) = previous {
                for event in collect_ci_events(repo, &snapshot.repo_name, &previous.ci, &ci) {
                    send_event(tx, event).await?;
                }
            }
            Ok(ci)
        }
        Err(error) => {
            telemetry::emit(source_record(
                telemetry::event_name::SOURCE_DEGRADED,
                "source_poll_failed",
                Some(&repo.path),
                Some(error.to_string()),
            ));
            eprintln!(
                "clawhip source GitHub CI polling failed for {}: {error}",
                repo.path
            );
            Ok(previous.map(|entry| entry.ci.clone()).unwrap_or_default())
        }
    }
}

fn source_record(
    event_name: &str,
    reason_code: &str,
    repo_path: Option<&str>,
    error: Option<String>,
) -> serde_json::Map<String, serde_json::Value> {
    let correlation = format!("source:github:{}", repo_path.unwrap_or("inventory"));
    let mut record = telemetry::record(event_name, reason_code, correlation);
    record.insert("source".to_string(), serde_json::json!("github"));
    if let Some(repo_path) = repo_path {
        record.insert("repo_path".to_string(), serde_json::json!(repo_path));
    }
    if let Some(error) = error {
        record.insert("error".to_string(), serde_json::json!(error));
    }
    record
}

async fn send_event(tx: &mpsc::Sender<IncomingEvent>, event: IncomingEvent) -> Result<()> {
    tx.send(event)
        .await
        .map_err(|error| format!("github source channel closed: {error}").into())
}

// ── Reconciliation ───────────────────────────────────────────────────────

/// Run reconciliation for all repos: fetch open issues/PRs and compare with
/// tracked state. Emit events for items present on GitHub but absent from
/// state, marking the payload with `"source": "reconciliation"`.
async fn run_reconciliation(
    config: &AppConfig,
    client: &reqwest::Client,
    tx: &mpsc::Sender<IncomingEvent>,
    state: &HashMap<String, GitHubRepoState>,
) -> Result<()> {
    for repo in &config.monitors.git.repos {
        if !repo.emit_issue_opened && !repo.emit_pr_status {
            continue;
        }

        let snapshot = match snapshot_github_repo(repo).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                eprintln!(
                    "clawhip source github reconciliation: snapshot failed for {}: {error}",
                    repo.path
                );
                continue;
            }
        };

        let Some(repo_state) = state.get(&repo.path) else {
            continue;
        };

        if repo.emit_issue_opened {
            if let Err(error) = reconcile_issues(config, client, repo, &snapshot, repo_state, tx)
                .await
            {
                eprintln!(
                    "clawhip source github reconciliation: issue check failed for {}: {error}",
                    repo.path
                );
            }
        }

        if repo.emit_pr_status {
            if let Err(error) = reconcile_prs(config, client, repo, &snapshot, repo_state, tx).await
            {
                eprintln!(
                    "clawhip source github reconciliation: PR check failed for {}: {error}",
                    repo.path
                );
            }
        }
    }
    Ok(())
}

async fn reconcile_issues(
    config: &AppConfig,
    client: &reqwest::Client,
    repo: &GitRepoMonitor,
    snapshot: &GitSnapshot,
    repo_state: &GitHubRepoState,
    tx: &mpsc::Sender<IncomingEvent>,
) -> Result<()> {
    let github_repo = snapshot
        .github_repo
        .clone()
        .ok_or_else(|| format!("no GitHub repo for {}", repo.path))?;

    let response = github_get(
        client,
        &config.monitors.github_api_base,
        &format!("repos/{github_repo}/issues"),
        &[("state", "open"), ("per_page", "100")],
        &format!("reconciliation issues for {github_repo}"),
    )
    .await?;

    let issues: Vec<GitHubIssue> = response.json().await?;
    let mut emitted = 0u64;

    for issue in issues {
        if issue.is_pull_request() {
            continue;
        }
        if !repo_state.issues.contains_key(&issue.number) {
            let event = IncomingEvent::github_issue_opened(
                snapshot.repo_name.clone(),
                issue.number,
                issue.title,
                repo.channel.clone(),
            )
            .with_mention(repo.mention.clone())
            .with_format(repo.format.clone())
            .with_source("reconciliation");

            send_event(tx, event).await?;
            emitted += 1;
        }
    }

    if emitted > 0 {
        eprintln!(
            "clawhip source github reconciliation: emitted {} issue(s) for {}",
            emitted, snapshot.repo_name
        );
    }
    Ok(())
}

async fn reconcile_prs(
    config: &AppConfig,
    client: &reqwest::Client,
    repo: &GitRepoMonitor,
    snapshot: &GitSnapshot,
    repo_state: &GitHubRepoState,
    tx: &mpsc::Sender<IncomingEvent>,
) -> Result<()> {
    let github_repo = snapshot
        .github_repo
        .clone()
        .ok_or_else(|| format!("no GitHub repo for {}", repo.path))?;

    let response = github_get(
        client,
        &config.monitors.github_api_base,
        &format!("repos/{github_repo}/pulls"),
        &[("state", "open"), ("per_page", "100")],
        &format!("reconciliation PRs for {github_repo}"),
    )
    .await?;

    let pulls: Vec<GitHubPullRequest> = response.json().await?;
    let mut emitted = 0u64;

    for pull in pulls {
        if !repo_state.prs.contains_key(&pull.number) {
            let status = if pull.merged_at.is_some() {
                "merged"
            } else {
                pull.state.as_str()
            };

            let event = IncomingEvent::github_pr_status_changed(
                snapshot.repo_name.clone(),
                pull.number,
                pull.title,
                "<unknown>".to_string(),
                status.to_string(),
                pull.html_url,
                repo.channel.clone(),
            )
            .with_mention(repo.mention.clone())
            .with_format(repo.format.clone())
            .with_source("reconciliation");

            send_event(tx, event).await?;
            emitted += 1;
        }
    }

    if emitted > 0 {
        eprintln!(
            "clawhip source github reconciliation: emitted {} PR(s) for {}",
            emitted, snapshot.repo_name
        );
    }
    Ok(())
}

async fn github_get(
    client: &reqwest::Client,
    api_base: &str,
    path: &str,
    query: &[(&str, &str)],
    context: &str,
) -> Result<reqwest::Response> {
    let url = format!(
        "{}/{}",
        api_base.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    eprintln!("clawhip source github: GET {url} ({context})");

    let response = client.get(&url).query(query).send().await?;
    let status = response.status();

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        eprintln!("clawhip source github: GET {url} ({context}) failed with {status}: {body}");
        return Err(format!("GitHub API request failed with {status}: {body}").into());
    }

    eprintln!("clawhip source github: GET {url} ({context}) -> {status}");
    Ok(response)
}

fn collect_issue_events(
    repo: &GitRepoMonitor,
    repo_name: &str,
    previous: &HashMap<u64, IssueSnapshot>,
    current: &HashMap<u64, IssueSnapshot>,
) -> Vec<IncomingEvent> {
    let mut events = Vec::new();
    for (number, issue) in current {
        match previous.get(number) {
            None => events.push(
                IncomingEvent::github_issue_opened(
                    repo_name.to_string(),
                    *number,
                    issue.title.clone(),
                    repo.channel.clone(),
                )
                .with_mention(repo.mention.clone())
                .with_format(repo.format.clone()),
            ),
            Some(old) => {
                if old.state != issue.state && issue.state == "closed" {
                    events.push(
                        IncomingEvent::github_issue_closed(
                            repo_name.to_string(),
                            *number,
                            issue.title.clone(),
                            repo.channel.clone(),
                        )
                        .with_mention(repo.mention.clone())
                        .with_format(repo.format.clone()),
                    );
                }
                if issue.comments > old.comments {
                    events.push(
                        IncomingEvent::github_issue_commented(
                            repo_name.to_string(),
                            *number,
                            issue.title.clone(),
                            issue.comments,
                            repo.channel.clone(),
                        )
                        .with_mention(repo.mention.clone())
                        .with_format(repo.format.clone()),
                    );
                }
            }
        }
    }
    events
}

fn collect_ci_events(
    repo: &GitRepoMonitor,
    repo_name: &str,
    previous: &HashMap<String, GitHubCISnapshot>,
    current: &HashMap<String, GitHubCISnapshot>,
) -> Vec<IncomingEvent> {
    let mut events = Vec::new();
    for (key, ci) in current {
        let changed = previous
            .get(key)
            .map(|old| old.status != ci.status || old.conclusion != ci.conclusion)
            .unwrap_or(true);
        if !changed {
            continue;
        }

        let mut event = IncomingEvent::github_ci(
            ci.event_kind(),
            repo_name.to_string(),
            ci.pr_number,
            ci.workflow.clone(),
            ci.status.clone(),
            ci.conclusion.clone(),
            ci.sha.clone(),
            ci.url.clone(),
            ci.branch.clone(),
            repo.channel.clone(),
        )
        .with_mention(repo.mention.clone())
        .with_format(repo.format.clone());
        if let Some(payload) = event.payload.as_object_mut() {
            if let Some(run_id) = &ci.run_id {
                payload.insert("run_id".to_string(), json!(run_id));
            }
            payload.insert("run_job_count".to_string(), json!(ci.run_job_count));
            payload.insert("run_all_terminal".to_string(), json!(ci.run_all_terminal));
        }
        events.push(event);
    }

    events.sort_by(|left, right| {
        left.payload["workflow"]
            .as_str()
            .cmp(&right.payload["workflow"].as_str())
            .then_with(|| {
                left.payload["number"]
                    .as_u64()
                    .cmp(&right.payload["number"].as_u64())
            })
    });
    events
}

async fn fetch_issues(
    client: &reqwest::Client,
    api_base: &str,
    repo: &GitRepoMonitor,
    snapshot: &GitSnapshot,
) -> Result<HashMap<u64, IssueSnapshot>> {
    let github_repo = snapshot
        .github_repo
        .clone()
        .ok_or_else(|| format!("no GitHub repo configured or inferred for {}", repo.path))?;
    let response = github_get(
        client,
        api_base,
        &format!("repos/{github_repo}/issues"),
        &[("state", "all"), ("per_page", "100")],
        &format!("issues for {github_repo}"),
    )
    .await?;
    let issues: Vec<GitHubIssue> = response.json().await?;
    Ok(issues
        .into_iter()
        .filter(|issue| !issue.is_pull_request())
        .map(|issue| {
            (
                issue.number,
                IssueSnapshot {
                    title: issue.title,
                    state: issue.state,
                    comments: issue.comments,
                },
            )
        })
        .collect())
}

async fn fetch_pull_requests(
    client: &reqwest::Client,
    api_base: &str,
    repo: &GitRepoMonitor,
    snapshot: &GitSnapshot,
) -> Result<HashMap<u64, PullRequestSnapshot>> {
    let github_repo = snapshot
        .github_repo
        .clone()
        .ok_or_else(|| format!("no GitHub repo configured or inferred for {}", repo.path))?;
    let response = github_get(
        client,
        api_base,
        &format!("repos/{github_repo}/pulls"),
        &[("state", "all"), ("per_page", "100")],
        &format!("pull requests for {github_repo}"),
    )
    .await?;
    let pulls: Vec<GitHubPullRequest> = response.json().await?;
    Ok(pulls
        .into_iter()
        .map(|pull| {
            let status = if pull.merged_at.is_some() {
                "merged".to_string()
            } else {
                pull.state
            };
            (
                pull.number,
                PullRequestSnapshot {
                    title: pull.title,
                    status,
                    url: pull.html_url,
                    head_branch: pull.head.reference,
                    head_sha: pull.head.sha,
                },
            )
        })
        .collect())
}

async fn fetch_ci_statuses(
    client: &reqwest::Client,
    api_base: &str,
    repo: &GitRepoMonitor,
    snapshot: &GitSnapshot,
    open_prs: &[(u64, &PullRequestSnapshot)],
) -> Result<HashMap<String, GitHubCISnapshot>> {
    let github_repo = snapshot
        .github_repo
        .clone()
        .ok_or_else(|| format!("no GitHub repo configured or inferred for {}", repo.path))?;
    let mut check_runs = HashMap::new();
    let mut seen_run_ids = HashSet::new();

    for (number, pr) in open_prs {
        for check_run in fetch_check_runs(client, api_base, &github_repo, *number, pr).await? {
            if let Some(run_id) = &check_run.run_id {
                seen_run_ids.insert(run_id.clone());
            }
            check_runs.insert(check_run.dedupe_key(), check_run);
        }
    }

    for workflow_run in fetch_direct_workflow_runs(client, api_base, &github_repo, snapshot).await?
    {
        if workflow_run
            .run_id
            .as_ref()
            .is_some_and(|run_id| seen_run_ids.contains(run_id))
        {
            continue;
        }
        check_runs.insert(workflow_run.dedupe_key(), workflow_run);
    }

    Ok(check_runs)
}

async fn fetch_check_runs(
    client: &reqwest::Client,
    api_base: &str,
    github_repo: &str,
    pr_number: u64,
    pr: &PullRequestSnapshot,
) -> Result<Vec<GitHubCISnapshot>> {
    let response = github_get(
        client,
        api_base,
        &format!("repos/{github_repo}/commits/{}/check-runs", pr.head_sha),
        &[("per_page", "100")],
        &format!("check runs for {github_repo} PR #{pr_number}"),
    )
    .await?;

    let runs: GitHubCheckRunsResponse = response.json().await?;
    let run_summaries = summarize_workflow_runs(&runs.check_runs);
    Ok(runs
        .check_runs
        .into_iter()
        .map(|check_run| {
            let url = check_run.details_url.unwrap_or_else(|| pr.url.clone());
            let run_id = workflow_run_id(&url);
            let (run_job_count, run_all_terminal) = run_id
                .as_deref()
                .and_then(|id| run_summaries.get(id).copied())
                .unwrap_or((1, check_run.status == "completed"));
            GitHubCISnapshot {
                pr_number: Some(pr_number),
                workflow: check_run.name,
                status: check_run.status,
                conclusion: check_run.conclusion,
                sha: check_run.head_sha,
                url,
                branch: Some(pr.head_branch.clone()),
                run_id,
                run_job_count,
                run_all_terminal,
            }
        })
        .collect())
}

fn summarize_workflow_runs(check_runs: &[GitHubCheckRun]) -> HashMap<String, (usize, bool)> {
    let mut summaries = HashMap::new();
    for check_run in check_runs {
        let Some(run_id) = check_run.details_url.as_deref().and_then(workflow_run_id) else {
            continue;
        };
        let entry = summaries.entry(run_id).or_insert((0, true));
        entry.0 += 1;
        entry.1 &= check_run.status == "completed";
    }
    summaries
}

async fn fetch_direct_workflow_runs(
    client: &reqwest::Client,
    api_base: &str,
    github_repo: &str,
    snapshot: &GitSnapshot,
) -> Result<Vec<GitHubCISnapshot>> {
    let mut query = vec![("per_page", "100"), ("event", "push")];
    if !snapshot.branch.is_empty() {
        query.push(("branch", snapshot.branch.as_str()));
    }

    let response = github_get(
        client,
        api_base,
        &format!("repos/{github_repo}/actions/runs"),
        &query,
        &format!("workflow runs for {github_repo}"),
    )
    .await?;

    let runs: GitHubWorkflowRunsResponse = response.json().await?;
    Ok(runs
        .workflow_runs
        .into_iter()
        .filter(|run| run.pull_requests.is_empty())
        .map(|run| {
            let run_all_terminal = run.status == "completed";
            GitHubCISnapshot {
                pr_number: None,
                workflow: run
                    .name
                    .unwrap_or_else(|| format!("workflow-run-{}", run.id)),
                status: run.status,
                conclusion: run.conclusion,
                sha: run.head_sha,
                url: run.html_url,
                branch: non_empty_string(run.head_branch),
                run_id: Some(run.id.to_string()),
                run_job_count: 1,
                run_all_terminal,
            }
        })
        .collect())
}

fn workflow_run_id(url: &str) -> Option<String> {
    url.split("/actions/runs/")
        .nth(1)
        .and_then(|tail| tail.split('/').next())
        .filter(|part| !part.is_empty())
        .map(ToString::to_string)
}

fn build_github_client(token: Option<String>) -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("clawhip/0.1"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    if let Some(token) = token {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))?,
        );
    }
    Ok(reqwest::Client::builder()
        .default_headers(headers)
        .build()?)
}

#[derive(Deserialize)]
struct GitHubIssue {
    number: u64,
    title: String,
    state: String,
    comments: u64,
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

impl GitHubIssue {
    fn is_pull_request(&self) -> bool {
        self.pull_request.is_some()
    }
}

#[derive(Deserialize)]
struct GitHubPullRequest {
    number: u64,
    title: String,
    state: String,
    html_url: String,
    merged_at: Option<String>,
    head: GitHubPullRequestHead,
}

#[derive(Deserialize)]
struct GitHubPullRequestHead {
    #[serde(rename = "ref")]
    reference: String,
    sha: String,
}

#[derive(Deserialize)]
struct GitHubCheckRunsResponse {
    check_runs: Vec<GitHubCheckRun>,
}

#[derive(Deserialize)]
struct GitHubCheckRun {
    name: String,
    status: String,
    conclusion: Option<String>,
    details_url: Option<String>,
    head_sha: String,
}

#[derive(Deserialize)]
struct GitHubWorkflowRunsResponse {
    workflow_runs: Vec<GitHubWorkflowRun>,
}

#[derive(Deserialize)]
struct GitHubWorkflowRun {
    id: u64,
    #[serde(default)]
    name: Option<String>,
    status: String,
    conclusion: Option<String>,
    head_branch: String,
    head_sha: String,
    html_url: String,
    #[serde(default)]
    pull_requests: Vec<serde_json::Value>,
}

fn non_empty_string(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

fn classify_ci_event_kind(status: &str, conclusion: Option<&str>) -> &'static str {
    if status != "completed" {
        return "github.ci-started";
    }

    match conclusion {
        Some("success" | "neutral" | "skipped") => "github.ci-passed",
        Some("cancelled") => "github.ci-cancelled",
        _ => "github.ci-failed",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::config::{DefaultsConfig, RouteRule};
    use crate::events::MessageFormat;
    use crate::router::Router;
    use serde_json::json;

    #[tokio::test]
    async fn new_issue_events_apply_route_channel_and_mention_over_repo_monitor_channel() {
        let repo = GitRepoMonitor {
            path: "/tmp/clawhip".into(),
            name: Some("clawhip".into()),
            channel: Some("dev-channel".into()),
            ..GitRepoMonitor::default()
        };
        let previous = HashMap::new();
        let current = [(
            2_u64,
            IssueSnapshot {
                title: "live issue".into(),
                state: "open".into(),
                comments: 0,
            },
        )]
        .into_iter()
        .collect();
        let events = collect_issue_events(&repo, "clawhip", &previous, &current);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].canonical_kind(), "github.issue-opened");
        assert_eq!(events[0].payload["repo"], "clawhip");

        let config = AppConfig {
            defaults: DefaultsConfig {
                channel: Some("fallback".into()),
                channel_name: None,
                format: MessageFormat::Compact,
            },
            routes: vec![RouteRule {
                event: "github.*".into(),
                sink: "discord".into(),
                filter: [("repo".to_string(), "clawhip".to_string())]
                    .into_iter()
                    .collect(),
                channel: Some("route-channel".into()),
                channel_name: None,
                webhook: None,
                slack_webhook: None,
                mention: Some("<@1465264645320474637>".into()),
                allow_dynamic_tokens: false,
                format: Some(MessageFormat::Alert),
                template: None,
            }],
            ..AppConfig::default()
        };
        let router = Router::new(Arc::new(config));
        let (channel, _, content) = router.preview(&events[0]).await.unwrap();
        assert_eq!(channel, "route-channel");
        assert!(content.starts_with("<@1465264645320474637> "));
        assert!(content.contains("live issue"));
    }

    #[test]
    fn issue_comment_and_close_events_are_emitted() {
        let repo = GitRepoMonitor {
            path: "/tmp/clawhip".into(),
            name: Some("clawhip".into()),
            ..GitRepoMonitor::default()
        };
        let previous = [(
            2_u64,
            IssueSnapshot {
                title: "live issue".into(),
                state: "open".into(),
                comments: 0,
            },
        )]
        .into_iter()
        .collect();
        let current = [(
            2_u64,
            IssueSnapshot {
                title: "live issue".into(),
                state: "closed".into(),
                comments: 1,
            },
        )]
        .into_iter()
        .collect();
        let events = collect_issue_events(&repo, "clawhip", &previous, &current);
        assert!(
            events
                .iter()
                .any(|event| event.canonical_kind() == "github.issue-commented")
        );
        assert!(
            events
                .iter()
                .any(|event| event.canonical_kind() == "github.issue-closed")
        );
    }

    fn ci_snapshot(
        pr_number: u64,
        workflow: &str,
        status: &str,
        conclusion: Option<&str>,
    ) -> GitHubCISnapshot {
        GitHubCISnapshot {
            pr_number: Some(pr_number),
            workflow: workflow.into(),
            status: status.into(),
            conclusion: conclusion.map(ToString::to_string),
            sha: "abcdef1234567890".into(),
            url: "https://github.com/Yeachan-Heo/clawhip/actions/runs/1".into(),
            branch: Some("feat/github-ci-events".into()),
            run_id: Some("1".into()),
            run_job_count: 1,
            run_all_terminal: status == "completed",
        }
    }

    #[tokio::test]
    async fn direct_branch_workflow_run_without_open_pr_emits_ci_failed_event() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = json!({
                "workflow_runs": [{
                    "id": 24007460067_u64,
                    "name": "Rust CI",
                    "status": "completed",
                    "conclusion": "failure",
                    "head_branch": "main",
                    "head_sha": "deadbeef",
                    "html_url": "https://github.com/ultraworkers/claw-code/actions/runs/24007460067",
                    "pull_requests": []
                }]
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            req
        });

        let mut config = AppConfig::default();
        config.monitors.github_api_base = format!("http://{addr}");
        let repo = GitRepoMonitor {
            path: "/tmp/claw-code".into(),
            emit_pr_status: true,
            ..GitRepoMonitor::default()
        };
        let snapshot = GitSnapshot {
            repo_name: "claw-code".into(),
            repo_path: "/tmp/claw-code".into(),
            worktree_path: "/tmp/claw-code".into(),
            branch: "main".into(),
            head: "deadbeef".into(),
            commits: Vec::new(),
            github_repo: Some("ultraworkers/claw-code".into()),
        };
        let client = build_github_client(None).unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let prs = HashMap::new();

        let ci = poll_ci_statuses(&config, Some(&client), &repo, &snapshot, None, &prs, &tx)
            .await
            .unwrap();

        assert_eq!(ci.len(), 1);
        assert!(
            rx.try_recv().is_err(),
            "first poll after startup should prime CI baseline without emitting historical events"
        );

        let req = server.await.unwrap();
        assert!(req.contains("GET /repos/ultraworkers/claw-code/actions/runs?"));
        assert!(req.contains("branch=main"));
        assert!(req.contains("event=push"));
        assert!(req.contains("per_page=100"));
    }

    #[tokio::test]
    async fn direct_workflow_runs_skip_run_ids_already_seen_from_pr_checks() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            let responses = [
                json!({
                    "check_runs": [{
                        "name": "test",
                        "status": "completed",
                        "conclusion": "failure",
                        "details_url": "https://github.com/org/repo/actions/runs/123/jobs/1",
                        "head_sha": "prsha"
                    }]
                })
                .to_string(),
                json!({
                    "workflow_runs": [
                        {
                            "id": 123_u64,
                            "name": "CI",
                            "status": "completed",
                            "conclusion": "failure",
                            "head_branch": "feat/pr",
                            "head_sha": "prsha",
                            "html_url": "https://github.com/org/repo/actions/runs/123",
                            "pull_requests": [{"number": 42}]
                        },
                        {
                            "id": 456_u64,
                            "name": "Rust CI",
                            "status": "completed",
                            "conclusion": "failure",
                            "head_branch": "main",
                            "head_sha": "mainsha",
                            "html_url": "https://github.com/org/repo/actions/runs/456",
                            "pull_requests": []
                        }
                    ]
                })
                .to_string(),
            ];

            for body in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = vec![0_u8; 4096];
                let n = stream.read(&mut buf).await.unwrap();
                requests.push(String::from_utf8_lossy(&buf[..n]).to_string());
                // connection: close prevents reqwest from reusing the TCP
                // stream — the mock server calls accept() per request, so
                // keep-alive pooling causes the 2nd request to go to a dead
                // connection under load (flake root-cause, see #194).
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }

            requests
        });

        let snapshot = GitSnapshot {
            repo_name: "repo".into(),
            repo_path: "/tmp/repo".into(),
            worktree_path: "/tmp/repo".into(),
            branch: "main".into(),
            head: "mainsha".into(),
            commits: Vec::new(),
            github_repo: Some("org/repo".into()),
        };
        let client = build_github_client(None).unwrap();
        let pr = PullRequestSnapshot {
            title: "PR".into(),
            status: "open".into(),
            url: "https://github.com/org/repo/pull/42".into(),
            head_branch: "feat/pr".into(),
            head_sha: "prsha".into(),
        };
        let open_prs = vec![(42_u64, &pr)];

        let ci = fetch_ci_statuses(
            &client,
            &format!("http://{addr}"),
            &GitRepoMonitor::default(),
            &snapshot,
            &open_prs,
        )
        .await
        .unwrap();

        assert_eq!(ci.len(), 2);
        assert_eq!(
            ci.values()
                .filter(|snapshot| snapshot.run_id.as_deref() == Some("123"))
                .count(),
            1
        );
        let direct = ci
            .values()
            .find(|snapshot| snapshot.run_id.as_deref() == Some("456"))
            .unwrap();
        assert_eq!(direct.pr_number, None);
        assert_eq!(direct.branch.as_deref(), Some("main"));

        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("GET /repos/org/repo/commits/prsha/check-runs?"));
        assert!(requests[1].contains("GET /repos/org/repo/actions/runs?"));
        assert!(requests[1].contains("branch=main"));
        assert!(requests[1].contains("event=push"));
    }

    #[test]
    fn initial_ci_detection_emits_started_event_with_route_metadata() {
        let repo = GitRepoMonitor {
            path: "/tmp/clawhip".into(),
            name: Some("clawhip".into()),
            channel: Some("dev-channel".into()),
            mention: Some("<@123>".into()),
            format: Some(MessageFormat::Alert),
            ..GitRepoMonitor::default()
        };
        let previous = HashMap::new();
        let current_ci = ci_snapshot(58, "CI / test", "in_progress", None);
        let current = [(current_ci.dedupe_key(), current_ci)]
            .into_iter()
            .collect();

        let events = collect_ci_events(&repo, "clawhip", &previous, &current);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].canonical_kind(), "github.ci-started");
        assert_eq!(events[0].channel.as_deref(), Some("dev-channel"));
        assert_eq!(events[0].mention.as_deref(), Some("<@123>"));
        assert_eq!(events[0].format, Some(MessageFormat::Alert));
        assert_eq!(events[0].payload["repo"], json!("clawhip"));
        assert_eq!(events[0].payload["number"], json!(58));
        assert_eq!(events[0].payload["workflow"], json!("CI / test"));
        assert_eq!(events[0].payload["status"], json!("in_progress"));
        assert_eq!(events[0].payload["sha"], json!("abcdef1234567890"));
        assert_eq!(
            events[0].payload["url"],
            json!("https://github.com/Yeachan-Heo/clawhip/actions/runs/1")
        );
    }

    #[test]
    fn unchanged_ci_state_is_suppressed() {
        let repo = GitRepoMonitor {
            path: "/tmp/clawhip".into(),
            ..GitRepoMonitor::default()
        };
        let ci = ci_snapshot(58, "CI / test", "in_progress", None);
        let previous = [(ci.dedupe_key(), ci.clone())].into_iter().collect();
        let current = [(ci.dedupe_key(), ci)].into_iter().collect();

        let events = collect_ci_events(&repo, "clawhip", &previous, &current);
        assert!(events.is_empty());
    }

    #[test]
    fn ci_state_transition_to_failed_emits_failed_event() {
        let repo = GitRepoMonitor {
            path: "/tmp/clawhip".into(),
            ..GitRepoMonitor::default()
        };
        let previous_ci = ci_snapshot(58, "CI / test", "in_progress", None);
        let current_ci = ci_snapshot(58, "CI / test", "completed", Some("failure"));
        let previous = [(previous_ci.dedupe_key(), previous_ci)]
            .into_iter()
            .collect();
        let current = [(current_ci.dedupe_key(), current_ci)]
            .into_iter()
            .collect();

        let events = collect_ci_events(&repo, "clawhip", &previous, &current);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].canonical_kind(), "github.ci-failed");
        assert_eq!(events[0].payload["workflow"], json!("CI / test"));
        assert_eq!(events[0].payload["status"], json!("completed"));
        assert_eq!(events[0].payload["conclusion"], json!("failure"));
    }

    #[test]
    fn ci_state_transition_to_passed_emits_passed_event() {
        let repo = GitRepoMonitor {
            path: "/tmp/clawhip".into(),
            ..GitRepoMonitor::default()
        };
        let previous_ci = ci_snapshot(58, "CI / test", "in_progress", None);
        let current_ci = ci_snapshot(58, "CI / test", "completed", Some("success"));
        let previous = [(previous_ci.dedupe_key(), previous_ci)]
            .into_iter()
            .collect();
        let current = [(current_ci.dedupe_key(), current_ci)]
            .into_iter()
            .collect();

        let events = collect_ci_events(&repo, "clawhip", &previous, &current);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].canonical_kind(), "github.ci-passed");
    }

    #[test]
    fn ci_state_transition_to_cancelled_emits_cancelled_event() {
        let repo = GitRepoMonitor {
            path: "/tmp/clawhip".into(),
            ..GitRepoMonitor::default()
        };
        let previous_ci = ci_snapshot(58, "CI / test", "in_progress", None);
        let current_ci = ci_snapshot(58, "CI / test", "completed", Some("cancelled"));
        let previous = [(previous_ci.dedupe_key(), previous_ci)]
            .into_iter()
            .collect();
        let current = [(current_ci.dedupe_key(), current_ci)]
            .into_iter()
            .collect();

        let events = collect_ci_events(&repo, "clawhip", &previous, &current);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].canonical_kind(), "github.ci-cancelled");
    }

    #[tokio::test]
    async fn github_client_includes_bearer_auth_when_token_configured() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n[]")
                .await
                .unwrap();
            req
        });

        let client = build_github_client(Some("secret-token".into())).unwrap();
        let _ = client
            .get(format!("http://{}/repos/x/y/pulls", addr))
            .send()
            .await
            .unwrap();
        let req = server.await.unwrap();
        assert!(
            req.contains("Authorization: Bearer secret-token")
                || req.contains("authorization: Bearer secret-token")
        );
    }

    #[tokio::test]
    async fn snapshot_falls_back_to_configured_github_repo_without_local_clone() {
        let repo = GitRepoMonitor {
            path: "/tmp/clawhip-test-private-repo-missing".into(),
            name: Some("private-repo".into()),
            github_repo: Some("owner/private-repo".into()),
            ..GitRepoMonitor::default()
        };

        let snapshot = snapshot_github_repo(&repo).await.unwrap();

        assert_eq!(snapshot.repo_name, "private-repo");
        assert_eq!(snapshot.github_repo.as_deref(), Some("owner/private-repo"));
    }

    #[tokio::test]
    async fn source_loop_survives_transient_github_api_errors() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let request_count = Arc::new(AtomicUsize::new(0));
        let request_count_for_server = request_count.clone();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            let responses = [
                "HTTP/1.1 500 Internal Server Error\r\ncontent-type: text/plain\r\ncontent-length: 4\r\n\r\nboom",
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n[]",
            ];

            for response in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = vec![0_u8; 4096];
                let n = stream.read(&mut buf).await.unwrap();
                requests.push(String::from_utf8_lossy(&buf[..n]).to_string());
                request_count_for_server.fetch_add(1, Ordering::SeqCst);
                stream.write_all(response.as_bytes()).await.unwrap();
            }

            requests
        });

        let mut config = AppConfig::default();
        config.monitors.poll_interval_secs = 1;
        config.monitors.github_api_base = format!("http://{addr}");
        config.monitors.git.repos = vec![GitRepoMonitor {
            path: "/tmp/clawhip-test-private-repo-missing".into(),
            name: Some("private-repo".into()),
            github_repo: Some("owner/private-repo".into()),
            emit_commits: false,
            emit_branch_changes: false,
            emit_issue_opened: true,
            emit_pr_status: false,
            ..GitRepoMonitor::default()
        }];

        let source = GitHubSource::new(Arc::new(config));
        let (tx, _rx) = mpsc::channel(4);
        let source_task = tokio::spawn(async move { source.run(tx).await });

        tokio::time::timeout(Duration::from_secs(5), async {
            while request_count.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        assert!(
            !source_task.is_finished(),
            "GitHub source loop exited after a transient API failure"
        );

        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| {
            request.contains("GET /repos/owner/private-repo/issues?")
                || request.contains("GET /repos/owner/private-repo/issues ")
        }));

        source_task.abort();
        let _ = source_task.await;
    }

    // ── State persistence tests ──────────────────────────────────────────

    #[tokio::test]
    async fn state_persistence_save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("github-state.json");

        let mut state: HashMap<String, GitHubRepoState> = HashMap::new();
        let mut issues = HashMap::new();
        issues.insert(
            42_u64,
            IssueSnapshot {
                title: "critical bug".into(),
                state: "open".into(),
                comments: 3,
            },
        );
        let mut prs = HashMap::new();
        prs.insert(
            7_u64,
            PullRequestSnapshot {
                title: "fix it".into(),
                status: "open".into(),
                url: "https://github.com/owner/repo/pull/7".into(),
                head_branch: "fix".into(),
                head_sha: "abc123".into(),
            },
        );
        state.insert(
            "/tmp/repo".to_string(),
            GitHubRepoState {
                issues,
                prs,
                ci: HashMap::new(),
            },
        );

        save_state(&Some(path.clone()), &state).await.unwrap();

        assert!(path.exists(), "state file should exist after save");

        let loaded = load_state(&Some(path.clone())).await;
        assert_eq!(loaded.len(), 1);

        let repo_state = loaded.get("/tmp/repo").unwrap();
        assert_eq!(repo_state.issues.len(), 1);
        assert_eq!(repo_state.issues.get(&42).unwrap().title, "critical bug");
        assert_eq!(repo_state.issues.get(&42).unwrap().state, "open");
        assert_eq!(repo_state.issues.get(&42).unwrap().comments, 3);
        assert_eq!(repo_state.prs.len(), 1);
        assert_eq!(repo_state.prs.get(&7).unwrap().title, "fix it");
        assert_eq!(repo_state.prs.get(&7).unwrap().status, "open");
        assert!(repo_state.ci.is_empty());
    }

    #[tokio::test]
    async fn state_persistence_none_path_returns_empty() {
        let state = load_state(&None).await;
        assert!(state.is_empty());
    }

    #[tokio::test]
    async fn state_persistence_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let state = load_state(&Some(path)).await;
        assert!(state.is_empty());
    }

    // ── Reconciliation tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn reconciliation_emits_events_for_untracked_open_issues() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = json!([
                {
                    "number": 100,
                    "title": "untracked issue",
                    "state": "open",
                    "comments": 0
                },
                {
                    "number": 200,
                    "title": "another untracked",
                    "state": "open",
                    "comments": 1,
                    "pull_request": {}
                }
            ])
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            req
        });

        let mut config = AppConfig::default();
        config.monitors.github_api_base = format!("http://{addr}");
        config.monitors.git.repos = vec![GitRepoMonitor {
            path: "/tmp/clawhip".into(),
            name: Some("clawhip".into()),
            github_repo: Some("owner/clawhip".into()),
            emit_issue_opened: true,
            emit_pr_status: false,
            ..GitRepoMonitor::default()
        }];

        let client = build_github_client(None).unwrap();
        let (tx, mut rx) = mpsc::channel(4);

        let mut state: HashMap<String, GitHubRepoState> = HashMap::new();
        state.insert(
            "/tmp/clawhip".to_string(),
            GitHubRepoState {
                issues: HashMap::new(),
                prs: HashMap::new(),
                ci: HashMap::new(),
            },
        );

        run_reconciliation(&config, &client, &tx, &state)
            .await
            .unwrap();

        let event = rx.recv().await.unwrap();
        assert_eq!(event.canonical_kind(), "github.issue-opened");
        assert_eq!(event.payload["number"], json!(100));
        assert_eq!(event.payload["title"], json!("untracked issue"));
        assert_eq!(event.payload["source"], json!("reconciliation"));

        // PR #200 should be filtered out (is_pull_request)
        assert!(rx.try_recv().is_err(), "PR should be filtered out");

        let _req = server.await.unwrap();
    }

    #[tokio::test]
    async fn reconciliation_skips_already_tracked_issues() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = json!([
                {
                    "number": 100,
                    "title": "tracked issue",
                    "state": "open",
                    "comments": 2
                }
            ])
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            req
        });

        let mut config = AppConfig::default();
        config.monitors.github_api_base = format!("http://{addr}");
        config.monitors.git.repos = vec![GitRepoMonitor {
            path: "/tmp/clawhip".into(),
            name: Some("clawhip".into()),
            github_repo: Some("owner/clawhip".into()),
            emit_issue_opened: true,
            emit_pr_status: false,
            ..GitRepoMonitor::default()
        }];

        let client = build_github_client(None).unwrap();
        let (tx, mut rx) = mpsc::channel(4);

        let mut issues = HashMap::new();
        issues.insert(
            100_u64,
            IssueSnapshot {
                title: "tracked issue".into(),
                state: "open".into(),
                comments: 2,
            },
        );
        let mut state: HashMap<String, GitHubRepoState> = HashMap::new();
        state.insert(
            "/tmp/clawhip".to_string(),
            GitHubRepoState {
                issues,
                prs: HashMap::new(),
                ci: HashMap::new(),
            },
        );

        run_reconciliation(&config, &client, &tx, &state)
            .await
            .unwrap();

        assert!(
            rx.try_recv().is_err(),
            "tracked issue should not emit reconciliation event"
        );

        let _req = server.await.unwrap();
    }

    #[tokio::test]
    async fn reconciliation_emits_events_for_untracked_open_prs() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = json!([
                {
                    "number": 42,
                    "title": "untracked PR",
                    "state": "open",
                    "html_url": "https://github.com/owner/repo/pull/42",
                    "merged_at": null,
                    "head": {
                        "ref": "feat",
                        "sha": "abc123"
                    }
                }
            ])
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            req
        });

        let mut config = AppConfig::default();
        config.monitors.github_api_base = format!("http://{addr}");
        config.monitors.git.repos = vec![GitRepoMonitor {
            path: "/tmp/clawhip".into(),
            name: Some("clawhip".into()),
            github_repo: Some("owner/clawhip".into()),
            emit_issue_opened: false,
            emit_pr_status: true,
            ..GitRepoMonitor::default()
        }];

        let client = build_github_client(None).unwrap();
        let (tx, mut rx) = mpsc::channel(4);

        let mut state: HashMap<String, GitHubRepoState> = HashMap::new();
        state.insert(
            "/tmp/clawhip".to_string(),
            GitHubRepoState {
                issues: HashMap::new(),
                prs: HashMap::new(),
                ci: HashMap::new(),
            },
        );

        run_reconciliation(&config, &client, &tx, &state)
            .await
            .unwrap();

        let event = rx.recv().await.unwrap();
        assert_eq!(event.canonical_kind(), "github.pr-status-changed");
        assert_eq!(event.payload["number"], json!(42));
        assert_eq!(event.payload["title"], json!("untracked PR"));
        assert_eq!(event.payload["source"], json!("reconciliation"));

        let _req = server.await.unwrap();
    }

    // ── Sink routing test ────────────────────────────────────────────────

    #[tokio::test]
    async fn github_events_route_through_discord_sink_correctly() {
        let repo = GitRepoMonitor {
            path: "/tmp/clawhip".into(),
            name: Some("clawhip".into()),
            channel: Some("dev-channel".into()),
            format: Some(MessageFormat::Alert),
            ..GitRepoMonitor::default()
        };
        let previous = HashMap::new();
        let current = [(
            2_u64,
            IssueSnapshot {
                title: "test issue".into(),
                state: "open".into(),
                comments: 0,
            },
        )]
        .into_iter()
        .collect();
        let events = collect_issue_events(&repo, "clawhip", &previous, &current);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].canonical_kind(), "github.issue-opened");

        let config = AppConfig {
            defaults: DefaultsConfig {
                channel: Some("fallback".into()),
                channel_name: None,
                format: MessageFormat::Compact,
            },
            routes: vec![RouteRule {
                event: "github.*".into(),
                sink: "discord".into(),
                filter: [("repo".to_string(), "clawhip".to_string())]
                    .into_iter()
                    .collect(),
                channel: Some("route-channel".into()),
                channel_name: None,
                webhook: None,
                slack_webhook: None,
                mention: Some("<@test>".into()),
                allow_dynamic_tokens: false,
                format: Some(MessageFormat::Alert),
                template: None,
            }],
            ..AppConfig::default()
        };
        let router = Router::new(Arc::new(config));
        let (channel, _, content) = router.preview(&events[0]).await.unwrap();
        assert_eq!(channel, "route-channel");
        assert!(content.starts_with("<@test> "));
        assert!(content.contains("test issue"));
    }

    #[tokio::test]
    async fn reconciliation_marked_events_route_through_discord_sink() {
        let event = IncomingEvent::github_issue_opened(
            "clawhip".into(),
            99,
            "reconciled issue".into(),
            None,
        )
        .with_mention(Some("<@bot>".into()))
        .with_format(Some(MessageFormat::Alert))
        .with_source("reconciliation");

        assert_eq!(event.payload["source"], json!("reconciliation"));

        let config = AppConfig {
            defaults: DefaultsConfig {
                channel: Some("default".into()),
                channel_name: None,
                format: MessageFormat::Compact,
            },
            routes: vec![RouteRule {
                event: "github.*".into(),
                sink: "discord".into(),
                filter: [("repo".to_string(), "clawhip".to_string())]
                    .into_iter()
                    .collect(),
                channel: Some("recon-route".into()),
                channel_name: None,
                webhook: None,
                slack_webhook: None,
                mention: Some("<@bot>".into()),
                allow_dynamic_tokens: false,
                format: Some(MessageFormat::Alert),
                template: None,
            }],
            ..AppConfig::default()
        };
        let router = Router::new(Arc::new(config));
        let (channel, format, content) = router.preview(&event).await.unwrap();
        assert_eq!(channel, "recon-route");
        assert_eq!(format, MessageFormat::Alert);
        assert!(content.starts_with("<@bot> "));
        assert!(content.contains("reconciled issue"));
    }

    // ── Backfill tests ───────────────────────────────────────────────────

    #[tokio::test]
    async fn backfill_emits_only_open_issues_with_source_marker() {
        let repo = GitRepoMonitor {
            path: "/tmp/clawhip".into(),
            name: Some("clawhip".into()),
            channel: Some("dev-channel".into()),
            ..GitRepoMonitor::default()
        };
        let snapshot = GitSnapshot {
            repo_name: "clawhip".into(),
            repo_path: "/tmp/clawhip".into(),
            worktree_path: "/tmp/clawhip".into(),
            branch: "main".into(),
            head: "abc".into(),
            commits: Vec::new(),
            github_repo: Some("owner/clawhip".into()),
        };
        let mut issues = HashMap::new();
        // Open issue — should emit
        issues.insert(
            1_u64,
            IssueSnapshot {
                title: "open issue".into(),
                state: "open".into(),
                comments: 0,
            },
        );
        // Closed issue — should NOT emit
        issues.insert(
            2_u64,
            IssueSnapshot {
                title: "closed issue".into(),
                state: "closed".into(),
                comments: 5,
            },
        );

        let (tx, mut rx) = mpsc::channel(4);
        backfill_issues(&repo, &snapshot, &issues, &tx)
            .await
            .unwrap();

        let event = rx.recv().await.unwrap();
        assert_eq!(event.canonical_kind(), "github.issue-opened");
        assert_eq!(event.payload["number"], json!(1));
        assert_eq!(event.payload["title"], json!("open issue"));
        assert_eq!(event.payload["source"], json!("backfill"));

        assert!(
            rx.try_recv().is_err(),
            "closed issue should not be backfilled"
        );
    }
}
