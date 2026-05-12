use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
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

        let state_path = state_file_path(&self.config);
        let file_state = load_monitor_state(&state_path);
        let known_repos: HashSet<String> = file_state.keys().cloned().collect();
        let mut state = file_state;

        loop {
            run_github_poll_cycle(
                self.config.as_ref(),
                github_client.as_ref(),
                &tx,
                &mut state,
                &known_repos,
                &state_path,
            )
            .await;
            sleep(Duration::from_secs(
                self.config.monitors.poll_interval_secs.max(1),
            ))
            .await;
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

// ── State persistence ────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct GitHubMonitorStateFile {
    repos: HashMap<String, GitHubRepoState>,
    updated_at: String,
}

/// Returns the path for the GitHub monitor state file.
fn state_file_path(config: &AppConfig) -> PathBuf {
    config
        .monitors
        .github_monitor_state_path
        .clone()
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".clawhip").join("github-monitor-state.json")
        })
}

/// Loads persisted GitHub monitor state from disk.
/// Returns an empty map if the file is missing or corrupt.
fn load_monitor_state(path: &Path) -> HashMap<String, GitHubRepoState> {
    match fs::read_to_string(path) {
        Ok(content) => match serde_json::from_str::<GitHubMonitorStateFile>(&content) {
            Ok(file) => {
                eprintln!(
                    "clawhip source github: loaded state for {} repo(s) from {}",
                    file.repos.len(),
                    path.display()
                );
                file.repos
            }
            Err(_) => {
                // Corrupt file — ignore, fall through to legacy suppress behaviour.
                eprintln!(
                    "clawhip source github: corrupt state file at {}, ignoring",
                    path.display()
                );
                HashMap::new()
            }
        },
        Err(_) => {
            // No state file yet — first run, legacy behaviour.
            HashMap::new()
        }
    }
}

/// Persists the current in-memory state to disk.
fn save_monitor_state(path: &Path, repos: &HashMap<String, GitHubRepoState>) {
    let updated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| String::new());

    let file = GitHubMonitorStateFile {
        repos: repos.clone(),
        updated_at,
    };

    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("clawhip source github: failed to create state dir: {e}");
            return;
        }
    }
    match serde_json::to_string_pretty(&file) {
        Ok(json) => {
            if let Err(e) = fs::write(path, &json) {
                eprintln!("clawhip source github: failed to write state file: {e}");
            }
        }
        Err(e) => {
            eprintln!("clawhip source github: failed to serialize state: {e}");
        }
    }
}

// ── Poll cycle ────────────────────────────────────────────────────

async fn run_github_poll_cycle(
    config: &AppConfig,
    github_client: Option<&reqwest::Client>,
    tx: &mpsc::Sender<IncomingEvent>,
    state: &mut HashMap<String, GitHubRepoState>,
    known_repos: &HashSet<String>,
    state_path: &Path,
) {
    if let Err(error) = poll_github(config, github_client, tx, state, known_repos).await {
        telemetry::emit(source_record(
            telemetry::event_name::SOURCE_DEGRADED,
            "source_poll_failed",
            None,
            Some(error.to_string()),
        ));
        eprintln!("clawhip source github poll failed: {error}");
    }
    save_monitor_state(state_path, state);
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
    known_repos: &HashSet<String>,
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
        let is_new_repo = previous.is_none() && !known_repos.contains(&repo.path);

        let issues = match poll_issues(config, github_client, repo, &snapshot, previous, is_new_repo, tx).await {
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
        let prs =
            match poll_pull_requests(config, github_client, repo, &snapshot, previous, is_new_repo, tx).await {
                Ok(prs) => prs,
                Err(error) => {
                    eprintln!(
                        "clawhip source GitHub pull request processing failed for {}: {error}",
                        repo.path
                    );
                    previous.map(|entry| entry.prs.clone()).unwrap_or_default()
                }
            };
        let ci = match poll_ci_statuses(config, github_client, repo, &snapshot, previous, is_new_repo, &prs, tx)
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

async fn poll_issues(
    config: &AppConfig,
    github_client: Option<&reqwest::Client>,
    repo: &GitRepoMonitor,
    snapshot: &GitSnapshot,
    previous: Option<&GitHubRepoState>,
    is_new_repo: bool,
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
            } else if is_new_repo {
                // Backfill: emit all open issues for a newly registered repo.
                for (number, issue) in &issues {
                    if issue.state == "open" {
                        send_event(
                            tx,
                            IncomingEvent::github_issue_opened(
                                snapshot.repo_name.clone(),
                                *number,
                                issue.title.clone(),
                                repo.channel.clone(),
                            )
                            .with_mention(repo.mention.clone())
                            .with_format(repo.format.clone()),
                        )
                        .await?;
                    }
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
    is_new_repo: bool,
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
            } else if is_new_repo {
                // Backfill: emit all PRs for a newly registered repo.
                for (number, pr) in &prs {
                    send_event(
                        tx,
                        IncomingEvent::github_pr_status_changed(
                            snapshot.repo_name.clone(),
                            *number,
                            pr.title.clone(),
                            "<new>".to_string(),
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
    is_new_repo: bool,
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
            } else if is_new_repo {
                // Backfill: emit all current CI states for a newly registered repo.
                for event in collect_ci_events(repo, &snapshot.repo_name, &HashMap::new(), &ci) {
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

        let ci = poll_ci_statuses(&config, Some(&client), &repo, &snapshot, None, false, &prs, &tx)
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

    // ── State persistence tests ─────────────────────────────────────

    #[test]
    fn state_save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let mut repos = HashMap::new();
        let mut issues = HashMap::new();
        issues.insert(
            1_u64,
            IssueSnapshot {
                title: "bug: fix".into(),
                state: "open".into(),
                comments: 3,
            },
        );
        issues.insert(
            2_u64,
            IssueSnapshot {
                title: "feat: add".into(),
                state: "closed".into(),
                comments: 5,
            },
        );
        let mut prs = HashMap::new();
        prs.insert(
            42_u64,
            PullRequestSnapshot {
                title: "PR title".into(),
                status: "open".into(),
                url: "https://github.com/org/repo/pull/42".into(),
                head_branch: "main".into(),
                head_sha: "abc123".into(),
            },
        );
        let mut ci = HashMap::new();
        let ci_snap = GitHubCISnapshot {
            pr_number: Some(42),
            workflow: "CI".into(),
            status: "completed".into(),
            conclusion: Some("success".into()),
            sha: "abc123".into(),
            url: "https://github.com/org/repo/actions/runs/1".into(),
            branch: Some("main".into()),
            run_id: Some("1".into()),
            run_job_count: 3,
            run_all_terminal: true,
        };
        let ci_key = ci_snap.dedupe_key();
        ci.insert(ci_key.clone(), ci_snap);

        repos.insert(
            "/tmp/repo-a".to_string(),
            GitHubRepoState {
                issues: issues.clone(),
                prs: prs.clone(),
                ci: ci.clone(),
            },
        );

        // Save
        save_monitor_state(&path, &repos);
        assert!(path.exists(), "state file should exist after save");

        // Load
        let loaded = load_monitor_state(&path);
        assert_eq!(loaded.len(), 1);
        let loaded_repo = loaded.get("/tmp/repo-a").unwrap();
        assert_eq!(loaded_repo.issues.len(), 2);
        assert_eq!(loaded_repo.issues.get(&1).unwrap().title, "bug: fix");
        assert_eq!(loaded_repo.issues.get(&1).unwrap().state, "open");
        assert_eq!(loaded_repo.issues.get(&1).unwrap().comments, 3);
        assert_eq!(loaded_repo.prs.len(), 1);
        assert_eq!(loaded_repo.prs.get(&42).unwrap().title, "PR title");
        assert_eq!(loaded_repo.ci.len(), 1);
        assert_eq!(
            loaded_repo.ci.get(&ci_key).unwrap().conclusion.as_deref(),
            Some("success")
        );
    }

    #[test]
    fn state_file_missing_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let state = load_monitor_state(&path);
        assert!(state.is_empty());
    }

    #[test]
    fn state_file_corrupt_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.json");
        fs::write(&path, "not valid json at all").unwrap();
        let state = load_monitor_state(&path);
        assert!(state.is_empty());
    }

    #[tokio::test]
    async fn backfill_issues_emit_events_for_new_repo() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let _req = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = json!([
                {"number": 1, "title": "open issue", "state": "open", "comments": 0},
                {"number": 2, "title": "closed issue", "state": "closed", "comments": 1}
            ])
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let mut config = AppConfig::default();
        config.monitors.github_api_base = format!("http://{addr}");
        let client = build_github_client(None).unwrap();
        let repo = GitRepoMonitor {
            path: "/tmp/new-repo".into(),
            name: Some("new-repo".into()),
            github_repo: Some("owner/new-repo".into()),
            emit_issue_opened: true,
            emit_pr_status: false,
            ..GitRepoMonitor::default()
        };
        let snapshot = GitSnapshot {
            repo_name: "new-repo".into(),
            repo_path: "/tmp/new-repo".into(),
            worktree_path: "/tmp/new-repo".into(),
            branch: "main".into(),
            head: "head".into(),
            commits: Vec::new(),
            github_repo: Some("owner/new-repo".into()),
        };
        let (tx, mut rx) = mpsc::channel(4);

        let issues = poll_issues(
            &config,
            Some(&client),
            &repo,
            &snapshot,
            None,
            true, // is_new_repo = true → backfill
            &tx,
        )
        .await
        .unwrap();

        // Both issues should be returned
        assert_eq!(issues.len(), 2);
        // Only the open issue should have been emitted
        let emitted = rx.try_recv().unwrap();
        assert_eq!(emitted.canonical_kind(), "github.issue-opened");
        assert_eq!(emitted.payload["number"], json!(1));
        assert_eq!(emitted.payload["title"], "open issue");
        // Second emission should be pending (closed issue is skipped in backfill)
        assert!(rx.try_recv().is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn backfill_prs_emit_events_for_new_repo() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let _req = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = json!([
                {"number": 10, "title": "open PR", "state": "open", "html_url": "https://github.com/owner/new-repo/pull/10", "merged_at": null, "head": {"ref": "feat", "sha": "abc"}},
                {"number": 11, "title": "merged PR", "state": "closed", "html_url": "https://github.com/owner/new-repo/pull/11", "merged_at": "2026-01-01T00:00:00Z", "head": {"ref": "main", "sha": "def"}}
            ])
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let mut config = AppConfig::default();
        config.monitors.github_api_base = format!("http://{addr}");
        let client = build_github_client(None).unwrap();
        let repo = GitRepoMonitor {
            path: "/tmp/new-repo".into(),
            name: Some("new-repo".into()),
            github_repo: Some("owner/new-repo".into()),
            emit_issue_opened: false,
            emit_pr_status: true,
            ..GitRepoMonitor::default()
        };
        let snapshot = GitSnapshot {
            repo_name: "new-repo".into(),
            repo_path: "/tmp/new-repo".into(),
            worktree_path: "/tmp/new-repo".into(),
            branch: "main".into(),
            head: "head".into(),
            commits: Vec::new(),
            github_repo: Some("owner/new-repo".into()),
        };
        let (tx, mut rx) = mpsc::channel(4);

        let prs = poll_pull_requests(
            &config,
            Some(&client),
            &repo,
            &snapshot,
            None,
            true, // is_new_repo = true → backfill
            &tx,
        )
        .await
        .unwrap();

        assert_eq!(prs.len(), 2);
        // Both PRs should be emitted during backfill
        let first = rx.try_recv().unwrap();
        assert_eq!(first.canonical_kind(), "github.pr-status-changed");
        assert_eq!(first.payload["number"], json!(10));
        assert_eq!(first.payload["old_status"], "<new>");
        let second = rx.try_recv().unwrap();
        assert_eq!(second.canonical_kind(), "github.pr-status-changed");
        assert_eq!(second.payload["number"], json!(11));
        assert_eq!(second.payload["old_status"], "<new>");
        assert!(rx.try_recv().is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn backfill_suppressed_for_known_repo_on_first_poll() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let _req = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = json!([
                {"number": 5, "title": "existing issue", "state": "open", "comments": 0}
            ])
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let mut config = AppConfig::default();
        config.monitors.github_api_base = format!("http://{addr}");
        let client = build_github_client(None).unwrap();
        let repo = GitRepoMonitor {
            path: "/tmp/known-repo".into(),
            name: Some("known-repo".into()),
            github_repo: Some("owner/known-repo".into()),
            emit_issue_opened: true,
            emit_pr_status: false,
            ..GitRepoMonitor::default()
        };
        let snapshot = GitSnapshot {
            repo_name: "known-repo".into(),
            repo_path: "/tmp/known-repo".into(),
            worktree_path: "/tmp/known-repo".into(),
            branch: "main".into(),
            head: "head".into(),
            commits: Vec::new(),
            github_repo: Some("owner/known-repo".into()),
        };
        let (tx, mut rx) = mpsc::channel(4);

        let issues = poll_issues(
            &config,
            Some(&client),
            &repo,
            &snapshot,
            None,
            false, // is_new_repo = false → known repo, suppress (legacy)
            &tx,
        )
        .await
        .unwrap();

        assert_eq!(issues.len(), 1);
        // No events should be emitted for a known repo (legacy first-poll suppress)
        assert!(rx.try_recv().is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn known_repo_restores_state_and_does_not_backfill() {
        // Simulate restart: state exists in file, repo is "known"
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let _req = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = json!([
                {"number": 1, "title": "existing issue", "state": "open", "comments": 0}
            ])
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let mut config = AppConfig::default();
        config.monitors.github_api_base = format!("http://{addr}");
        let client = build_github_client(None).unwrap();
        let repo = GitRepoMonitor {
            path: "/tmp/persisted-repo".into(),
            name: Some("persisted-repo".into()),
            github_repo: Some("owner/persisted-repo".into()),
            emit_issue_opened: true,
            emit_pr_status: false,
            ..GitRepoMonitor::default()
        };
        let snapshot = GitSnapshot {
            repo_name: "persisted-repo".into(),
            repo_path: "/tmp/persisted-repo".into(),
            worktree_path: "/tmp/persisted-repo".into(),
            branch: "main".into(),
            head: "head".into(),
            commits: Vec::new(),
            github_repo: Some("owner/persisted-repo".into()),
        };

        // Set up state with the repo already tracked (simulating restart)
        let mut state = HashMap::new();
        let mut issues = HashMap::new();
        issues.insert(
            1_u64,
            IssueSnapshot {
                title: "existing issue".into(),
                state: "open".into(),
                comments: 0,
            },
        );
        state.insert(
            "/tmp/persisted-repo".to_string(),
            GitHubRepoState {
                issues,
                prs: HashMap::new(),
                ci: HashMap::new(),
            },
        );

        let known_repos: HashSet<String> = state.keys().cloned().collect();
        let (tx, mut rx) = mpsc::channel(4);

        let issues = poll_issues(
            &config,
            Some(&client),
            &repo,
            &snapshot,
            state.get("/tmp/persisted-repo"), // previous restored from file state
            false,
            &tx,
        )
        .await
        .unwrap();

        assert_eq!(issues.len(), 1);
        // No events emitted: state was restored, nothing changed → no diff
        assert!(rx.try_recv().is_err());
        assert!(known_repos.contains("/tmp/persisted-repo"));
        server.await.unwrap();
    }
}
