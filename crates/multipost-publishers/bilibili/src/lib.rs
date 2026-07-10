//! Bilibili (哔哩哔哩) publisher.
//!
//! Unlike the other CDP-driven publishers that automate the browser UI,
//! Bilibili uses a **hybrid** approach:
//!
//! - **Authentication**: cookies are extracted from a Chrome profile that's
//!   already logged into `bilibili.com`, addressed via a CDP endpoint.
//! - **Upload + publish**: driven entirely through Bilibili's REST API
//!   (preupload → chunked upload → complete → submit), avoiding the fragile
//!   browser-based upload that crashes Chrome on large files.

#![deny(missing_docs)]

pub mod api;
pub mod credentials;
pub mod publisher;

pub use credentials::BilibiliCredentials;
pub use publisher::BilibiliPublisher;
