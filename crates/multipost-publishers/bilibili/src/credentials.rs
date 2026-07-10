//! Per-account credentials for the Bilibili publisher.
//!
//! Authentication lives in the Chrome profile's cookies — we extract
//! `SESSDATA`, `bili_jct`, `buvid3`, and `DedeUserID` via the CDP
//! `Network.getCookies` call at registration time and cache them.
//! The `cdp_url` is retained so `check_auth` can re-extract fresh
//! cookies when needed.

use serde::{Deserialize, Serialize};

/// What multipost stores for a Bilibili account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BilibiliCredentials {
    /// Chrome DevTools Protocol HTTP endpoint, e.g.
    /// `http://chrome-host:9222`. Must point at a Chrome that's already
    /// logged into `bilibili.com`.
    pub cdp_url: String,
    /// Cached display name (B站昵称), informational only.
    #[serde(default)]
    pub nickname: String,
    /// Cached Bilibili user ID (mid), informational only.
    #[serde(default)]
    pub bilibili_uid: String,
    /// `SESSDATA` cookie value — primary auth token.
    #[serde(default)]
    pub sessdata: String,
    /// `bili_jct` cookie value — CSRF token.
    #[serde(default)]
    pub bili_jct: String,
    /// `buvid3` cookie value — device fingerprint.
    #[serde(default)]
    pub buvid3: String,
    /// `DedeUserID` cookie value.
    #[serde(default)]
    pub dedeuserid: String,
}

impl BilibiliCredentials {
    /// Build the `Cookie` header value from cached cookie fields.
    pub fn cookie_header(&self) -> String {
        format!(
            "SESSDATA={}; bili_jct={}; buvid3={}; DedeUserID={}",
            self.sessdata, self.bili_jct, self.buvid3, self.dedeuserid
        )
    }

    /// Whether we have the essential cookies for API calls.
    pub fn has_cookies(&self) -> bool {
        !self.sessdata.is_empty() && !self.bili_jct.is_empty()
    }
}
