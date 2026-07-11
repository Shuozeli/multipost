//! YouTube discovery crawler.
//!
//! Crawls channel/video listing pages via `pwright` and normalizes the
//! visible video cards into [`multipost_core::DiscoveredItem`]s.

mod publisher;

pub use publisher::YouTubeCrawler;
