use std::path::Path;

use anyhow::{Context, anyhow};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};

use crate::Result;

const READONLY_SCOPE: &str = "https://www.googleapis.com/auth/calendar.events.readonly";

#[derive(Clone, Debug, Deserialize)]
pub struct AuthorizedUserCredentials {
    client_id: String,
    client_secret: String,
    refresh_token: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    scope: String,
}

#[derive(Serialize)]
struct TokenRequest<'a> {
    client_id: &'a str,
    client_secret: &'a str,
    refresh_token: &'a str,
    grant_type: &'static str,
}

fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

impl AuthorizedUserCredentials {
    pub fn load(path: &Path) -> Result<Self> {
        require_private_file(path)?;
        let bytes = std::fs::read(path)
            .with_context(|| format!("read Google OAuth credentials {}", path.display()))?;
        let credentials: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse Google OAuth credentials {}", path.display()))?;
        credentials.validate_token_uri()?;
        Ok(credentials)
    }

    fn validate_token_uri(&self) -> Result<()> {
        let url = Url::parse(&self.token_uri)
            .map_err(|_| anyhow!("Google OAuth token_uri must be a valid URL"))?;
        if is_official_token_uri(&url) || is_debug_loopback_token_uri(&url) {
            return Ok(());
        }
        Err(anyhow!(
            "Google OAuth token_uri must be https://oauth2.googleapis.com/token (loopback HTTP is allowed only in debug builds)"
        )
        .into())
    }

    pub async fn access_token(&self, client: &Client) -> Result<String> {
        let response = client
            .post(&self.token_uri)
            .form(&TokenRequest {
                client_id: &self.client_id,
                client_secret: &self.client_secret,
                refresh_token: &self.refresh_token,
                grant_type: "refresh_token",
            })
            .send()
            .await?
            .error_for_status()?
            .json::<TokenResponse>()
            .await?;
        if !has_calendar_read_scope(&response.scope) {
            return Err(anyhow!(
                "Google OAuth token must grant only the Calendar events read scope"
            )
            .into());
        }
        Ok(response.access_token)
    }
}

fn is_official_token_uri(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("oauth2.googleapis.com")
        && url.path() == "/token"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.port().is_none()
        && url.as_str() == default_token_uri()
}

fn is_debug_loopback_token_uri(url: &Url) -> bool {
    #[cfg(debug_assertions)]
    {
        url.scheme() == "http"
            && url.path() == "/token"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.host_str().is_some_and(|host| {
                host.parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
            })
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = url;
        false
    }
}

fn has_calendar_read_scope(scopes: &str) -> bool {
    let mut has_required_read_scope = false;
    for scope in scopes.split_ascii_whitespace() {
        match scope {
            READONLY_SCOPE => has_required_read_scope = true,
            "openid" | "email" | "https://www.googleapis.com/auth/userinfo.email" => {}
            _ => return false,
        }
    }
    has_required_read_scope
}

#[cfg(unix)]
fn require_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(anyhow!(
            "Google OAuth credentials {} must not be accessible by group or others",
            path.display()
        )
        .into());
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_private_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
#[path = "credentials_tests.rs"]
mod tests;
