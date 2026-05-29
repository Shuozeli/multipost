//! Token + identity helpers for the WeChat MP publisher.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::API_BASE;

/// What we store in `AccountRecord.credentials` for a WxGzh account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WxGzhCredentials {
    /// MP appid (looks like `wxXXXXXXXXXXXXXXXX`).
    pub appid: String,
    /// App secret.
    pub app_secret: String,
    /// Cached `stable_token` access token. Empty until first refresh.
    #[serde(default)]
    pub access_token: String,
    /// When the cached access_token expires (Unix epoch seconds).
    #[serde(default)]
    pub expires_at: i64,
}

#[derive(Debug, Deserialize)]
struct StableTokenResponse {
    access_token: Option<String>,
    expires_in: Option<i64>,
    errcode: Option<i64>,
    errmsg: Option<String>,
}

/// Account-basic-info response shape used during registration.
#[derive(Debug, Deserialize, Serialize)]
pub struct AccountBasicInfo {
    /// MP appid.
    pub appid: String,
    /// Public-facing name of the account (the 公众号 nickname).
    pub nickname: Option<String>,
    /// Real-name principal (verified Chinese name / business name).
    pub principal_name: Option<String>,
    /// 1 = 订阅号 (subscription), 2 = 服务号 (service).
    pub account_type: Option<i64>,
    /// 0 = 个人 (individual), 1 = 企业 (enterprise).
    pub principal_type: Option<i64>,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// If `creds.access_token` is missing or close to expiry, fetch a new one
/// and return the updated `WxGzhCredentials`. Returns `None` if the cache
/// is still valid.
///
/// Margin: 60 seconds. So we refresh at expires_at - 60.
pub async fn ensure_access_token(
    http: &reqwest::Client,
    creds: &WxGzhCredentials,
) -> anyhow::Result<Option<WxGzhCredentials>> {
    if !creds.access_token.is_empty() && creds.expires_at > now_secs() + 60 {
        return Ok(None);
    }
    let resp = http
        .post(format!("{API_BASE}/cgi-bin/stable_token"))
        .json(&serde_json::json!({
            "grant_type": "client_credential",
            "appid": creds.appid,
            "secret": creds.app_secret,
            "force_refresh": false,
        }))
        .send()
        .await
        .context("post stable_token")?;
    let body = resp
        .json::<StableTokenResponse>()
        .await
        .context("parse stable_token response")?;
    if let Some(code) = body.errcode
        && code != 0
    {
        anyhow::bail!(
            "WeChat stable_token errcode={code} errmsg={}",
            body.errmsg.unwrap_or_default()
        );
    }
    let access_token = body
        .access_token
        .ok_or_else(|| anyhow::anyhow!("stable_token response missing access_token"))?;
    let expires_in = body.expires_in.unwrap_or(7200);
    Ok(Some(WxGzhCredentials {
        appid: creds.appid.clone(),
        app_secret: creds.app_secret.clone(),
        access_token,
        // 30-second safety margin.
        expires_at: now_secs() + expires_in - 30,
    }))
}

/// Validate the appid+secret pair by fetching basic account info.
///
/// Used at registration time to confirm the credentials work AND populate
/// the account's `external_id` (= appid) and `display_name` (= nickname).
pub async fn check_account_info(
    http: &reqwest::Client,
    access_token: &str,
) -> anyhow::Result<AccountBasicInfo> {
    let resp = http
        .get(format!("{API_BASE}/cgi-bin/account/getaccountbasicinfo"))
        .query(&[("access_token", access_token)])
        .send()
        .await
        .context("get account/getaccountbasicinfo")?;
    let body: serde_json::Value = resp.json().await.context("parse account info")?;
    if let Some(code) = body.get("errcode").and_then(|v| v.as_i64())
        && code != 0
    {
        anyhow::bail!(
            "WeChat account/getaccountbasicinfo errcode={code} errmsg={}",
            body.get("errmsg").and_then(|v| v.as_str()).unwrap_or("")
        );
    }
    serde_json::from_value(body).context("deserialize account info")
}
