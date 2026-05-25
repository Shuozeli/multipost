//! Crawler for the Toutiao 推荐 feed.
//!
//! Captures `https://www.toutiao.com/api/pc/list/feed?...` XHR
//! responses by driving `pwright network-listen --include-body` as a
//! subprocess, then decodes each captured response into normalized
//! [`DiscoveredItem`]s.
//!
//! See the parent design doc in `docs/architecture.md` (Phase 6 —
//! Crawling). The pwright network-listen surface is documented in
//! `pwright/docs/knowledge/network-capture-design.md`.

#![deny(missing_docs)]

mod parser;
mod publisher;

pub use parser::decode_feed_response;
pub use publisher::ToutiaoCrawler;
