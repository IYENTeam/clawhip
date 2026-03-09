use std::collections::HashSet;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeywordHit {
    pub keyword: String,
    pub line: String,
}

#[derive(Debug, Clone)]
pub struct PendingKeywordHits {
    started_at: Instant,
    hits: Vec<KeywordHit>,
    seen: HashSet<(String, String)>,
}

impl PendingKeywordHits {
    pub fn new(started_at: Instant) -> Self {
        Self {
            started_at,
            hits: Vec::new(),
            seen: HashSet::new(),
        }
    }

    pub fn push(&mut self, hits: Vec<KeywordHit>) {
        for hit in hits {
            let key = (hit.keyword.clone(), hit.line.clone());
            if self.seen.insert(key) {
                self.hits.push(hit);
            }
        }
    }

    pub fn ready_to_flush(&self, now: Instant, window: Duration) -> bool {
        now.duration_since(self.started_at) >= window
    }

    pub fn into_hits(self) -> Vec<KeywordHit> {
        self.hits
    }
}

pub fn collect_keyword_hits(previous: &str, current: &str, keywords: &[String]) -> Vec<KeywordHit> {
    if keywords.is_empty() {
        return Vec::new();
    }

    let previous_lines: HashSet<&str> = previous.lines().collect();
    let normalized_keywords = keywords
        .iter()
        .map(|keyword| (keyword.clone(), keyword.to_ascii_lowercase()))
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    let mut hits = Vec::new();

    for line in current
        .lines()
        .filter(|line| !previous_lines.contains(*line))
    {
        let lower_line = line.to_ascii_lowercase();
        for (keyword, lower_keyword) in &normalized_keywords {
            if lower_line.contains(lower_keyword) {
                let key = (keyword.clone(), line.to_string());
                if seen.insert(key.clone()) {
                    hits.push(KeywordHit {
                        keyword: key.0,
                        line: key.1,
                    });
                }
            }
        }
    }

    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_keyword_hits_dedups_same_keyword_and_line() {
        let hits = collect_keyword_hits(
            "done",
            "done\nerror: failed\nerror: failed\nERROR: FAILED",
            &["error".into()],
        );

        assert_eq!(
            hits,
            vec![
                KeywordHit {
                    keyword: "error".into(),
                    line: "error: failed".into(),
                },
                KeywordHit {
                    keyword: "error".into(),
                    line: "ERROR: FAILED".into(),
                },
            ]
        );
    }

    #[test]
    fn pending_keyword_hits_dedups_across_window_additions() {
        let start = Instant::now();
        let mut pending = PendingKeywordHits::new(start);
        pending.push(vec![KeywordHit {
            keyword: "error".into(),
            line: "error: failed".into(),
        }]);
        pending.push(vec![
            KeywordHit {
                keyword: "error".into(),
                line: "error: failed".into(),
            },
            KeywordHit {
                keyword: "complete".into(),
                line: "complete".into(),
            },
        ]);

        assert_eq!(
            pending.into_hits(),
            vec![
                KeywordHit {
                    keyword: "error".into(),
                    line: "error: failed".into(),
                },
                KeywordHit {
                    keyword: "complete".into(),
                    line: "complete".into(),
                },
            ]
        );
    }

    #[test]
    fn pending_keyword_hits_flush_when_window_expires() {
        let start = Instant::now();
        let pending = PendingKeywordHits::new(start);

        assert!(!pending.ready_to_flush(start + Duration::from_secs(29), Duration::from_secs(30)));
        assert!(pending.ready_to_flush(start + Duration::from_secs(30), Duration::from_secs(30)));
    }
}
