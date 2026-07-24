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
channel_token = "test-channel-token-0123456789abcdef"
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
    #[cfg(unix)]
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600))
        .expect("secure op_pi config fixture");
    config_path
}

pub(crate) fn spawn_daemon(
    config_path: &std::path::Path,
    daemon_port: u16,
) -> (super::DaemonProcess, Arc<Notify>) {
    spawn_daemon_inner(config_path, daemon_port, None)
}

pub(crate) fn spawn_daemon_with_proxy(
    config_path: &std::path::Path,
    proxy_listener: TcpListener,
) -> (super::DaemonProcess, Arc<Notify>) {
    spawn_daemon_inner(config_path, 0, Some(proxy_listener))
}

fn spawn_daemon_inner(
    config_path: &std::path::Path,
    daemon_port: u16,
    proxy_listener: Option<TcpListener>,
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
    let (target_tx, target_rx) = tokio::sync::watch::channel(None::<std::net::SocketAddr>);
    let stdout_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(address) = line
                .split_once(" listening on ")
                .and_then(|(_, suffix)| suffix.split_ascii_whitespace().next())
                .and_then(|url| url.strip_prefix("http://"))
                .and_then(|address| address.parse().ok())
            {
                let _ = target_tx.send(Some(address));
                listening_for_task.notify_one();
                break;
            }
        }
    });
    let proxy_task = proxy_listener.map(|listener| {
        tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                let mut target_rx = target_rx.clone();
                tokio::spawn(async move {
                    let target = loop {
                        if let Some(target) = *target_rx.borrow() {
                            break target;
                        }
                        if target_rx.changed().await.is_err() {
                            return;
                        }
                    };
                    let Ok(mut outbound) = tokio::net::TcpStream::connect(target).await else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                });
            }
        })
    });
    (
        super::DaemonProcess {
            child,
            stdout_task: Some(stdout_task),
            proxy_task,
        },
        listening,
    )
}
