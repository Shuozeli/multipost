//! Per-account credentials for the Twitter publisher.
//!
//! Twitter auth lives in the Chrome profile's cookies (we don't store
//! tokens). The handle is also persisted so `delete()` can navigate
//! directly to `https://x.com/<handle>` to find the tweet.

use serde::{Deserialize, Serialize};

/// What multipost stores for a Twitter / X account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwitterCredentials {
    /// Chrome DevTools Protocol HTTP endpoint, e.g. `http://chrome-host:9222`.
    /// Must point at a Chrome that's already logged into x.com.
    pub cdp_url: String,
    /// Twitter handle without the leading `@`, e.g. `multipost_dev`.
    /// Used by `delete()` to navigate to the profile and locate posts.
    pub handle: String,
    /// Cached display name (informational only).
    #[serde(default)]
    pub display_name: String,
}
