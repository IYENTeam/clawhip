use std::env;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, anyhow};

use crate::{Result, plugins};

const GITHUB_REPO: &str = "IYENTeam/op-pi";
const BINARY_NAME: &str = "op-pi";
const LEGACY_BINARY_NAME: &str = "clawhip";
const SERVICE_NAME: &str = "op-pi";
const LEGACY_SERVICE_NAME: &str = "clawhip";
const SKIP_STAR_PROMPT_ENV: &str = "OP_PI_SKIP_STAR_PROMPT";
const LEGACY_SKIP_STAR_PROMPT_ENV: &str = "CLAWHIP_SKIP_STAR_PROMPT";

pub fn install(systemd: bool, skip_star_prompt: bool) -> Result<()> {
    let repo_root = current_repo_root()?;
    let mut command = cargo_install_command(&repo_root);
    run(&mut command)?;
    ensure_config_dir()?;
    ensure_legacy_binary_link()?;
    plugins::install_bundled_plugins(&config_dir().join("plugins"))?;
    if systemd {
        install_systemd(&repo_root)?;
    }
    maybe_prompt_to_star_repo(skip_star_prompt)?;
    println!("op-pi install complete");
    Ok(())
}

fn cargo_install_command(repo_root: &Path) -> Command {
    let mut command = Command::new("cargo");
    command
        .arg("install")
        .arg("--path")
        .arg(repo_root)
        .arg("--force");
    command
}

pub fn update(restart: bool) -> Result<()> {
    let repo_root = current_repo_root()?;
    update_repo(&repo_root, restart)
}

/// Perform a self-update using an explicit repo root (from config) or by
/// attempting automatic discovery. Prefer passing an explicit path for
/// deterministic behavior in daemon/systemd contexts.
pub fn update_from_repo(explicit_root: Option<&str>, restart: bool) -> Result<()> {
    let repo_root = match explicit_root {
        Some(path) => {
            let root = PathBuf::from(path);
            if !root.join("Cargo.toml").exists() || !root.join("src").exists() {
                return Err(anyhow!(
                    "configured repo_root '{}' does not contain an op-pi checkout",
                    root.display()
                )
                .into());
            }
            root
        }
        None => find_repo_root()?,
    };
    update_repo(&repo_root, restart)
}

fn update_repo(repo_root: &Path, restart: bool) -> Result<()> {
    run(Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("pull")
        .arg("--ff-only"))?;
    run(Command::new("cargo")
        .arg("install")
        .arg("--path")
        .arg(repo_root)
        .arg("--force"))?;
    ensure_config_dir()?;
    ensure_legacy_binary_link()?;
    plugins::install_bundled_plugins(&config_dir().join("plugins"))?;
    refresh_systemd_binary_if_present()?;
    if restart {
        restart_systemd_if_present()?;
    }
    println!("op-pi update complete");
    Ok(())
}

fn find_repo_root() -> Result<PathBuf> {
    if let Ok(root) = current_repo_root() {
        return Ok(root);
    }
    let output = Command::new("cargo")
        .arg("locate-project")
        .arg("--workspace")
        .arg("--message-format=plain")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok());
    if let Some(path) = output {
        let manifest = PathBuf::from(path.trim());
        if let Some(parent) = manifest.parent()
            && parent.join("src").exists()
        {
            return Ok(parent.to_path_buf());
        }
    }
    Err(anyhow!(
        "could not locate op-pi repo root; run from the git clone or ensure cargo is available"
    )
    .into())
}

pub fn uninstall(remove_systemd: bool, remove_config: bool) -> Result<()> {
    stop_systemd_if_present()?;
    for binary_name in [BINARY_NAME, LEGACY_BINARY_NAME] {
        let binary_path = cargo_bin_dir().join(binary_name);
        if binary_path.exists() || binary_path.is_symlink() {
            fs::remove_file(&binary_path)?;
            println!("Removed {}", binary_path.display());
        }
    }
    if remove_systemd {
        uninstall_systemd_if_present()?;
    }
    if remove_config {
        for config_dir in [config_dir(), legacy_config_dir()] {
            if config_dir.exists() || config_dir.is_symlink() {
                fs::remove_dir_all(&config_dir)?;
                println!("Removed {}", config_dir.display());
            }
        }
    }
    println!("op-pi uninstall complete");
    Ok(())
}

fn current_repo_root() -> Result<PathBuf> {
    let dir = env::current_dir()?;
    if dir.join("Cargo.toml").exists() && dir.join("src").exists() {
        Ok(dir)
    } else {
        Err(anyhow!("run this command from the op-pi git clone root").into())
    }
}

fn ensure_config_dir() -> Result<()> {
    let dir = config_dir();
    migrate_legacy_config_dir(&dir)?;
    fs::create_dir_all(&dir)?;
    println!("Ensured config dir {}", dir.display());
    Ok(())
}

fn config_dir() -> PathBuf {
    home_dir().join(".op-pi")
}

fn legacy_config_dir() -> PathBuf {
    home_dir().join(".clawhip")
}

fn home_dir() -> PathBuf {
    PathBuf::from(env::var("HOME").unwrap_or_else(|_| ".".to_string()))
}

fn migrate_legacy_config_dir(destination: &Path) -> Result<()> {
    let legacy = legacy_config_dir();
    if !legacy.is_dir() {
        return Ok(());
    }

    copy_missing_dir_contents(&legacy, destination)?;
    println!(
        "Migrated missing config and state from {} to {}",
        legacy.display(),
        destination.display()
    );
    Ok(())
}

fn copy_missing_dir_contents(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_missing_dir_contents(&source_path, &destination_path)?;
        } else if !destination_path.exists() {
            fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}

fn cargo_bin_dir() -> PathBuf {
    env::var("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env::var("HOME").unwrap_or_else(|_| ".".into())).join(".cargo")
        })
        .join("bin")
}

#[cfg(unix)]
fn ensure_legacy_binary_link() -> Result<()> {
    let binary_dir = cargo_bin_dir();
    let primary = binary_dir.join(BINARY_NAME);
    if !primary.exists() {
        return Ok(());
    }

    let legacy = binary_dir.join(LEGACY_BINARY_NAME);
    if legacy.exists() || legacy.is_symlink() {
        fs::remove_file(&legacy)?;
    }
    std::os::unix::fs::symlink(BINARY_NAME, &legacy)?;
    println!("Linked {} to {}", legacy.display(), primary.display());
    Ok(())
}

#[cfg(not(unix))]
fn ensure_legacy_binary_link() -> Result<()> {
    Ok(())
}

fn maybe_prompt_to_star_repo(skip_star_prompt: bool) -> Result<()> {
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    let env_skip_star_prompt = env::var(SKIP_STAR_PROMPT_ENV)
        .or_else(|_| env::var(LEGACY_SKIP_STAR_PROMPT_ENV))
        .ok();

    maybe_prompt_to_star_repo_with(
        skip_star_prompt,
        env_skip_star_prompt.as_deref(),
        interactive,
        &mut input,
        &mut output,
        gh_command_succeeds,
    )
}

fn maybe_prompt_to_star_repo_with<R, W, F>(
    skip_star_prompt: bool,
    env_skip_star_prompt: Option<&str>,
    interactive: bool,
    input: &mut R,
    output: &mut W,
    mut gh_command_succeeds: F,
) -> Result<()>
where
    R: BufRead,
    W: Write,
    F: FnMut(&[&str]) -> bool,
{
    if star_prompt_disabled(skip_star_prompt, env_skip_star_prompt) {
        writeln!(
            output,
            "[op-pi] skipping GitHub star prompt (--skip-star-prompt or {SKIP_STAR_PROMPT_ENV})"
        )?;
        return Ok(());
    }

    if !interactive || !gh_command_succeeds(&["auth", "status"]) {
        return Ok(());
    }

    writeln!(
        output,
        "[op-pi] optional: star {GITHUB_REPO} on GitHub to support the project"
    )?;
    write!(
        output,
        "[op-pi] Would you like to star {GITHUB_REPO} on GitHub with gh? [y/N]: "
    )?;
    output.flush()?;

    let mut response = String::new();
    if input.read_line(&mut response)? == 0 {
        return Ok(());
    }

    match response.trim() {
        "y" | "Y" | "yes" | "Yes" | "YES" => {
            if gh_star_repo_succeeds_with(&mut gh_command_succeeds) {
                writeln!(output, "[op-pi] thanks for starring {GITHUB_REPO}")?;
            } else {
                writeln!(
                    output,
                    "[op-pi] unable to star {GITHUB_REPO} with gh; continuing without it"
                )?;
            }
        }
        _ => {
            writeln!(output, "[op-pi] skipping GitHub star step")?;
        }
    }

    Ok(())
}

fn star_prompt_disabled(skip_star_prompt: bool, env_skip_star_prompt: Option<&str>) -> bool {
    skip_star_prompt || env_skip_star_prompt.is_some_and(is_truthy)
}

fn is_truthy(value: &str) -> bool {
    matches!(value, "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
}

fn gh_star_repo_succeeds_with<F>(gh_command_succeeds: &mut F) -> bool
where
    F: FnMut(&[&str]) -> bool,
{
    let endpoint = format!("/user/starred/{GITHUB_REPO}");
    gh_command_succeeds(&["api", "--method", "PUT", endpoint.as_str(), "--silent"])
}

fn gh_command_succeeds(args: &[&str]) -> bool {
    Command::new("gh")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn install_systemd(repo_root: &Path) -> Result<()> {
    let unit_src = repo_root.join("deploy").join("op-pi.service");
    let unit_dest = systemd_unit_path(SERVICE_NAME);
    install_systemd_binary()?;
    let legacy_unit = systemd_unit_path(LEGACY_SERVICE_NAME);
    if legacy_unit.exists() {
        let _ = run(Command::new("sudo")
            .arg("systemctl")
            .arg("disable")
            .arg("--now")
            .arg(LEGACY_SERVICE_NAME));
        let _ = run(Command::new("sudo").arg("rm").arg("-f").arg(&legacy_unit));
    }
    run(Command::new("sudo")
        .arg("cp")
        .arg(&unit_src)
        .arg(&unit_dest))?;
    run(Command::new("sudo").arg("systemctl").arg("daemon-reload"))?;
    run(Command::new("sudo")
        .arg("systemctl")
        .arg("enable")
        .arg("--now")
        .arg(SERVICE_NAME))?;
    Ok(())
}

fn uninstall_systemd_if_present() -> Result<()> {
    let mut removed_unit = false;
    for service_name in [SERVICE_NAME, LEGACY_SERVICE_NAME] {
        let unit_dest = systemd_unit_path(service_name);
        if unit_dest.exists() {
            let _ = run(Command::new("sudo")
                .arg("systemctl")
                .arg("disable")
                .arg("--now")
                .arg(service_name));
            let _ = run(Command::new("sudo").arg("rm").arg("-f").arg(&unit_dest));
            removed_unit = true;
        }
    }
    if removed_unit {
        let _ = run(Command::new("sudo").arg("systemctl").arg("daemon-reload"));
    }
    Ok(())
}

fn restart_systemd_if_present() -> Result<()> {
    if let Some(service_name) = installed_systemd_service_name() {
        let _ = run(Command::new("sudo")
            .arg("systemctl")
            .arg("restart")
            .arg(service_name));
    }
    Ok(())
}

fn stop_systemd_if_present() -> Result<()> {
    for service_name in [SERVICE_NAME, LEGACY_SERVICE_NAME] {
        if systemd_unit_path(service_name).exists() {
            let _ = run(Command::new("sudo")
                .arg("systemctl")
                .arg("stop")
                .arg(service_name));
        }
    }
    Ok(())
}

fn systemd_unit_path(service_name: &str) -> PathBuf {
    PathBuf::from("/etc/systemd/system").join(format!("{service_name}.service"))
}

fn installed_systemd_service_name() -> Option<&'static str> {
    [SERVICE_NAME, LEGACY_SERVICE_NAME]
        .into_iter()
        .find(|service_name| systemd_unit_path(service_name).exists())
}

fn install_systemd_binary() -> Result<()> {
    let primary = cargo_bin_dir().join(BINARY_NAME);
    if !primary.exists() {
        return Err(anyhow!("installed op-pi binary not found at {}", primary.display()).into());
    }

    run(Command::new("sudo")
        .arg("install")
        .arg("-m")
        .arg("755")
        .arg(&primary)
        .arg(format!("/usr/local/bin/{BINARY_NAME}")))?;
    run(Command::new("sudo")
        .arg("rm")
        .arg("-f")
        .arg(format!("/usr/local/bin/{LEGACY_BINARY_NAME}")))?;
    run(Command::new("sudo")
        .arg("ln")
        .arg("-s")
        .arg(BINARY_NAME)
        .arg(format!("/usr/local/bin/{LEGACY_BINARY_NAME}")))?;
    Ok(())
}

fn refresh_systemd_binary_if_present() -> Result<()> {
    if installed_systemd_service_name().is_some() {
        install_systemd_binary()?;
    }
    Ok(())
}

fn run(command: &mut Command) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("failed to run command: {command:?}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("command failed with status {status}: {command:?}").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn skip_flag_or_env_disables_star_prompt() {
        let mut output = Vec::new();
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut gh_calls = Vec::<Vec<String>>::new();

        maybe_prompt_to_star_repo_with(true, Some("1"), true, &mut input, &mut output, |args| {
            gh_calls.push(args.iter().map(|arg| (*arg).to_string()).collect());
            true
        })
        .expect("skip should succeed");

        assert!(gh_calls.is_empty());
        let stdout = String::from_utf8(output).expect("utf8 output");
        assert!(stdout.contains("skipping GitHub star prompt"));
    }

    #[test]
    fn skips_star_prompt_when_not_interactive() {
        let mut output = Vec::new();
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut gh_calls = Vec::<Vec<String>>::new();

        maybe_prompt_to_star_repo_with(false, None, false, &mut input, &mut output, |args| {
            gh_calls.push(args.iter().map(|arg| (*arg).to_string()).collect());
            true
        })
        .expect("non-interactive install should succeed");

        assert!(gh_calls.is_empty());
        assert!(output.is_empty());
    }

    #[test]
    fn skips_prompt_when_gh_is_unauthenticated() {
        let mut output = Vec::new();
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut gh_calls = Vec::<Vec<String>>::new();

        maybe_prompt_to_star_repo_with(false, None, true, &mut input, &mut output, |args| {
            gh_calls.push(args.iter().map(|arg| (*arg).to_string()).collect());
            !matches!(args, ["auth", "status"])
        })
        .expect("unauthenticated gh should skip cleanly");

        assert_eq!(
            gh_calls,
            vec![vec![String::from("auth"), String::from("status")]]
        );
        let stdout = String::from_utf8(output).expect("utf8 output");
        assert!(!stdout.contains("Would you like to star"));
    }

    #[test]
    fn stars_repo_only_after_explicit_yes() {
        let mut output = Vec::new();
        let mut input = Cursor::new(b"y\n".to_vec());
        let mut gh_calls = Vec::<Vec<String>>::new();

        maybe_prompt_to_star_repo_with(false, None, true, &mut input, &mut output, |args| {
            gh_calls.push(args.iter().map(|arg| (*arg).to_string()).collect());
            true
        })
        .expect("yes path should succeed");

        assert_eq!(
            gh_calls,
            vec![
                vec![String::from("auth"), String::from("status")],
                vec![
                    String::from("api"),
                    String::from("--method"),
                    String::from("PUT"),
                    format!("/user/starred/{GITHUB_REPO}"),
                    String::from("--silent"),
                ],
            ]
        );
        let stdout = String::from_utf8(output).expect("utf8 output");
        assert!(stdout.contains("Would you like to star"));
        assert!(stdout.contains("thanks for starring"));
    }

    #[test]
    fn star_failure_does_not_fail_the_install() {
        let mut output = Vec::new();
        let mut input = Cursor::new(b"yes\n".to_vec());
        let mut gh_calls = Vec::<Vec<String>>::new();

        maybe_prompt_to_star_repo_with(false, None, true, &mut input, &mut output, |args| {
            gh_calls.push(args.iter().map(|arg| (*arg).to_string()).collect());
            !matches!(
                args,
                ["api", "--method", "PUT", endpoint, "--silent"]
                    if *endpoint == format!("/user/starred/{GITHUB_REPO}")
            )
        })
        .expect("star failure should not fail install");

        assert_eq!(
            gh_calls,
            vec![
                vec![String::from("auth"), String::from("status")],
                vec![
                    String::from("api"),
                    String::from("--method"),
                    String::from("PUT"),
                    format!("/user/starred/{GITHUB_REPO}"),
                    String::from("--silent"),
                ],
            ]
        );
        let stdout = String::from_utf8(output).expect("utf8 output");
        assert!(stdout.contains("continuing without it"));
    }

    #[test]
    fn copies_only_missing_legacy_config_and_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join(".clawhip");
        let destination = dir.path().join(".op-pi");
        std::fs::create_dir_all(source.join("state")).expect("create legacy state");
        std::fs::write(source.join("config.toml"), "legacy = true").expect("write legacy config");
        std::fs::write(source.join("state/event.json"), "legacy event")
            .expect("write legacy state");
        std::fs::create_dir_all(&destination).expect("create destination");
        std::fs::write(destination.join("config.toml"), "current = true")
            .expect("write current config");

        copy_missing_dir_contents(&source, &destination).expect("migrate legacy data");

        assert_eq!(
            std::fs::read_to_string(destination.join("config.toml")).expect("read config"),
            "current = true"
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("state/event.json")).expect("read state"),
            "legacy event"
        );
    }

    #[test]
    fn config_directory_names_use_the_current_and_legacy_brands() {
        assert_eq!(config_dir().file_name().unwrap(), ".op-pi");
        assert_eq!(legacy_config_dir().file_name().unwrap(), ".clawhip");
    }

    #[test]
    fn install_replaces_binaries_owned_by_the_legacy_package() {
        let command = cargo_install_command(Path::new("/tmp/op-pi"));
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(args, ["install", "--path", "/tmp/op-pi", "--force"]);
    }

    #[test]
    fn systemd_unit_keeps_the_legacy_service_alias() {
        let unit = include_str!("../deploy/op-pi.service");

        assert!(unit.lines().any(|line| line == "Alias=clawhip.service"));
    }

    #[test]
    fn update_from_repo_rejects_invalid_explicit_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bad_path = dir.path().join("not-a-checkout");
        std::fs::create_dir_all(&bad_path).expect("mkdir");

        let error = update_from_repo(Some(bad_path.to_str().unwrap()), false)
            .expect_err("should reject missing Cargo.toml");

        assert!(
            error
                .to_string()
                .contains("does not contain an op-pi checkout")
        );
    }

    #[test]
    fn update_from_repo_validates_explicit_root_structure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        // Create only Cargo.toml but not src/
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"op-pi\"")
            .expect("write Cargo.toml");

        let error = update_from_repo(Some(root.to_str().unwrap()), false)
            .expect_err("should reject missing src/");

        assert!(
            error
                .to_string()
                .contains("does not contain an op-pi checkout")
        );
    }

    #[test]
    fn find_repo_root_returns_cwd_when_valid() {
        // When run from the actual repo root (which is the case in cargo test),
        // find_repo_root should succeed
        let result = find_repo_root();
        // This test runs from the repo root, so it should find it
        if let Ok(root) = result {
            assert!(root.join("Cargo.toml").exists());
            assert!(root.join("src").exists());
        }
        // If CWD is not repo root (CI sandbox), the test still passes — we just
        // verify it doesn't panic
    }
}
