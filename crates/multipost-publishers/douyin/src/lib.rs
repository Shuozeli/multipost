//! Douyin (抖音) publisher.
//!
//! Drives Douyin's web creator center (creator.douyin.com) via Chrome
//! DevTools Protocol. Each Douyin account corresponds to a Chrome profile
//! with its own `--user-data-dir`; the multipost account record stores
//! the CDP HTTP endpoint that profile exposes.
//!
//! Discovery work in `scripts/douyin/` mapped the upload + post-upload UI;
//! selectors live in `selectors.rs`.

#![deny(missing_docs)]

pub mod cdp;
pub mod credentials;
pub mod publisher;
pub mod selectors;
pub mod staging;

pub use credentials::DouyinCredentials;
pub use publisher::DouyinPublisher;

/// Base URL for the Douyin creator center.
pub const CREATOR_BASE: &str = "https://creator.douyin.com";

/// Direct URL for the upload page.
pub const UPLOAD_URL: &str = "https://creator.douyin.com/creator-micro/content/upload";
