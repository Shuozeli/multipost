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
    /// SSH host where the Chrome runs. Empty means Chrome is local.
    #[serde(default)]
    pub ssh_host: String,
    /// SSH username on `ssh_host`.
    #[serde(default)]
    pub ssh_user: String,
    /// Optional SSH password. If set, staging uses `sshpass`.
    #[serde(default)]
    pub ssh_password: String,
    /// SSH port. `None` means 22.
    #[serde(default)]
    pub ssh_port: Option<u16>,
    /// Directory on `ssh_host` where staged video uploads land.
    #[serde(default)]
    pub remote_temp_dir: Option<String>,
    /// Cached display name (头条号 / 昵称), informational only.
    #[serde(default)]
    pub nickname: String,
    /// Cached Toutiao user ID (头条号 ID), informational only.
    #[serde(default)]
    pub toutiao_id: String,
}

impl ToutiaoCredentials {
    /// Default remote temp directory if the field is unset.
    pub fn effective_remote_temp_dir(&self) -> &str {
        self.remote_temp_dir
            .as_deref()
            .unwrap_or("C:/Users/cyuan/Videos/multipost-uploads")
    }

    /// SSH `user@host` target string, or `None` when Chrome is local.
    pub fn ssh_target(&self) -> Option<String> {
        if self.ssh_host.is_empty() {
            None
        } else if self.ssh_user.is_empty() {
            Some(self.ssh_host.clone())
        } else {
            Some(format!("{}@{}", self.ssh_user, self.ssh_host))
        }
    }
}
