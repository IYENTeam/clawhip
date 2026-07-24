use super::*;

pub(crate) fn write_calendar_config(
    temp: &TempDir,
    api_addr: std::net::SocketAddr,
    daemon_port: u16,
    renewal_margin_secs: u64,
) -> std::path::PathBuf {
    let credentials_path = temp.path().join("oauth.json");
    std::fs::write(
        &credentials_path,
        format!(
            r#"{{
  "type": "authorized_user",
  "client_id": "test-client",
  "client_secret": "test-secret",
  "refresh_token": "test-refresh",
  "token_uri": "http://{api_addr}/token"
}}"#
        ),
    )
    .expect("write OAuth fixture");
    #[cfg(unix)]
    std::fs::set_permissions(&credentials_path, std::fs::Permissions::from_mode(0o600))
        .expect("secure OAuth fixture permissions");

    let config_path = temp.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[daemon]
bind_host = "127.0.0.1"
port = {daemon_port}
base_url = "http://127.0.0.1:{daemon_port}"

[google_calendar]
channel_token = "test-channel-token"
credentials_file = "{}"
state_file = "{}"
calendar_id = "primary"
api_base_url = "http://{api_addr}/calendar/v3"
callback_url = "https://calendar.example.test/google/calendar"
renewal_margin_secs = {renewal_margin_secs}

[[routes]]
event = "google.calendar.*"
sink = "localfile"
local_path = "{}"

[[routes]]
event = "calendar.*"
webhook = "http://{api_addr}/sink"
format = "compact"
"#,
            credentials_path.display(),
            temp.path().join("calendar-state.json").display(),
            temp.path().join("calendar-events.jsonl").display(),
        ),
    )
    .expect("write op_pi config");
    config_path
}

pub(crate) fn spawn_daemon(
    config_path: &std::path::Path,
    daemon_port: u16,
) -> (super::DaemonProcess, Arc<Notify>) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_op_pi"));
    command
        .arg("--config")
        .arg(config_path)
        .args(["start", "--port", &daemon_port.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().expect("start configured op_pi daemon");
    let stdout = child.stdout.take().expect("daemon stdout");
    let listening = Arc::new(Notify::new());
    let listening_for_task = listening.clone();
    let stdout_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.contains(" listening on ") {
                listening_for_task.notify_one();
                break;
            }
        }
    });
    (super::DaemonProcess { child, stdout_task }, listening)
}
