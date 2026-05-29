//! Storage layer.
//!
//! Repository traits + Postgres implementations.
//! Schema migrations live in `migrations/` (added in Phase 0c).

#![deny(missing_docs)]

pub mod accounts;
pub mod discovered;
pub mod jobs;
pub mod media;
pub mod stats;
pub mod tenants;

// Re-export the connection helper at the crate root.
pub use connection::{connect, DbPool};

mod connection {
    use sqlx::postgres::PgPoolOptions;
    use sqlx::PgPool;

    /// Wrapper around the sqlx connection pool. Renamed for callers'
    /// convenience.
    pub type DbPool = PgPool;

    /// Open a Postgres connection pool.
    pub async fn connect(url: &str, max_connections: u32) -> Result<DbPool, sqlx::Error> {
        PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(url)
            .await
    }
}
