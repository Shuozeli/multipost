//! Error types used across publishers.

use thiserror::Error;

/// Result alias for publisher operations.
pub type Result<T> = std::result::Result<T, PublishError>;

/// Errors a publisher can surface.
///
/// Variants are classified by retry policy: the orchestrator uses these
/// to decide whether to retry, route to re-auth, or dead-letter the job.
#[derive(Debug, Error)]
pub enum PublishError {
    /// The account's auth is no longer valid.
    /// Orchestrator routes to re-auth; does NOT retry.
    #[error("auth expired for {0}")]
    AuthExpired(&'static str),

    /// The platform rejected the content (size, format, policy).
    /// Orchestrator marks failed; does NOT retry.
    #[error("rejected by platform: {0}")]
    Rejected(String),

    /// The platform is rate-limiting us. Orchestrator should honor
    /// `Retry-After` (or use exponential backoff).
    #[error("rate limited, retry after {retry_after_secs}s")]
    RateLimited {
        /// Seconds until we may retry (from `Retry-After` header).
        retry_after_secs: u64,
    },

    /// Transient network or platform error. Orchestrator retries with backoff.
    #[error("transient: {0}")]
    Transient(String),

    /// Anything we didn't think of, treated as transient by default.
    #[error("unexpected: {0}")]
    Other(#[from] anyhow::Error),
}
