//! Platform identifiers and capability metadata.

use serde::{Deserialize, Serialize};

/// One of the platforms multipost integrates with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    /// YouTube — official Data API v3.
    YouTube,
    /// WeChat Official Account (公众号) — official API.
    WxGzh,
    /// Twitter / X — browser automation (paid API avoided).
    Twitter,
    /// Douyin (抖音) — browser automation.
    Douyin,
    /// Toutiao (头条号) — browser automation against mp.toutiao.com.
    Toutiao,
}

impl Platform {
    /// Canonical string used in URLs, config files, and CLI flags.
    pub fn as_str(&self) -> &'static str {
        match self {
            Platform::YouTube => "youtube",
            Platform::WxGzh => "wx-gzh",
            Platform::Twitter => "twitter",
            Platform::Douyin => "douyin",
            Platform::Toutiao => "toutiao",
        }
    }

    /// Whether this platform is implemented via the official API (true) or
    /// via browser automation (false).
    pub fn is_api(&self) -> bool {
        match self {
            Platform::YouTube | Platform::WxGzh => true,
            Platform::Twitter | Platform::Douyin | Platform::Toutiao => false,
        }
    }
}

/// Current authentication status for a connected account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStatus {
    /// Account is usable.
    Active,
    /// Tokens or cookies have expired; user must re-auth.
    Expired,
    /// User explicitly revoked, or platform revoked.
    Revoked,
    /// We've never authenticated successfully against this account.
    Pending,
}

/// What a publisher supports for a single piece of content.
///
/// Used by the orchestrator to fail jobs at validation time rather than
/// after we've started uploading bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    /// Maximum text body length in characters. `None` = unlimited.
    pub max_text_chars: Option<usize>,
    /// Maximum images in a single post. `None` = the platform doesn't
    /// support image attachments.
    pub max_images: Option<usize>,
    /// Whether videos are accepted.
    pub video_supported: bool,
    /// Max video duration (seconds). `None` if not platform-limited.
    pub video_max_seconds: Option<u32>,
    /// Whether the publisher supports scheduled posts.
    pub schedule_supported: bool,
    /// Whether posts can be edited in-place after publish.
    pub edit_supported: bool,
    /// Whether posts can be deleted.
    pub delete_supported: bool,
}
