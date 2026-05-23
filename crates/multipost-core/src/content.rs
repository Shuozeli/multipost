//! The unified `Content` model.
//!
//! Same shape across platforms; publisher impls translate fields onto the
//! platform's specific request shape and may use `overrides` to substitute
//! per-platform text/media.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::platform::Platform;

/// What kind of post this is.
///
/// Used by the orchestrator to pick the right code path inside a publisher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    /// Text-only post (e.g. a tweet without media).
    Text,
    /// One image with a caption.
    Image,
    /// Multiple images / image carousel.
    Carousel,
    /// Short video (typically vertical, <60s).
    ShortVideo,
    /// Long-form video (e.g. YouTube upload).
    LongVideo,
    /// Long-form article (WeChat MP, etc.).
    Article,
}

/// Reference to a media asset by ID. The orchestrator resolves these
/// to bytes via the storage layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MediaRef(pub Uuid);

/// Who can see the post once it's published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Visible to everyone (or to the platform's "public" default).
    Public,
    /// Visible to followers only (or platform equivalent).
    Followers,
    /// Visible only via direct URL (YouTube "unlisted").
    Unlisted,
    /// Private — only the author / authenticated account.
    Private,
}

/// Per-platform overrides for a single piece of content.
///
/// Posting "the same thing" everywhere often requires per-platform tweaks
/// (e.g. shorter text on Twitter, different hashtags on Douyin vs YouTube).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlatformOverride {
    /// Override text for this platform.
    pub text: Option<String>,
    /// Override hashtags.
    pub hashtags: Option<Vec<String>>,
    /// Override thumbnail (e.g. YouTube custom thumbnail).
    pub thumbnail: Option<MediaRef>,
    /// Escape hatch for platform-specific JSON.
    pub raw: Option<serde_json::Value>,
}

/// One piece of content destined for one or more platforms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Content {
    /// Stable ID for this content.
    pub id: Uuid,
    /// Owning user (multi-tenant scope).
    pub user_id: Uuid,
    /// Kind of post.
    pub kind: ContentKind,
    /// Universal caption / body.
    pub text: String,
    /// Hashtags as bare strings (no `#`).
    pub hashtags: Vec<String>,
    /// Media references attached to this post.
    pub media: Vec<MediaRef>,
    /// When to publish. `None` = publish immediately.
    pub schedule_at: Option<DateTime<Utc>>,
    /// Default visibility for platforms that support it.
    pub visibility: Visibility,
    /// Per-platform overrides.
    pub overrides: HashMap<Platform, PlatformOverride>,
    /// When the content record was created.
    pub created_at: DateTime<Utc>,
}
