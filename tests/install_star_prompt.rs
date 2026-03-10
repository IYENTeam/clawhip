use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn install_script() -> PathBuf {
    repo_root().join("install.sh")
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write file");
    let mut perms = fs::metadata(path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("chmod");
}

fn fake_gh_script() -> &'static str {
    r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$GH_LOG"
case "${1:-} ${2:-}" in
  "auth status")
    exit "${GH_AUTH_EXIT_CODE:-0}"
    ;;
  "api --method")
    exit "${GH_STAR_EXIT_CODE:-0}"
    ;;
esac
"#
}

fn run_shell(temp: &TempDir, script_body: &str, extra_env: &[(&str, &str)]) -> Output {
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("create bin dir");
    write_executable(&bin_dir.join("gh"), fake_gh_script());

    let script_path = temp.path().join("runner.sh");
    let script = format!(
        "#!/usr/bin/env bash\nset -euo pipefail\nsource \"{}\"\n{}\n",
        install_script().display(),
        script_body
    );
    write_executable(&script_path, &script);

    let existing_path = std::env::var("PATH").unwrap_or_default();
    let mut command = Command::new("bash");
    command.arg(script_path);
    command.env("PATH", format!("{}:{}", bin_dir.display(), existing_path));
    command.env("GH_LOG", temp.path().join("gh.log"));
    command.env("HOME", temp.path().join("home"));
    command.env("CARGO_HOME", temp.path().join("cargo"));
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.output().expect("run shell script")
}

#[test]
fn skips_star_prompt_when_not_interactive() {
    let temp = TempDir::new().expect("tempdir");
    let output = run_shell(
        &temp,
        r#"
can_use_github_cli_for_star() {
  echo invoked >> "$HOME/can-use.log"
  return 0
}
maybe_prompt_to_star_repo
"#,
        &[],
    );

    assert!(output.status.success(), "script failed: {output:?}");
    assert!(!temp.path().join("home/can-use.log").exists());
    assert!(!temp.path().join("gh.log").exists());
}

#[test]
fn skip_flag_or_env_disables_star_prompt() {
    let temp = TempDir::new().expect("tempdir");
    let output = run_shell(
        &temp,
        r#"
SKIP_STAR_PROMPT=1
is_interactive_install() {
  return 0
}
can_use_github_cli_for_star() {
  echo invoked >> "$HOME/can-use.log"
  return 0
}
maybe_prompt_to_star_repo <<'EOF_INPUT'
y
EOF_INPUT
"#,
        &[],
    );

    assert!(output.status.success(), "script failed: {output:?}");
    assert!(!temp.path().join("home/can-use.log").exists());
    assert!(!temp.path().join("gh.log").exists());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("skipping GitHub star prompt"),
        "stdout was: {stdout}"
    );
}

#[test]
fn skips_prompt_when_gh_is_unauthenticated() {
    let temp = TempDir::new().expect("tempdir");
    let output = run_shell(
        &temp,
        r#"
is_interactive_install() {
  return 0
}
maybe_prompt_to_star_repo
"#,
        &[("GH_AUTH_EXIT_CODE", "1")],
    );

    assert!(output.status.success(), "script failed: {output:?}");
    let gh_log = fs::read_to_string(temp.path().join("gh.log")).expect("gh log");
    assert_eq!(gh_log.trim(), "auth status");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Would you like to star"),
        "stdout was: {stdout}"
    );
}

#[test]
fn stars_repo_only_after_explicit_yes() {
    let temp = TempDir::new().expect("tempdir");
    let output = run_shell(
        &temp,
        r#"
prompt_to_star_repo <<'EOF_INPUT'
y
EOF_INPUT
"#,
        &[],
    );

    assert!(output.status.success(), "script failed: {output:?}");
    let gh_log = fs::read_to_string(temp.path().join("gh.log")).expect("gh log");
    assert_eq!(
        gh_log.trim(),
        "api --method PUT /user/starred/Yeachan-Heo/clawhip --silent"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("thanks for starring"),
        "stdout was: {stdout}"
    );
}

#[test]
fn star_failure_does_not_fail_the_script() {
    let temp = TempDir::new().expect("tempdir");
    let output = run_shell(
        &temp,
        r#"
prompt_to_star_repo <<'EOF_INPUT'
yes
EOF_INPUT
echo after-prompt
"#,
        &[("GH_STAR_EXIT_CODE", "1")],
    );

    assert!(output.status.success(), "script failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("continuing without it"),
        "stdout was: {stdout}"
    );
    assert!(stdout.contains("after-prompt"), "stdout was: {stdout}");
}
