use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::process::Command;
use tokio::time::timeout;

const TOKEN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_OUTPUT_CHARS: usize = 1200;
const MAX_TAIL_LINES: usize = 200;

pub async fn render_template(
    template: &str,
    context: &BTreeMap<String, String>,
    allow_dynamic_tokens: bool,
) -> String {
    let rendered = crate::events::render_template(template, context);
    if !allow_dynamic_tokens {
        return rendered;
    }

    let mut output = String::with_capacity(rendered.len());
    let mut remainder = rendered.as_str();

    while let Some(start) = remainder.find('{') {
        output.push_str(&remainder[..start]);
        let after_start = &remainder[start + 1..];
        let Some(end) = after_start.find('}') else {
            output.push_str(&remainder[start..]);
            return output;
        };

        let token = &after_start[..end];
        if let Some(value) = evaluate_token(token).await {
            output.push_str(&value);
        } else {
            output.push('{');
            output.push_str(token);
            output.push('}');
        }

        remainder = &after_start[end + 1..];
    }

    output.push_str(remainder);
    output
}

async fn evaluate_token(token: &str) -> Option<String> {
    match token {
        "now" => Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_secs()
                .to_string(),
        ),
        "iso_time" => Some(
            OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .ok()
                .map(|value| cap_output(&value))?,
        ),
        _ if token.starts_with("env:") => std::env::var(token.trim_start_matches("env:"))
            .ok()
            .map(|value| cap_output(value.trim())),
        _ if token.starts_with("sh:") => run_shell_token(token.trim_start_matches("sh:")).await,
        _ if token.starts_with("tmux_tail:") => {
            run_tmux_tail_token(token.trim_start_matches("tmux_tail:")).await
        }
        _ if token.starts_with("file_tail:") => {
            run_file_tail_token(token.trim_start_matches("file_tail:")).await
        }
        _ => None,
    }
}

async fn run_shell_token(command: &str) -> Option<String> {
    let output = timeout(
        TOKEN_TIMEOUT,
        Command::new("sh").arg("-lc").arg(command).output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(cap_output(String::from_utf8_lossy(&output.stdout).trim()))
}

async fn run_tmux_tail_token(spec: &str) -> Option<String> {
    let (target, lines) = parse_tail_spec(spec)?;
    let output = timeout(
        TOKEN_TIMEOUT,
        Command::new(tmux_bin())
            .arg("capture-pane")
            .arg("-p")
            .arg("-t")
            .arg(target)
            .arg("-S")
            .arg(format!("-{}", lines))
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(cap_output(String::from_utf8_lossy(&output.stdout).trim()))
}

async fn run_file_tail_token(spec: &str) -> Option<String> {
    let (path, lines) = parse_tail_spec(spec)?;
    let bytes = timeout(TOKEN_TIMEOUT, tokio::fs::read(path))
        .await
        .ok()?
        .ok()?;
    let content = String::from_utf8_lossy(&bytes);
    let tail = content
        .lines()
        .rev()
        .take(lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    Some(cap_output(tail.trim()))
}

fn parse_tail_spec(spec: &str) -> Option<(&str, usize)> {
    let (target, lines) = spec.rsplit_once(':')?;
    let lines: usize = lines.parse().ok()?;
    Some((target, lines.clamp(1, MAX_TAIL_LINES)))
}

fn cap_output(value: impl AsRef<str>) -> String {
    let trimmed = value.as_ref().trim();
    let mut out = String::new();
    for ch in trimmed.chars().take(MAX_OUTPUT_CHARS) {
        out.push(ch);
    }
    if trimmed.chars().count() > MAX_OUTPUT_CHARS {
        out.push('…');
    }
    out
}

fn tmux_bin() -> String {
    std::env::var("OP_PI_TMUX_BIN").unwrap_or_else(|_| "tmux".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn disabled_dynamic_tokens_remain_literal() {
        let rendered = render_template("build {sh:printf hi}", &BTreeMap::new(), false).await;
        assert_eq!(rendered, "build {sh:printf hi}");
    }

    #[tokio::test]
    async fn enabled_shell_token_expands() {
        let rendered = render_template("build {sh:printf hi}", &BTreeMap::new(), true).await;
        assert_eq!(rendered, "build hi");
    }

    #[tokio::test]
    async fn env_and_file_tail_tokens_expand() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "one\ntwo\nthree").unwrap();
        let file_path = file.path().display().to_string();
        let rendered = render_template(
            &format!("{{file_tail:{}:2}}", file_path),
            &BTreeMap::new(),
            true,
        )
        .await;
        assert!(rendered.contains("two\nthree") || rendered.contains("two\r\nthree"));
    }
}
