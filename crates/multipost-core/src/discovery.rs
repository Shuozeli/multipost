//! Types for content discovery (crawling) — the read-only counterpart
//! to the [`Publisher`](crate::Publisher) trait.
//!
//! A [`Crawler`] observes the recommendation feed of a platform (Toutiao
//! 推荐, Twitter For You, etc.) and emits normalized [`DiscoveredItem`]s
//! the rest of the system can store and analyze. Unlike `Publisher`,
//! crawling is bounded by a duration rather than a content payload —
//! the crawler watches network traffic for `duration` and stops.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::platform::Platform;

/// Engagement metrics extracted from the platform payload.
///
/// All fields are `Option` because platforms surface different subsets
/// (e.g. Toutiao has no `views` field; Twitter has no `repin`/`share`
/// equivalent for native tweets in the same shape). `None` means "the
/// platform didn't report this metric", **not** "the value is zero".
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryMetrics {
    /// Number of reads / impressions.
    pub read_count: Option<i64>,
    /// Number of likes / favorites / 点赞.
    pub like_count: Option<i64>,
    /// Number of comments / replies.
    pub comment_count: Option<i64>,
    /// Number of shares / retweets / 转发.
    pub share_count: Option<i64>,
    /// Number of "view" plays (Twitter `views.count`, distinct from
    /// `read_count` which is impressions on Toutiao).
    pub view_count: Option<i64>,
    /// Number of bookmarks / 收藏 / repins.
    pub bookmark_count: Option<i64>,
}

/// One item observed in a platform's recommendation feed.
///
/// The `metadata` map is the escape hatch for platform-specific fields
/// that don't fit the normalized shape (e.g. Toutiao `cell_type`,
/// Twitter `lang`, media URLs). Crawlers should only put data in
/// `metadata` that's useful to downstream analytics; do **not** put the
/// entire raw response there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveredItem {
    /// Source platform.
    pub platform: Platform,
    /// Platform's stable identifier for this item (e.g. Toutiao
    /// `item_id`, Twitter tweet `rest_id`). Unique within `platform`.
    pub item_id: String,
    /// When this item was captured (server clock at network event time).
    pub captured_at: DateTime<Utc>,
    /// Author handle (e.g. `@user`, source media name). May be empty
    /// for platform-promoted items (Toutiao 微头条 has no `source`).
    pub author_handle: String,
    /// Display name of the author, if distinct from the handle.
    pub author_name: Option<String>,
    /// Main text body — article title for Toutiao, full tweet text for
    /// Twitter. Always present (empty string for media-only items).
    pub body: String,
    /// Canonical URL to view this item, if the platform exposed one.
    pub url: Option<String>,
    /// Engagement metrics. Missing fields are `None`.
    pub metrics: DiscoveryMetrics,
    /// Platform-specific extras the consumer may find useful.
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

/// What every platform crawler implements.
///
/// Implementations are expected to drive a real browser (via `pwright
/// network-listen --include-body`) and decode the platform's feed JSON
/// into [`DiscoveredItem`]s. Errors classify themselves via
/// [`PublishError`](crate::PublishError) — auth-required, network, etc.
#[async_trait]
pub trait Crawler: Send + Sync + 'static {
    /// Which platform this crawler targets.
    fn platform(&self) -> Platform;

    /// Run one crawl cycle: drive the browser, capture feed responses,
    /// and return the items decoded so far.
    ///
    /// `duration` bounds how long the crawler will listen + scroll.
    /// The crawler is responsible for: launching pwright, navigating to
    /// the feed URL, scrolling to trigger XHR, parsing JSONL, decoding
    /// platform JSON, and returning the normalized items.
    ///
    /// Idempotency: re-running on the same content returns items with
    /// the same `(platform, item_id)` — callers de-dup by that key.
    async fn run(&self, opts: &CrawlOptions) -> Result<Vec<DiscoveredItem>>;
}

/// Per-call options for [`Crawler::run`].
#[derive(Debug, Clone)]
pub struct CrawlOptions {
    /// How long to listen + scroll, in seconds.
    pub duration_secs: u64,
    /// Path to the `pwright` binary. Defaults to `pwright` (looked up
    /// on `PATH`) when constructed via `Default`.
    pub pwright_bin: String,
    /// Chrome CDP HTTP endpoint (passed to pwright as `PWRIGHT_CDP`).
    /// If `None`, pwright uses its own default / state.
    pub cdp_url: Option<String>,
}

impl Default for CrawlOptions {
    fn default() -> Self {
        Self {
            duration_secs: 30,
            pwright_bin: std::env::var("PWRIGHT_BIN").unwrap_or_else(|_| "pwright".to_string()),
            cdp_url: std::env::var("PWRIGHT_CDP").ok(),
        }
    }
}
