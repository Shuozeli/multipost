//! Per-account credentials for the Toutiao publisher.
//!
//! Like Douyin, Toutiao authentication lives in the Chrome profile's
//! cookies — we don't store any tokens. The only thing we persist is
//! how to reach the Chrome that owns the logged-in `mp.toutiao.com`
//! session.

use serde::{Deserialize, Serialize};

/// What multipost stores for a Toutiao account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToutiaoCredentials {
    /// Chrome DevTools Protocol HTTP endpoint, e.g.
    /// `http://chrome-host:9222`. Must point at a Chrome that's already
    /// logged into `mp.toutiao.com`.
    pub cdp_url: String,
    /// Cached display name (头条号 / 昵称), informational only.
    #[serde(default)]
    pub nickname: String,
    /// Cached Toutiao user ID (头条号 ID), informational only.
    #[serde(default)]
    pub toutiao_id: String,
}
