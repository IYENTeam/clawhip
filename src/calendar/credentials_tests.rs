use super::{AuthorizedUserCredentials, READONLY_SCOPE, has_calendar_read_scope};

#[test]
fn requires_readonly_calendar_scope_and_allows_harmless_identity_scopes() {
    for scopes in [
        READONLY_SCOPE.to_string(),
        format!("  {READONLY_SCOPE}\t\n"),
        format!("{READONLY_SCOPE} {READONLY_SCOPE}"),
        format!("openid email https://www.googleapis.com/auth/userinfo.email {READONLY_SCOPE}"),
    ] {
        assert!(
            has_calendar_read_scope(&scopes),
            "required scope set should be accepted: {scopes:?}"
        );
    }
}

#[test]
fn rejects_missing_or_non_readonly_calendar_scopes() {
    for scopes in [
        "",
        " \t\n",
        "openid",
        "https://www.googleapis.com/auth/cloud-platform",
        &format!("{READONLY_SCOPE} https://www.googleapis.com/auth/cloud-platform"),
        &format!("{READONLY_SCOPE} https://www.googleapis.com/auth/drive"),
        &format!("{READONLY_SCOPE} https://mail.google.com/"),
        &format!("{READONLY_SCOPE} profile"),
        "https://www.googleapis.com/auth/calendar.readonly",
        "https://www.googleapis.com/auth/calendar.events",
        "https://www.googleapis.com/auth/calendar",
        "https://www.googleapis.com/auth/calendar.events.readonly-other",
        &format!("{READONLY_SCOPE} https://www.googleapis.com/auth/calendar.events"),
        &format!("openid {READONLY_SCOPE} https://www.googleapis.com/auth/calendar.readonly"),
    ] {
        assert!(
            !has_calendar_read_scope(scopes),
            "missing or over-broad Calendar scope should be rejected: {scopes:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn rejects_oauth_credentials_readable_by_group_or_others() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("temporary credential directory");
    let path = temp.path().join("oauth.json");
    std::fs::write(
        &path,
        r#"{
  "client_id": "client",
  "client_secret": "secret",
  "refresh_token": "refresh",
  "token_uri": "https://oauth2.googleapis.com/token"
}"#,
    )
    .expect("write credential fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("set unsafe credential permissions");

    let error = AuthorizedUserCredentials::load(&path)
        .expect_err("group-readable OAuth credentials must fail");
    assert!(error.to_string().contains("group or others"));
}

#[cfg(unix)]
#[test]
fn rejects_unsafe_oauth_token_uris() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("temporary credential directory");
    let path = temp.path().join("oauth.json");
    for token_uri in [
        "http://oauth2.googleapis.com/token",
        "https://user@oauth2.googleapis.com/token",
        "https://oauth2.googleapis.com/token?redirect=attacker",
        "https://oauth2.googleapis.com/token#fragment",
        "https://attacker.example/token",
    ] {
        std::fs::write(
            &path,
            format!(
                r#"{{
  "client_id": "client",
  "client_secret": "secret",
  "refresh_token": "refresh",
  "token_uri": "{token_uri}"
}}"#,
            ),
        )
        .expect("write credential fixture");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("set private credential permissions");

        let error =
            AuthorizedUserCredentials::load(&path).expect_err("unsafe OAuth token URI must fail");
        assert!(error.to_string().contains("token_uri"));
    }
}

#[cfg(all(unix, debug_assertions))]
#[test]
fn allows_loopback_oauth_token_uri_only_in_debug_builds() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("temporary credential directory");
    let path = temp.path().join("oauth.json");
    std::fs::write(
        &path,
        r#"{
  "client_id": "client",
  "client_secret": "secret",
  "refresh_token": "refresh",
  "token_uri": "http://127.0.0.1:3000/token"
}"#,
    )
    .expect("write credential fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("set private credential permissions");

    AuthorizedUserCredentials::load(&path)
        .expect("debug builds may use loopback OAuth token endpoints");
}

#[cfg(all(unix, not(debug_assertions)))]
#[test]
fn rejects_loopback_oauth_token_uri_in_release_builds() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("temporary credential directory");
    let path = temp.path().join("oauth.json");
    std::fs::write(
        &path,
        r#"{
  "client_id": "client",
  "client_secret": "secret",
  "refresh_token": "refresh",
  "token_uri": "http://127.0.0.1:3000/token"
}"#,
    )
    .expect("write credential fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("set private credential permissions");

    let error = AuthorizedUserCredentials::load(&path)
        .expect_err("release builds must reject loopback OAuth token endpoints");
    assert!(error.to_string().contains("token_uri"));
}

#[cfg(unix)]
#[test]
fn loads_gcloud_authorized_user_credentials_without_token_uri() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("temporary credential directory");
    let path = temp.path().join("application_default_credentials.json");
    std::fs::write(
        &path,
        r#"{
  "type": "authorized_user",
  "client_id": "client",
  "client_secret": "secret",
  "refresh_token": "refresh",
  "account": "victor@example.com",
  "universe_domain": "googleapis.com"
}"#,
    )
    .expect("write gcloud ADC fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("secure gcloud ADC fixture");

    let credentials =
        AuthorizedUserCredentials::load(&path).expect("gcloud ADC credentials should load");
    assert_eq!(credentials.token_uri, "https://oauth2.googleapis.com/token");
}
