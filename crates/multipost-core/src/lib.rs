//! Shared types and the `Publisher` trait for multipost.
//!
//! This crate has zero runtime dependencies (no tokio, no reqwest). It defines
//! the abstractions every concrete publisher implements.

#![deny(missing_docs)]

pub mod content;
pub mod discovery;
pub mod error;
pub mod job;
pub mod platform;
pub mod publisher;
pub mod stats;
pub mod user_discovery;

pub use content::{Content, ContentKind, MediaRef, Visibility};
pub use discovery::{CrawlOptions, Crawler, DiscoveredItem, DiscoveryMetrics};
pub use error::{PublishError, Result};
pub use job::JobState;
pub use platform::{AuthStatus, Capabilities, Platform};
pub use publisher::{ConfirmStatus, MediaPayload, PublishContext, PublishHandle, Publisher};
pub use stats::{AccountStats, PostStats, StatsCollector, StatsOptions, StatsSnapshot};
pub use user_discovery::{UserCrawlOptions, UserCrawler};
