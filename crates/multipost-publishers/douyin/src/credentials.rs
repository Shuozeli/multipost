//! Per-account credentials stored in `AccountRecord.credentials`.

use serde::{Deserialize, Serialize};

/// What we store for a Douyin account.
///
/// Unlike YouTube (OAuth tokens) or WxGzh (appid + secret), Douyin
/// authentication lives entirely inside the Chrome profile's cookies,
/// and uploads need a file on the Chrome host's filesystem. We store:
///
///   - `cdp_url`: HTTP endpoint of the Chrome's `--remote-debugging-port`
///   - `ssh_host` + `ssh_user`: where to SCP the video file to so that
///     `DOM.setFileInputFiles` (which takes a path on the Chrome host)
///     can find it. Only required for cross-host Chromes; for a local
///     Chrome you can leave `ssh_host` empty.
///   - `douyin_id` + `nickname`: cached identity for display + drift check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DouyinCredentials {
    /// Chrome DevTools Protocol HTTP endpoint.
    pub cdp_url: String,
    /// SSH host where the Chrome runs (e.g. `chrome-host` on your
    /// network). Empty means Chrome is local to the multipost-server.
    #[serde(default)]
    pub ssh_host: String,
    /// SSH username for `ssh_host`. Defaults to current user if empty.
    #[serde(default)]
    pub ssh_user: String,
    /// SSH port. `None` → 22.
    #[serde(default)]
    pub ssh_port: Option<u16>,
    /// Directory on `ssh_host` where staged uploads land. `None` →
    /// `/tmp/multipost-uploads`.
    #[serde(default)]
    pub remote_temp_dir: Option<String>,
    /// Douyin user ID (抖音号), captured at registration. Used to detect
    /// silent account switches in the browser session.
    #[serde(default)]
    pub douyin_id: String,
    /// Cached display name (账号 / 昵称).
    #[serde(default)]
    pub nickname: String,
}

impl DouyinCredentials {
    /// Default remote temp directory if the field is unset.
    pub fn effective_remote_temp_dir(&self) -> &str {
        self.remote_temp_dir
            .as_deref()
            .unwrap_or("/tmp/multipost-uploads")
    }

    /// SSH `user@host` target string. Returns `None` when `ssh_host` is empty
    /// (i.e. Chrome runs locally, no SCP needed).
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
