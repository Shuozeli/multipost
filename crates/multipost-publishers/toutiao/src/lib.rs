//! Toutiao (头条号) publisher.
//!
//! Drives Toutiao's creator center (`mp.toutiao.com`) via Chrome
//! DevTools Protocol. Like the Douyin publisher, each Toutiao account
//! corresponds to a Chrome profile that's pre-logged-in to the platform;
//! credentials just point at the CDP endpoint.
//!
//! Discovery work in `scripts/toutiao/` mapped the publish editor.
//! Selectors live in [`selectors`].

#![deny(missing_docs)]

pub mod cdp;
pub mod credentials;
pub mod publisher;
pub mod selectors;
mod staging;
pub mod stats;

pub use credentials::ToutiaoCredentials;
pub use publisher::ToutiaoPublisher;
pub use stats::ToutiaoStatsCollector;
