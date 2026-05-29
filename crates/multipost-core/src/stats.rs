//! Types for profile-stats collection — the account-owner counterpart to
//! [`Crawler`](crate::Crawler).
//!
//! A [`StatsCollector`] drives the *logged-in* creator dashboard / profile
//! of a connected account and returns a [`StatsSnapshot`]: account-level
//! totals (followers, income, views) plus per-post metrics for the user's
//! own posts. This is strictly richer than what a [`Crawler`](crate::Crawler) sees on the
//! public recommendation feed — the owner's dashboard exposes impressions
//! (展现), reads (阅读), income, and follower trends that the feed never does.
//!
//! Snapshots are point-in-time: the storage layer timestamps each one so
//! callers can track growth over days.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::platform::Platform;

/// Account-level statistics for one connected account at one point in time.
///
/// All metric fields are `Option` because platforms surface different
/// subsets (Toutiao reports income + reads + plays; Twitter reports
/// followers/following/tweets). `None` means "the platform didn't report
/// this", **not** zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountStats {
    /// Platform this snapshot is for.
    pub platform: Platform,
    /// When the snapshot was taken (server clock).
    pub captured_at: DateTime<Utc>,
    /// Total followers / 粉丝 / subscribers.
    pub followers: Option<i64>,
    /// Accounts this profile follows (Twitter `friends_count`). Toutiao
    /// doesn't expose this.
    pub following: Option<i64>,
    /// Total published posts the platform attributes to this account.
    pub post_count: Option<i64>,
    /// Cumulative reads + plays across all content (Toutiao
    /// `total_read_play_count`). Twitter has no lifetime-impressions field.
    pub total_views: Option<i64>,
    /// Lifetime income, in the platform's currency major unit (Toutiao: 元).
    pub total_income: Option<f64>,
    /// Followers gained yesterday.
    pub yesterday_followers: Option<i64>,
    /// Reads/impressions yesterday.
    pub yesterday_views: Option<i64>,
    /// Income earned yesterday (major currency unit).
    pub yesterday_income: Option<f64>,
    /// Platform-specific extras that don't fit the normalized shape.
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl AccountStats {
    /// An empty snapshot for `platform` at `captured_at` — all metrics
    /// `None`. Collectors fill in the fields the platform reports.
    pub fn new(platform: Platform, captured_at: DateTime<Utc>) -> Self {
        Self {
            platform,
            captured_at,
            followers: None,
            following: None,
            post_count: None,
            total_views: None,
            total_income: None,
            yesterday_followers: None,
            yesterday_views: None,
            yesterday_income: None,
            metadata: HashMap::new(),
        }
    }
}

/// Per-post statistics for one of the account's own posts.
///
/// Metric fields are `Option` for the same reason as [`AccountStats`].
/// `impressions` is the closest cross-platform concept (Toutiao 展现
/// `showCount`, Twitter `views.count`); `reads` is Toutiao-only (阅读 /
/// `readCount` — a click into the detail page).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostStats {
    /// Platform this post belongs to.
    pub platform: Platform,
    /// Platform's stable post identifier (Toutiao `groupID`, Twitter
    /// tweet `rest_id`).
    pub post_id: String,
    /// When this snapshot was taken (server clock).
    pub captured_at: DateTime<Utc>,
    /// Post title / body text (truncated by the collector if huge).
    pub title: String,
    /// Platform-specific post kind, normalized to a short label
    /// (e.g. `微头条`, `文章`, `视频`, `tweet`).
    pub post_type: String,
    /// When the post was originally published, if the platform reported it.
    pub created_at: Option<DateTime<Utc>>,
    /// Impressions / 展现 / views.count.
    pub impressions: Option<i64>,
    /// Reads / 阅读 (Toutiao detail-page opens). Twitter has no equivalent.
    pub reads: Option<i64>,
    /// Likes / 点赞 / favorites.
    pub likes: Option<i64>,
    /// Comments / replies / 评论.
    pub comments: Option<i64>,
    /// Shares / retweets / 转发.
    pub shares: Option<i64>,
    /// Bookmarks / 收藏 / repins.
    pub bookmarks: Option<i64>,
    /// Video plays / 播放 (Toutiao `videoWatchCount`).
    pub plays: Option<i64>,
    /// Platform-specific extras.
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl PostStats {
    /// An empty per-post snapshot — all metrics `None`. Collectors fill
    /// in the fields the platform reports.
    pub fn new(platform: Platform, post_id: String, captured_at: DateTime<Utc>) -> Self {
        Self {
            platform,
            post_id,
            captured_at,
            title: String::new(),
            post_type: String::new(),
            created_at: None,
            impressions: None,
            reads: None,
            likes: None,
            comments: None,
            shares: None,
            bookmarks: None,
            plays: None,
            metadata: HashMap::new(),
        }
    }
}

/// One full stats reading for an account: the account totals plus the
/// per-post breakdown the collector pulled this run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsSnapshot {
    /// Account-level totals.
    pub account: AccountStats,
    /// Per-post stats, most-recent first. Bounded by
    /// [`StatsOptions::max_posts`].
    pub posts: Vec<PostStats>,
}

/// Per-call options for [`StatsCollector::collect`].
#[derive(Debug, Clone)]
pub struct StatsOptions {
    /// Max number of (recent) posts to pull stats for.
    pub max_posts: usize,
    /// Chrome CDP HTTP endpoint for the account's logged-in browser
    /// profile. Required for CDP-driven collectors (Toutiao).
    pub cdp_url: Option<String>,
    /// Account handle without the leading `@` (Twitter). Used to locate
    /// the profile timeline. Ignored by platforms that don't need it.
    pub handle: Option<String>,
    /// Path to the `pwright` binary, for collectors that drive it as a
    /// subprocess (Twitter network capture). Defaults to `pwright` on PATH.
    pub pwright_bin: String,
}

impl Default for StatsOptions {
    fn default() -> Self {
        Self {
            max_posts: 100,
            cdp_url: std::env::var("PWRIGHT_CDP").ok(),
            handle: None,
            pwright_bin: std::env::var("PWRIGHT_BIN").unwrap_or_else(|_| "pwright".to_string()),
        }
    }
}

/// What every platform's profile-stats collector implements.
#[async_trait]
pub trait StatsCollector: Send + Sync + 'static {
    /// Which platform this collector targets.
    fn platform(&self) -> Platform;

    /// Drive the logged-in dashboard/profile and return a snapshot of
    /// account totals + recent per-post stats.
    ///
    /// The returned `captured_at` on both [`AccountStats`] and each
    /// [`PostStats`] is set by the collector to the moment of capture.
    async fn collect(&self, opts: &StatsOptions) -> Result<StatsSnapshot>;
}
