use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::sleep;

use crate::Result;
use crate::config::{AppConfig, GitRepoMonitor};
use crate::events::IncomingEvent;
use crate::source::Source;
use crate::source::git::{GitSnapshot, repo_display_name, snapshot_git_repo};

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
        let mut state = HashMap::new();

        loop {
            run_github_poll_cycle(
                self.config.as_ref(),
                github_client.as_ref(),
                &tx,
                &mut state,
            )
            .await;
            sleep(Duration::from_secs(
                self.config.monitors.poll_interval_secs.max(1),
            ))
            .await;
        }
    }
}

struct GitHubRepoState {
    issues: HashMap<u64, IssueSnapshot>,
    issues_ready: bool,
    prs: HashMap<u64, PullRequestSnapshot>,
    prs_ready: bool,
    pr_reviews: HashMap<u64, HashMap<u64, ReviewSnapshot>>,
    pr_reviews_ready: bool,
    ci: HashMap<String, GitHubCISnapshot>,
    ci_ready: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReviewSnapshot {
    state: String,
    body: String,
    actor: Option<String>,
}

#[derive(Clone)]
struct IssueSnapshot {
    title: String,
    state: String,
    comments: u64,
    html_url: String,
    labels: Vec<String>,
    body: String,
}

#[derive(Clone)]
struct PullRequestSnapshot {
    title: String,
    status: String,
    url: String,
    head_branch: String,
    head_sha: String,
    body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
) {
    if let Err(error) = poll_github(config, github_client, tx, state).await {
        eprintln!("clawhip source github poll failed: {error}");
    }
}

async fn snapshot_github_repo(repo: &GitRepoMonitor) -> Result<GitSnapshot> {
    match snapshot_git_repo(repo).await {
        Ok(snapshot) => Ok(snapshot),
        Err(error) => match repo.github_repo.clone() {
            Some(github_repo) => {
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
) -> Result<()> {
    for repo in &config.monitors.git.repos {
        if !repo.emit_issue_opened && !repo.emit_pr_status && !repo.emit_pr_reviews {
            continue;
        }

        let snapshot = match snapshot_github_repo(repo).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                eprintln!(
                    "clawhip source github snapshot failed for {}: {error}",
                    repo.path
                );
                continue;
            }
        };

        let previous = state.get(&repo.path);
        let (issues, issues_ready) =
            match poll_issues(config, github_client, repo, &snapshot, previous, tx).await {
                Ok(result) => result,
                Err(error) => {
                    eprintln!(
                        "clawhip source GitHub issue processing failed for {}: {error}",
                        repo.path
                    );
                    (
                        previous
                            .map(|entry| entry.issues.clone())
                            .unwrap_or_default(),
                        previous.map(|entry| entry.issues_ready).unwrap_or(false),
                    )
                }
            };
        let (prs, prs_ready) =
            match poll_pull_requests(config, github_client, repo, &snapshot, previous, tx).await {
                Ok(result) => result,
                Err(error) => {
                    eprintln!(
                        "clawhip source GitHub pull request processing failed for {}: {error}",
                        repo.path
                    );
                    (
                        previous.map(|entry| entry.prs.clone()).unwrap_or_default(),
                        previous.map(|entry| entry.prs_ready).unwrap_or(false),
                    )
                }
            };
        let (pr_reviews, pr_reviews_ready) =
            match poll_pr_reviews(config, github_client, repo, &snapshot, previous, &prs, tx).await
            {
                Ok(result) => result,
                Err(error) => {
                    eprintln!(
                        "clawhip source GitHub PR review processing failed for {}: {error}",
                        repo.path
                    );
                    (
                        previous
                            .map(|entry| entry.pr_reviews.clone())
                            .unwrap_or_default(),
                        previous
                            .map(|entry| entry.pr_reviews_ready)
                            .unwrap_or(false),
                    )
                }
            };
        let (ci, ci_ready) = match poll_ci_statuses(
            config,
            github_client,
            repo,
            &snapshot,
            previous,
            &prs,
            tx,
        )
        .await
        {
            Ok(result) => result,
            Err(error) => {
                eprintln!(
                    "clawhip source GitHub CI processing failed for {}: {error}",
                    repo.path
                );
                (
                    previous.map(|entry| entry.ci.clone()).unwrap_or_default(),
                    previous.map(|entry| entry.ci_ready).unwrap_or(false),
                )
            }
        };

        state.insert(
            repo.path.clone(),
            GitHubRepoState {
                issues,
                issues_ready,
                prs,
                prs_ready,
                pr_reviews,
                pr_reviews_ready,
                ci,
                ci_ready,
            },
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
) -> Result<(HashMap<u64, IssueSnapshot>, bool)> {
    if !repo.emit_issue_opened {
        return Ok((
            previous
                .map(|entry| entry.issues.clone())
                .unwrap_or_default(),
            previous.map(|entry| entry.issues_ready).unwrap_or(false),
        ));
    }

    let Some(client) = github_client else {
        return Ok((
            previous
                .map(|entry| entry.issues.clone())
                .unwrap_or_default(),
            previous.map(|entry| entry.issues_ready).unwrap_or(false),
        ));
    };

    match fetch_issues(client, &config.monitors.github_api_base, repo, snapshot).await {
        Ok(issues) => {
            if let Some(previous) = previous.filter(|entry| entry.issues_ready) {
                for event in collect_issue_events(
                    client,
                    &config.monitors.github_api_base,
                    snapshot.github_repo.as_deref(),
                    repo,
                    &snapshot.repo_name,
                    &previous.issues,
                    &issues,
                )
                .await
                {
                    send_event(tx, event).await?;
                }
            } else {
                eprintln!(
                    "clawhip source GitHub issue baseline established for {}; suppressing initial {} issue events",
                    repo.path,
                    issues.len()
                );
            }
            Ok((issues, true))
        }
        Err(error) => {
            eprintln!(
                "clawhip source GitHub issue polling failed for {}: {error}",
                repo.path
            );
            Ok((
                previous
                    .map(|entry| entry.issues.clone())
                    .unwrap_or_default(),
                previous.map(|entry| entry.issues_ready).unwrap_or(false),
            ))
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
) -> Result<(HashMap<u64, PullRequestSnapshot>, bool)> {
    if !repo.emit_pr_status {
        return Ok((
            previous.map(|entry| entry.prs.clone()).unwrap_or_default(),
            previous.map(|entry| entry.prs_ready).unwrap_or(false),
        ));
    }

    let Some(client) = github_client else {
        return Ok((
            previous.map(|entry| entry.prs.clone()).unwrap_or_default(),
            previous.map(|entry| entry.prs_ready).unwrap_or(false),
        ));
    };

    match fetch_pull_requests(client, &config.monitors.github_api_base, repo, snapshot).await {
        Ok(prs) => {
            if let Some(previous) = previous.filter(|entry| entry.prs_ready) {
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
                                    pr.body.clone(),
                                    repo.channel.clone(),
                                )
                                .with_mention(repo.mention.clone())
                                .with_format(repo.format.clone()),
                            )
                            .await?;
                        }
                    }
                }
            } else {
                eprintln!(
                    "clawhip source GitHub PR baseline established for {}; suppressing initial {} PR events",
                    repo.path,
                    prs.len()
                );
            }
            Ok((prs, true))
        }
        Err(error) => {
            eprintln!(
                "clawhip source GitHub polling failed for {}: {error}",
                repo.path
            );
            Ok((
                previous.map(|entry| entry.prs.clone()).unwrap_or_default(),
                previous.map(|entry| entry.prs_ready).unwrap_or(false),
            ))
        }
    }
}

async fn poll_pr_reviews(
    config: &AppConfig,
    github_client: Option<&reqwest::Client>,
    repo: &GitRepoMonitor,
    snapshot: &GitSnapshot,
    previous: Option<&GitHubRepoState>,
    prs: &HashMap<u64, PullRequestSnapshot>,
    tx: &mpsc::Sender<IncomingEvent>,
) -> Result<(HashMap<u64, HashMap<u64, ReviewSnapshot>>, bool)> {
    if !repo.emit_pr_reviews {
        return Ok((
            previous
                .map(|entry| entry.pr_reviews.clone())
                .unwrap_or_default(),
            previous
                .map(|entry| entry.pr_reviews_ready)
                .unwrap_or(false),
        ));
    }

    let Some(client) = github_client else {
        return Ok((
            previous
                .map(|entry| entry.pr_reviews.clone())
                .unwrap_or_default(),
            previous
                .map(|entry| entry.pr_reviews_ready)
                .unwrap_or(false),
        ));
    };

    let Some(github_repo) = snapshot.github_repo.as_deref() else {
        return Ok((HashMap::new(), false));
    };

    let mut current = HashMap::new();
    let mut fetch_failed = false;
    for (number, pr) in prs {
        if pr.status == "merged" {
            continue;
        }
        match fetch_pr_reviews(
            client,
            &config.monitors.github_api_base,
            github_repo,
            *number,
        )
        .await
        {
            Ok(reviews) => {
                current.insert(*number, reviews);
            }
            Err(error) => {
                eprintln!(
                    "clawhip source GitHub PR review fetch failed for {github_repo}#{number}: {error}"
                );
                fetch_failed = true;
                if let Some(prev_reviews) = previous.and_then(|entry| entry.pr_reviews.get(number))
                {
                    current.insert(*number, prev_reviews.clone());
                }
            }
        }
    }

    let baseline_ready = previous
        .map(|entry| entry.pr_reviews_ready)
        .unwrap_or(false);
    if !baseline_ready {
        eprintln!(
            "clawhip source GitHub PR review baseline established for {}; suppressing initial reviews",
            repo.path
        );
        return Ok((current, true));
    }

    for (number, reviews) in &current {
        let Some(pr) = prs.get(number) else { continue };
        let prev_reviews = previous
            .and_then(|entry| entry.pr_reviews.get(number))
            .cloned()
            .unwrap_or_default();
        for (review_id, review) in reviews {
            if prev_reviews.contains_key(review_id) {
                continue;
            }
            send_event(
                tx,
                IncomingEvent::github_pr_review_submitted(
                    snapshot.repo_name.clone(),
                    *number,
                    pr.title.clone(),
                    review.state.clone(),
                    if review.body.is_empty() {
                        None
                    } else {
                        Some(review.body.clone())
                    },
                    review.actor.clone(),
                    repo.channel.clone(),
                )
                .with_mention(repo.mention.clone())
                .with_format(repo.format.clone()),
            )
            .await?;
        }
    }

    Ok((current, !fetch_failed))
}

async fn poll_ci_statuses(
    config: &AppConfig,
    github_client: Option<&reqwest::Client>,
    repo: &GitRepoMonitor,
    snapshot: &GitSnapshot,
    previous: Option<&GitHubRepoState>,
    prs: &HashMap<u64, PullRequestSnapshot>,
    tx: &mpsc::Sender<IncomingEvent>,
) -> Result<(HashMap<String, GitHubCISnapshot>, bool)> {
    if !repo.emit_pr_status {
        return Ok((
            previous.map(|entry| entry.ci.clone()).unwrap_or_default(),
            previous.map(|entry| entry.ci_ready).unwrap_or(false),
        ));
    }

    let Some(client) = github_client else {
        return Ok((
            previous.map(|entry| entry.ci.clone()).unwrap_or_default(),
            previous.map(|entry| entry.ci_ready).unwrap_or(false),
        ));
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
            if let Some(previous) = previous.filter(|entry| entry.ci_ready) {
                for event in collect_ci_events(repo, &snapshot.repo_name, &previous.ci, &ci) {
                    send_event(tx, event).await?;
                }
            } else {
                eprintln!(
                    "clawhip source GitHub CI baseline established for {}; suppressing initial {} CI events",
                    repo.path,
                    ci.len()
                );
            }
            Ok((ci, true))
        }
        Err(error) => {
            eprintln!(
                "clawhip source GitHub CI polling failed for {}: {error}",
                repo.path
            );
            Ok((
                previous.map(|entry| entry.ci.clone()).unwrap_or_default(),
                previous.map(|entry| entry.ci_ready).unwrap_or(false),
            ))
        }
    }
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

async fn collect_issue_events(
    client: &reqwest::Client,
    api_base: &str,
    github_repo: Option<&str>,
    repo: &GitRepoMonitor,
    repo_name: &str,
    previous: &HashMap<u64, IssueSnapshot>,
    current: &HashMap<u64, IssueSnapshot>,
) -> Vec<IncomingEvent> {
    let mut events = Vec::new();
    for (number, issue) in current {
        match previous.get(number) {
            None => events.push(
                IncomingEvent::github_issue_opened_rich(
                    repo_name.to_string(),
                    *number,
                    issue.title.clone(),
                    Some(issue.html_url.clone()),
                    issue.labels.clone(),
                    body_preview(&issue.body),
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
                let added: Vec<&String> = issue
                    .labels
                    .iter()
                    .filter(|name| !old.labels.contains(name))
                    .collect();
                for label_name in added {
                    let actor = match github_repo {
                        Some(gh) => {
                            fetch_label_actor(client, api_base, gh, *number, label_name).await
                        }
                        None => None,
                    };
                    events.push(
                        IncomingEvent::github_issues_labeled(
                            repo_name.to_string(),
                            *number,
                            issue.title.clone(),
                            label_name.clone(),
                            actor,
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
        let Some(old) = previous.get(key) else {
            // GitHub's Actions APIs are eventually consistent and may surface
            // older completed runs after clawhip restarts, after pagination
            // churn, or after a transient API failure. Treating every newly
            // discovered terminal run as a fresh event replays stale CI pass/fail
            // notifications. Only emit a first-seen CI event while the run is
            // still active; terminal pass/fail/cancel notifications require a
            // prior observed state transition.
            if is_terminal_ci(&ci.status) {
                continue;
            }
            events.push(ci_event(repo, repo_name, ci));
            continue;
        };

        let changed = old.status != ci.status || old.conclusion != ci.conclusion;
        if !changed {
            continue;
        }

        events.push(ci_event(repo, repo_name, ci));
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

fn ci_event(repo: &GitRepoMonitor, repo_name: &str, ci: &GitHubCISnapshot) -> IncomingEvent {
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
    event
}

fn is_terminal_ci(status: &str) -> bool {
    status == "completed"
}

/// Look up who applied a specific label to an issue, by scanning the
/// issue's events feed for the most recent `labeled` action carrying
/// that label name. Returns `None` when the call fails or the actor
/// is unknown — callers must NOT block emission on this lookup.
///
/// The events endpoint returns oldest-first; we walk in reverse so
/// repeated label cycles (label → unlabel → label) attribute to the
/// most recent labeler.
async fn fetch_label_actor(
    client: &reqwest::Client,
    api_base: &str,
    github_repo: &str,
    issue_number: u64,
    label_name: &str,
) -> Option<String> {
    #[derive(Deserialize)]
    struct IssueEvent {
        event: String,
        #[serde(default)]
        actor: Option<EventActor>,
        #[serde(default)]
        label: Option<GitHubLabel>,
    }

    #[derive(Deserialize)]
    struct EventActor {
        login: String,
    }

    let response = github_get(
        client,
        api_base,
        &format!("repos/{github_repo}/issues/{issue_number}/events"),
        &[("per_page", "100")],
        &format!("issue events for {github_repo}#{issue_number}"),
    )
    .await
    .ok()?;
    let events: Vec<IssueEvent> = response.json().await.ok()?;
    events
        .into_iter()
        .rev()
        .find(|e| {
            e.event == "labeled"
                && e.label
                    .as_ref()
                    .map(|l| l.name == label_name)
                    .unwrap_or(false)
        })
        .and_then(|e| e.actor.map(|a| a.login))
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
                    html_url: issue.html_url,
                    labels: issue.labels.into_iter().map(|label| label.name).collect(),
                    body: issue.body,
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
                    body: pull.body,
                },
            )
        })
        .collect())
}

async fn fetch_pr_reviews(
    client: &reqwest::Client,
    api_base: &str,
    github_repo: &str,
    pr_number: u64,
) -> Result<HashMap<u64, ReviewSnapshot>> {
    let response = github_get(
        client,
        api_base,
        &format!("repos/{github_repo}/pulls/{pr_number}/reviews"),
        &[("per_page", "100")],
        &format!("PR reviews for {github_repo}#{pr_number}"),
    )
    .await?;
    let reviews: Vec<GitHubPullRequestReview> = response.json().await?;
    Ok(reviews
        .into_iter()
        .filter(|review| !review.state.is_empty() && review.state != "PENDING")
        .map(|review| {
            (
                review.id,
                ReviewSnapshot {
                    state: normalize_review_state(&review.state),
                    body: review.body,
                    actor: review.user.map(|user| user.login),
                },
            )
        })
        .collect())
}

fn normalize_review_state(raw: &str) -> String {
    match raw.to_uppercase().as_str() {
        "APPROVED" => "approved".to_string(),
        "CHANGES_REQUESTED" => "changes_requested".to_string(),
        "COMMENTED" => "commented".to_string(),
        "DISMISSED" => "dismissed".to_string(),
        other => other.to_lowercase(),
    }
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
    html_url: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    labels: Vec<GitHubLabel>,
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct GitHubLabel {
    name: String,
}

fn body_preview(body: &str) -> Option<String> {
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        None
    } else if collapsed.chars().count() > 180 {
        Some(collapsed.chars().take(177).collect::<String>() + "…")
    } else {
        Some(collapsed)
    }
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
    #[serde(default)]
    body: String,
}

#[derive(Deserialize)]
struct GitHubPullRequestHead {
    #[serde(rename = "ref")]
    reference: String,
    sha: String,
}

#[derive(Deserialize)]
struct GitHubPullRequestReview {
    id: u64,
    #[serde(default)]
    state: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    user: Option<GitHubReviewUser>,
}

#[derive(Deserialize)]
struct GitHubReviewUser {
    login: String,
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
                html_url: "https://example.test/issues/2".into(),
                labels: Vec::new(),
                body: String::new(),
            },
        )]
        .into_iter()
        .collect();
        let client = build_github_client(None).unwrap();
        let events = collect_issue_events(
            &client,
            "http://127.0.0.1:1",
            None,
            &repo,
            "clawhip",
            &previous,
            &current,
        )
        .await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].canonical_kind(), "github.issue-opened");
        assert_eq!(events[0].payload["repo"], "clawhip");

        let config = AppConfig {
            defaults: DefaultsConfig {
                channel: Some("fallback".into()),
                format: MessageFormat::Compact,
            },
            routes: vec![RouteRule {
                event: "github.*".into(),
                sink: "discord".into(),
                filter: [("repo".to_string(), "clawhip".to_string())]
                    .into_iter()
                    .collect(),
                channel: Some("route-channel".into()),
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

    #[tokio::test]
    async fn issue_comment_and_close_events_are_emitted() {
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
                html_url: String::new(),
                labels: Vec::new(),
                body: String::new(),
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
                html_url: String::new(),
                labels: Vec::new(),
                body: String::new(),
            },
        )]
        .into_iter()
        .collect();
        let client = build_github_client(None).unwrap();
        let events = collect_issue_events(
            &client,
            "http://127.0.0.1:1",
            None,
            &repo,
            "clawhip",
            &previous,
            &current,
        )
        .await;
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

    #[tokio::test]
    async fn newly_added_label_emits_issues_labeled_event_with_actor_login() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let _ = stream.read(&mut buf).await.unwrap();
            let body = json!([
                { "event": "renamed", "actor": {"login": "human-author"} },
                { "event": "labeled", "label": {"name": "iyen:auto-fix"}, "actor": {"login": "openclaw-bot"} }
            ])
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let repo = GitRepoMonitor {
            path: "/tmp/clawhip".into(),
            ..GitRepoMonitor::default()
        };
        let previous = [(
            42_u64,
            IssueSnapshot {
                title: "bug".into(),
                state: "open".into(),
                comments: 0,
                html_url: String::new(),
                labels: vec!["bug".into()],
                body: String::new(),
            },
        )]
        .into_iter()
        .collect();
        let current = [(
            42_u64,
            IssueSnapshot {
                title: "bug".into(),
                state: "open".into(),
                comments: 0,
                html_url: String::new(),
                labels: vec!["bug".into(), "iyen:auto-fix".into()],
                body: String::new(),
            },
        )]
        .into_iter()
        .collect();

        let client = build_github_client(None).unwrap();
        let api_base = format!("http://{addr}");
        let events = collect_issue_events(
            &client,
            &api_base,
            Some("Org/Repo"),
            &repo,
            "Repo",
            &previous,
            &current,
        )
        .await;

        server.await.unwrap();
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.canonical_kind(), "github.issues-labeled");
        assert_eq!(event.payload["repo"], "Repo");
        assert_eq!(event.payload["number"], 42);
        assert_eq!(event.payload["label"]["name"], "iyen:auto-fix");
        assert_eq!(event.payload["sender"]["login"], "openclaw-bot");
        assert_eq!(event.payload["issue"]["title"], "bug");
    }

    #[tokio::test]
    async fn unchanged_labels_do_not_emit_labeled_event() {
        let repo = GitRepoMonitor {
            path: "/tmp/clawhip".into(),
            ..GitRepoMonitor::default()
        };
        let labels = vec!["bug".into(), "iyen:auto-fix".into()];
        let previous = [(
            42_u64,
            IssueSnapshot {
                title: "bug".into(),
                state: "open".into(),
                comments: 0,
                html_url: String::new(),
                labels: labels.clone(),
                body: String::new(),
            },
        )]
        .into_iter()
        .collect();
        let current = [(
            42_u64,
            IssueSnapshot {
                title: "bug".into(),
                state: "open".into(),
                comments: 0,
                html_url: String::new(),
                labels,
                body: String::new(),
            },
        )]
        .into_iter()
        .collect();
        let client = build_github_client(None).unwrap();
        let events = collect_issue_events(
            &client,
            "http://127.0.0.1:1",
            Some("Org/Repo"),
            &repo,
            "Repo",
            &previous,
            &current,
        )
        .await;
        assert!(
            !events
                .iter()
                .any(|e| e.canonical_kind() == "github.issues-labeled"),
            "label set unchanged → no labeled event should be emitted"
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

        let (ci, ci_ready) =
            poll_ci_statuses(&config, Some(&client), &repo, &snapshot, None, &prs, &tx)
                .await
                .unwrap();

        assert_eq!(ci.len(), 1);
        assert!(ci_ready);
        assert!(rx.try_recv().is_err());

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
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
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
            body: "PR body".into(),
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
    fn newly_discovered_terminal_ci_state_is_suppressed() {
        let repo = GitRepoMonitor {
            path: "/tmp/clawhip".into(),
            ..GitRepoMonitor::default()
        };
        let previous = HashMap::new();
        let current_ci = ci_snapshot(58, "CI / test", "completed", Some("success"));
        let current = [(current_ci.dedupe_key(), current_ci)]
            .into_iter()
            .collect();

        let events = collect_ci_events(&repo, "clawhip", &previous, &current);
        assert!(events.is_empty());
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

    #[test]
    fn normalize_review_state_maps_github_uppercase_to_clawhip_lowercase() {
        assert_eq!(normalize_review_state("APPROVED"), "approved");
        assert_eq!(
            normalize_review_state("CHANGES_REQUESTED"),
            "changes_requested"
        );
        assert_eq!(normalize_review_state("COMMENTED"), "commented");
        assert_eq!(normalize_review_state("DISMISSED"), "dismissed");
        assert_eq!(normalize_review_state("approved"), "approved");
    }

    #[tokio::test]
    async fn fetch_pr_reviews_filters_pending_and_normalizes_state() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let _ = stream.read(&mut buf).await.unwrap();
            let body = json!([
                { "id": 1, "state": "APPROVED", "body": "lgtm", "user": {"login": "alice"} },
                { "id": 2, "state": "CHANGES_REQUESTED", "body": "fix this", "user": {"login": "bob"} },
                { "id": 3, "state": "PENDING", "body": "draft", "user": {"login": "carol"} }
            ])
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let client = build_github_client(None).unwrap();
        let api_base = format!("http://{addr}");
        let reviews = fetch_pr_reviews(&client, &api_base, "owner/repo", 7)
            .await
            .unwrap();
        server.await.unwrap();

        assert_eq!(reviews.len(), 2, "PENDING reviews must be filtered out");
        assert_eq!(reviews.get(&1).unwrap().state, "approved");
        assert_eq!(reviews.get(&2).unwrap().state, "changes_requested");
        assert_eq!(reviews.get(&1).unwrap().actor.as_deref(), Some("alice"));
    }
}
