//! Route matching logic — candidates, glob matching, specificity scoring.

use std::collections::BTreeMap;

use crate::config::RouteRule;

/// Determine candidate event patterns for a given canonical event kind.
///
/// "git.commit" can also match "github.commit" routes, "agent.*" events
/// can also match "session.*", etc.
pub(crate) fn route_candidates(kind: &str) -> Vec<&str> {
    match kind {
        "git.commit" => vec!["git.commit", "github.commit"],
        "git.branch-changed" => vec!["git.branch-changed", "github.branch-changed"],
        "agent.started" | "agent.blocked" | "agent.finished" | "agent.failed" => {
            vec![kind, "agent.*", "session.*"]
        }
        "session.started" | "session.blocked" | "session.finished" | "session.failed" => {
            vec![kind, "session.*", "agent.*"]
        }
        "session.retry-needed"
        | "session.pr-created"
        | "session.test-started"
        | "session.test-finished"
        | "session.test-failed"
        | "session.handoff-needed" => vec![kind, "session.*"],
        other => vec![other],
    }
}

/// Check whether a single route rule matches a canonical event.
pub(crate) fn route_matches(
    route: &RouteRule,
    canonical_kind: &str,
    context: &BTreeMap<String, String>,
) -> bool {
    route_candidates(canonical_kind)
        .iter()
        .any(|candidate| glob_match(&route.event, candidate))
        && route.filter.iter().all(|(key, expected)| {
            context
                .get(key)
                .map(|actual| glob_match(expected, actual))
                .unwrap_or(false)
        })
}

/// Find all route rules that match a given canonical event, sorted by
/// specificity (most specific first).
///
/// When the event has repo metadata, routes using session-name prefix
/// heuristics are deprioritised in favour of metadata-aware routes.
pub(crate) fn matching_routes_for<'a>(
    routes: &'a [RouteRule],
    canonical_kind: &str,
    context: &BTreeMap<String, String>,
) -> Vec<&'a RouteRule> {
    let prefer_metadata = prefers_metadata_first_routing(canonical_kind, context);
    let mut preferred = Vec::new();
    let mut heuristic = Vec::new();

    for route in routes
        .iter()
        .filter(|route| route_matches(route, canonical_kind, context))
    {
        if prefer_metadata && route_uses_session_name_prefix_heuristics(route) {
            heuristic.push(route);
        } else {
            preferred.push(route);
        }
    }

    preferred.sort_by(|left, right| {
        route_specificity_score(right, context).cmp(&route_specificity_score(left, context))
    });
    heuristic.sort_by(|left, right| {
        route_specificity_score(right, context).cmp(&route_specificity_score(left, context))
    });

    if !prefer_metadata {
        preferred.extend(heuristic);
    }

    preferred
}

/// Compute a numeric specificity score for a route rule.
///
/// Routes with more specific path filters (worktree_path > repo_path > repo_name)
/// are ranked higher, with the total filter count as a tiebreaker.
fn route_specificity_score(
    route: &RouteRule,
    context: &BTreeMap<String, String>,
) -> usize {
    let path_rank = if route.filter.contains_key("worktree_path")
        && context
            .get("worktree_path")
            .is_some_and(|value| !value.trim().is_empty())
    {
        3
    } else if route.filter.contains_key("repo_path")
        && context
            .get("repo_path")
            .is_some_and(|value| !value.trim().is_empty())
    {
        2
    } else if route.filter.contains_key("repo_name")
        && context
            .get("repo_name")
            .is_some_and(|value| !value.trim().is_empty())
    {
        1
    } else {
        0
    };

    (path_rank * 100) + route.filter.len()
}

/// Whether the event has enough repo/project metadata to deprioritise
/// session-name-prefix heuristics for tmux and session events.
pub(crate) fn prefers_metadata_first_routing(
    canonical_kind: &str,
    context: &BTreeMap<String, String>,
) -> bool {
    if !(canonical_kind.starts_with("session.") || canonical_kind.starts_with("tmux.")) {
        return false;
    }

    ["project", "repo_name", "repo_path", "worktree_path", "session_id"]
        .into_iter()
        .filter_map(|key| context.get(key))
        .any(|value| !value.trim().is_empty())
}

/// Whether a route rule relies solely on session-name-prefix
/// filters (e.g. `session = "clawhip-*"`).
pub(crate) fn route_uses_session_name_prefix_heuristics(route: &RouteRule) -> bool {
    !route.filter.is_empty()
        && route.filter.iter().all(|(key, expected)| {
            matches!(key.as_str(), "session" | "session_name") && expected.contains('*')
        })
}

/// A simple glob-style matcher that supports `*` wildcards.
///
/// `*` matches any sequence of characters (including empty).
/// A pattern without `*` is treated as an exact match.
pub(crate) fn glob_match(pattern: &str, value: &str) -> bool {
    if pattern == value {
        return true;
    }
    if !pattern.contains('*') {
        return false;
    }

    let mut remainder = value;
    let parts: Vec<&str> = pattern.split('*').collect();
    let starts_with_wildcard = pattern.starts_with('*');
    let ends_with_wildcard = pattern.ends_with('*');

    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }

        if index == 0 && !starts_with_wildcard {
            if !remainder.starts_with(part) {
                return false;
            }
            remainder = &remainder[part.len()..];
            continue;
        }

        if index == parts.len() - 1 && !ends_with_wildcard {
            return remainder.ends_with(part);
        }

        if let Some(position) = remainder.find(part) {
            remainder = &remainder[(position + part.len())..];
        } else {
            return false;
        }
    }

    ends_with_wildcard || remainder.is_empty()
}
