//! OAuth 2.0 helpers for the YouTube publisher.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{AUTH_URL, SCOPES, TOKEN_URL};

/// OAuth client credentials, loaded from config or env.
///
/// These come from a Google Cloud project's "OAuth 2.0 Client" JSON.
#[derive(Debug, Clone)]
pub struct OAuthCredentials {
    /// Client ID (e.g. `<project-number>-<random>.apps.googleusercontent.com`).
    pub client_id: String,
    /// Client secret.
    pub client_secret: String,
    /// Redirect URI matching what's registered in the Google Cloud Console.
    pub redirect_uri: String,
}

/// Result of a successful OAuth exchange or refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    /// Short-lived access token (typically 1 hour).
    pub access_token: String,
    /// Long-lived refresh token (returned only on initial exchange with
    /// `prompt=consent`). May be absent on a refresh response.
    pub refresh_token: Option<String>,
    /// Unix epoch seconds when `access_token` expires. Computed at exchange
    /// time so callers don't have to.
    pub expires_at: i64,
    /// Granted scopes (space-separated).
    pub scope: String,
}

/// Raw response shape from Google's token endpoint. Internal helper.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: i64,
    scope: String,
}

/// Build the authorization URL the user opens in a browser.
///
/// Mirrors `scripts/youtube/05_oauth_login.py`. Returns a `Url` containing
/// the same query parameters.
pub fn start_oauth_url(creds: &OAuthCredentials, state: &str) -> anyhow::Result<Url> {
    let mut url = Url::parse(AUTH_URL).context("parse AUTH_URL")?;
    url.query_pairs_mut()
        .append_pair("client_id", &creds.client_id)
        .append_pair("redirect_uri", &creds.redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", &SCOPES.join(" "))
        .append_pair("state", state)
        .append_pair("access_type", "offline")
        // Force consent so we get a refresh_token even on re-auth.
        .append_pair("prompt", "consent");
    Ok(url)
}

/// Exchange an authorization code for access + refresh tokens.
pub async fn exchange_code(
    http: &reqwest::Client,
    creds: &OAuthCredentials,
    code: &str,
) -> anyhow::Result<OAuthTokens> {
    let resp = http
        .post(TOKEN_URL)
        .form(&[
            ("code", code),
            ("client_id", &creds.client_id),
            ("client_secret", &creds.client_secret),
            ("redirect_uri", &creds.redirect_uri),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
        .context("post to token endpoint")?;
    let status = resp.status();
    let body = resp.text().await.context("read token-response body")?;
    if !status.is_success() {
        anyhow::bail!("token exchange failed: HTTP {status} body={body}");
    }
    let token: TokenResponse =
        serde_json::from_str(&body).with_context(|| format!("parse token JSON: {body}"))?;
    Ok(into_tokens(token))
}

/// Refresh an access token using a stored refresh token.
///
/// The response from Google does NOT include a refresh_token; callers
/// should preserve their existing one (we paste it back into the returned
/// `OAuthTokens` automatically here).
pub async fn refresh_token(
    http: &reqwest::Client,
    creds: &OAuthCredentials,
    refresh: &str,
) -> anyhow::Result<OAuthTokens> {
    let resp = http
        .post(TOKEN_URL)
        .form(&[
            ("refresh_token", refresh),
            ("client_id", &creds.client_id),
            ("client_secret", &creds.client_secret),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await
        .context("post to token refresh endpoint")?;
    let status = resp.status();
    let body = resp.text().await.context("read refresh-response body")?;
    if !status.is_success() {
        anyhow::bail!("token refresh failed: HTTP {status} body={body}");
    }
    let token: TokenResponse =
        serde_json::from_str(&body).with_context(|| format!("parse refresh JSON: {body}"))?;
    let mut out = into_tokens(token);
    if out.refresh_token.is_none() {
        out.refresh_token = Some(refresh.to_string());
    }
    Ok(out)
}

fn into_tokens(t: TokenResponse) -> OAuthTokens {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    OAuthTokens {
        access_token: t.access_token,
        refresh_token: t.refresh_token,
        // Subtract 30 to give ourselves a safety margin before expiry.
        expires_at: now + t.expires_in - 30,
        scope: t.scope,
    }
}
