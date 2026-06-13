//! Crawler for the Twitter / X `For you` / `Following` HomeTimeline feed.
//!
//! Captures `https://x.com/i/api/graphql/<hash>/HomeTimeline`
//! responses by driving `pwright network-listen --include-body` while
//! navigating to `https://x.com/home` by default, or
//! `https://x.com/home?f=following` when
//! `MULTIPOST_TWITTER_CRAWL_FEED=following`. Decodes each response
//! into normalized [`DiscoveredItem`]s.
//!
//! Unlike Toutiao, Twitter only fires HomeTimeline on full-page
//! navigation — subsequent scroll mostly hydrates from the already-
//! delivered cursor — so a crawl run typically captures one timeline
//! page (~30–40 tweets).

#![deny(missing_docs)]

mod parser;
mod publisher;

pub use parser::decode_home_timeline;
pub use publisher::TwitterCrawler;
