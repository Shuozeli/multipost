//! Types for **user-content** crawling — fetching the recent posts of a
//! *particular* account, as opposed to the anonymous recommendation feed
//! ([`Crawler`](crate::Crawler)) or the owner's own dashboard
//! ([`StatsCollector`](crate::StatsCollector)).
//!
//! A [`UserCrawler`] navigates to one user's public profile (Twitter
//! `x.com/<handle>`, Toutiao `/c/user/token/<token>/`), scrolls to
//! accumulate the user's timeline, and decodes each captured feed
//! response into normalized [`DiscoveredItem`]s — the same shape the
//! recommendation [`Crawler`](crate::Crawler) emits, so both feed
//! discoveries and per-user discoveries share storage and analytics.
//!
//! Like the feed crawler it drives a real browser via `pwright
//! network-listen --include-body`; unlike the feed crawler it is bounded
//! by a **post count** (scroll until `max_posts` or no more pages),
//! falling back to `max_duration_secs` as a safety stop.

use async_trait::async_trait;

use crate::discovery::DiscoveredItem;
use crate::error::Result;
use crate::platform::Platform;

/// What every per-user crawler implements.
///
/// Implementations navigate to a single account's public profile,
/// capture its timeline feed responses, and decode them into
/// [`DiscoveredItem`]s. The items carry the same `(platform, item_id)`
/// key as feed-crawled items, so callers de-dup and store them the same
/// way.
#[async_trait]
pub trait UserCrawler: Send + Sync + 'static {
    /// Which platform this crawler targets.
    fn platform(&self) -> Platform;

    /// Crawl one user's recent posts.
    ///
    /// The crawler navigates to the profile identified by
    /// [`UserCrawlOptions::handle`], scrolls to trigger the platform's
    /// paginated timeline XHR, and returns up to
    /// [`UserCrawlOptions::max_posts`] decoded items (newest first as the
    /// platform orders them). Re-running on the same profile returns the
    /// same `(platform, item_id)` keys — callers de-dup by that.
    async fn crawl_user(&self, opts: &UserCrawlOptions) -> Result<Vec<DiscoveredItem>>;
}

/// Per-call options for [`UserCrawler::crawl_user`].
#[derive(Debug, Clone)]
pub struct UserCrawlOptions {
    /// The account to crawl. Platform-specific:
    /// - **Twitter** — the screen name (e.g. `Tesla`, no leading `@`).
    /// - **Toutiao** — the user token from the profile URL
    ///   (`MS4wLj…`), since Toutiao has no friendly handle.
    pub handle: String,
    /// Target number of posts to accumulate. The crawler scrolls /
    /// paginates until it reaches this many (de-duped) items or runs out
    /// of pages. Defaults to 100.
    pub max_posts: usize,
    /// Hard upper bound on how long to scroll, in seconds — a safety
    /// stop so a slow/endless timeline can't hang the crawl. Defaults to
    /// 120.
    pub max_duration_secs: u64,
    /// Path to the `pwright` binary. Defaults to `$PWRIGHT_BIN` or
    /// `pwright` on `PATH`.
    pub pwright_bin: String,
    /// Chrome CDP HTTP endpoint (passed to pwright as `PWRIGHT_CDP`).
    /// `None` lets pwright use its own default / saved state.
    pub cdp_url: Option<String>,
}

impl UserCrawlOptions {
    /// Construct options for `handle` with the default depth/limits,
    /// reading `pwright` bin + CDP url from the environment.
    pub fn for_handle(handle: impl Into<String>) -> Self {
        Self {
            handle: handle.into(),
            ..Self::default()
        }
    }
}

impl Default for UserCrawlOptions {
    fn default() -> Self {
        Self {
            handle: String::new(),
            max_posts: 100,
            max_duration_secs: 120,
            pwright_bin: std::env::var("PWRIGHT_BIN").unwrap_or_else(|_| "pwright".to_string()),
            cdp_url: std::env::var("PWRIGHT_CDP").ok(),
        }
    }
}
