//! Storage for [`DiscoveredItem`]s captured by crawlers.
//!
//! Backed by a single SQLite file at `~/.multipost/discovered.sqlite`
//! (or any path passed to [`SqliteDiscoveredRepository::open`]). One
//! row per `(platform, item_id)`; re-crawls upsert the latest metrics.

use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use multipost_core::{DiscoveredItem, DiscoveryMetrics, Platform};
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

/// Errors returned by the SQLite-backed discovery repository.
#[derive(Debug, Error)]
pub enum DiscoveredError {
    /// Underlying SQLite error.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Failed to encode/decode JSON metadata.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Failed to acquire repository mutex (poisoned).
    #[error("mutex poisoned")]
    Poisoned,
    /// Platform string in the database is unknown.
    #[error("unknown platform: {0}")]
    UnknownPlatform(String),
}

/// Result alias for discovery storage operations.
pub type DiscoveredResult<T> = std::result::Result<T, DiscoveredError>;

/// Repository surface for storing and querying discovered items.
#[async_trait]
pub trait DiscoveredRepository: Send + Sync {
    /// Upsert one item. Existing rows for the same `(platform,
    /// item_id)` are overwritten with the new metrics + capture time.
    async fn upsert(&self, item: &DiscoveredItem) -> DiscoveredResult<()>;

    /// Upsert a batch. May be more efficient than calling [`upsert`]
    /// in a loop on backends that support batched writes.
    async fn upsert_many(&self, items: &[DiscoveredItem]) -> DiscoveredResult<usize>;

    /// Fetch a single item by platform + item_id.
    async fn get(
        &self,
        platform: Platform,
        item_id: &str,
    ) -> DiscoveredResult<Option<DiscoveredItem>>;

    /// List the N most-recently-captured items for a platform.
    async fn recent(
        &self,
        platform: Platform,
        limit: usize,
    ) -> DiscoveredResult<Vec<DiscoveredItem>>;
}

/// SQLite-backed implementation.
pub struct SqliteDiscoveredRepository {
    conn: Mutex<Connection>,
}

impl SqliteDiscoveredRepository {
    /// Open (or create) a SQLite database at `path` and ensure the
    /// schema exists. The parent directory must already exist.
    pub fn open(path: impl AsRef<Path>) -> DiscoveredResult<Self> {
        let conn = Connection::open(path)?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory SQLite database. Useful for tests.
    pub fn in_memory() -> DiscoveredResult<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn init_schema(conn: &Connection) -> DiscoveredResult<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS discovered_items (
                platform        TEXT    NOT NULL,
                item_id         TEXT    NOT NULL,
                captured_at     INTEGER NOT NULL,
                author_handle   TEXT    NOT NULL,
                author_name     TEXT,
                body            TEXT    NOT NULL,
                url             TEXT,
                read_count      INTEGER,
                like_count      INTEGER,
                comment_count   INTEGER,
                share_count     INTEGER,
                view_count      INTEGER,
                bookmark_count  INTEGER,
                metadata        TEXT    NOT NULL DEFAULT '{}',
                PRIMARY KEY (platform, item_id)
             );
             CREATE INDEX IF NOT EXISTS idx_discovered_by_capture
                ON discovered_items (platform, captured_at DESC);",
        )?;
        Ok(())
    }
}

#[async_trait]
impl DiscoveredRepository for SqliteDiscoveredRepository {
    async fn upsert(&self, item: &DiscoveredItem) -> DiscoveredResult<()> {
        let conn = self.conn.lock().map_err(|_| DiscoveredError::Poisoned)?;
        upsert_one(&conn, item)?;
        Ok(())
    }

    async fn upsert_many(&self, items: &[DiscoveredItem]) -> DiscoveredResult<usize> {
        let mut conn = self.conn.lock().map_err(|_| DiscoveredError::Poisoned)?;
        let tx = conn.transaction()?;
        for item in items {
            upsert_one(&tx, item)?;
        }
        tx.commit()?;
        Ok(items.len())
    }

    async fn get(
        &self,
        platform: Platform,
        item_id: &str,
    ) -> DiscoveredResult<Option<DiscoveredItem>> {
        let conn = self.conn.lock().map_err(|_| DiscoveredError::Poisoned)?;
        let plat_str = platform_to_str(platform);
        let mut stmt = conn.prepare(SELECT_BY_PK)?;
        let row = stmt
            .query_row(params![plat_str, item_id], row_to_item)
            .optional()?;
        match row {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    async fn recent(
        &self,
        platform: Platform,
        limit: usize,
    ) -> DiscoveredResult<Vec<DiscoveredItem>> {
        let conn = self.conn.lock().map_err(|_| DiscoveredError::Poisoned)?;
        let plat_str = platform_to_str(platform);
        let mut stmt = conn.prepare(SELECT_RECENT)?;
        let rows = stmt.query_map(params![plat_str, limit as i64], row_to_item)?;
        let mut out = Vec::with_capacity(limit);
        for r in rows {
            out.push(r??);
        }
        Ok(out)
    }
}

const SELECT_BY_PK: &str = "SELECT platform, item_id, captured_at, author_handle, author_name,
        body, url, read_count, like_count, comment_count, share_count,
        view_count, bookmark_count, metadata
     FROM discovered_items
     WHERE platform = ?1 AND item_id = ?2";

const SELECT_RECENT: &str = "SELECT platform, item_id, captured_at, author_handle, author_name,
        body, url, read_count, like_count, comment_count, share_count,
        view_count, bookmark_count, metadata
     FROM discovered_items
     WHERE platform = ?1
     ORDER BY captured_at DESC
     LIMIT ?2";

fn upsert_one(conn: &Connection, item: &DiscoveredItem) -> DiscoveredResult<()> {
    let plat_str = platform_to_str(item.platform);
    let metadata_json = serde_json::to_string(&item.metadata)?;
    conn.execute(
        "INSERT INTO discovered_items (
            platform, item_id, captured_at, author_handle, author_name,
            body, url, read_count, like_count, comment_count, share_count,
            view_count, bookmark_count, metadata
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT (platform, item_id) DO UPDATE SET
            captured_at    = excluded.captured_at,
            author_handle  = excluded.author_handle,
            author_name    = excluded.author_name,
            body           = CASE
                                WHEN length(excluded.body) >= length(discovered_items.body)
                                THEN excluded.body
                                ELSE discovered_items.body
                             END,
            url            = COALESCE(excluded.url, discovered_items.url),
            read_count     = excluded.read_count,
            like_count     = excluded.like_count,
            comment_count  = excluded.comment_count,
            share_count    = excluded.share_count,
            view_count     = excluded.view_count,
            bookmark_count = excluded.bookmark_count,
            metadata       = CASE
                                WHEN length(excluded.body) >= length(discovered_items.body)
                                THEN excluded.metadata
                                ELSE discovered_items.metadata
                             END",
        params![
            plat_str,
            item.item_id,
            item.captured_at.timestamp_millis(),
            item.author_handle,
            item.author_name,
            item.body,
            item.url,
            item.metrics.read_count,
            item.metrics.like_count,
            item.metrics.comment_count,
            item.metrics.share_count,
            item.metrics.view_count,
            item.metrics.bookmark_count,
            metadata_json,
        ],
    )?;
    Ok(())
}

fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<DiscoveredResult<DiscoveredItem>> {
    // Pull columns first; only platform parse + metadata parse can fail
    // outside rusqlite's own error path, so we wrap those in
    // DiscoveredError below.
    let platform_str: String = row.get(0)?;
    let item_id: String = row.get(1)?;
    let captured_ms: i64 = row.get(2)?;
    let author_handle: String = row.get(3)?;
    let author_name: Option<String> = row.get(4)?;
    let body: String = row.get(5)?;
    let url: Option<String> = row.get(6)?;
    let metrics = DiscoveryMetrics {
        read_count: row.get(7)?,
        like_count: row.get(8)?,
        comment_count: row.get(9)?,
        share_count: row.get(10)?,
        view_count: row.get(11)?,
        bookmark_count: row.get(12)?,
    };
    let metadata_str: String = row.get(13)?;
    Ok((|| -> DiscoveredResult<DiscoveredItem> {
        let platform = platform_from_str(&platform_str)?;
        let metadata = serde_json::from_str(&metadata_str)?;
        let captured_at: DateTime<Utc> = Utc
            .timestamp_millis_opt(captured_ms)
            .single()
            .unwrap_or_else(Utc::now);
        Ok(DiscoveredItem {
            platform,
            item_id,
            captured_at,
            author_handle,
            author_name,
            body,
            url,
            metrics,
            metadata,
        })
    })())
}

fn platform_to_str(p: Platform) -> &'static str {
    match p {
        Platform::YouTube => "youtube",
        Platform::WxGzh => "wx_gzh",
        Platform::Twitter => "twitter",
        Platform::Douyin => "douyin",
        Platform::Toutiao => "toutiao",
        Platform::Bilibili => "bilibili",
    }
}

fn platform_from_str(s: &str) -> DiscoveredResult<Platform> {
    Ok(match s {
        "youtube" => Platform::YouTube,
        "wx_gzh" => Platform::WxGzh,
        "twitter" => Platform::Twitter,
        "douyin" => Platform::Douyin,
        "toutiao" => Platform::Toutiao,
        other => return Err(DiscoveredError::UnknownPlatform(other.to_string())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sample_item(item_id: &str, reads: i64) -> DiscoveredItem {
        let mut metadata = HashMap::new();
        metadata.insert(
            "cell_type".to_string(),
            serde_json::Value::Number(32.into()),
        );
        DiscoveredItem {
            platform: Platform::Toutiao,
            item_id: item_id.to_string(),
            captured_at: Utc::now(),
            author_handle: "test_handle".to_string(),
            author_name: Some("Test Name".to_string()),
            body: "hello world".to_string(),
            url: Some("https://example.com/123".to_string()),
            metrics: DiscoveryMetrics {
                read_count: Some(reads),
                like_count: Some(2),
                comment_count: Some(0),
                share_count: Some(1),
                view_count: None,
                bookmark_count: None,
            },
            metadata,
        }
    }

    #[tokio::test]
    async fn upsert_and_get_roundtrips() {
        // Arrange
        let repo = SqliteDiscoveredRepository::in_memory().unwrap();
        let item = sample_item("abc", 100);

        // Act
        repo.upsert(&item).await.unwrap();
        let fetched = repo.get(Platform::Toutiao, "abc").await.unwrap();

        // Assert
        let got = fetched.expect("row should exist");
        assert_eq!(got.item_id, "abc");
        assert_eq!(got.metrics.read_count, Some(100));
        assert_eq!(
            got.metadata.get("cell_type"),
            Some(&serde_json::Value::Number(32.into()))
        );
    }

    #[tokio::test]
    async fn upsert_overwrites_metrics_on_conflict() {
        // Arrange
        let repo = SqliteDiscoveredRepository::in_memory().unwrap();
        repo.upsert(&sample_item("abc", 100)).await.unwrap();

        // Act: same item_id, new reads
        repo.upsert(&sample_item("abc", 500)).await.unwrap();
        let fetched = repo.get(Platform::Toutiao, "abc").await.unwrap();

        // Assert
        assert_eq!(fetched.unwrap().metrics.read_count, Some(500));
    }

    #[tokio::test]
    async fn upsert_keeps_longer_existing_body_and_metadata() {
        // Arrange
        let repo = SqliteDiscoveredRepository::in_memory().unwrap();
        let mut full = sample_item("abc", 100);
        full.platform = Platform::Twitter;
        full.body = "complete tweet text with detail-page context".to_string();
        full.metadata
            .insert("source".to_string(), serde_json::json!("detail"));
        repo.upsert(&full).await.unwrap();

        let mut short = sample_item("abc", 500);
        short.platform = Platform::Twitter;
        short.body = "short card".to_string();
        short
            .metadata
            .insert("source".to_string(), serde_json::json!("timeline"));

        // Act
        repo.upsert(&short).await.unwrap();
        let fetched = repo
            .get(Platform::Twitter, "abc")
            .await
            .unwrap()
            .expect("row should exist");

        // Assert
        assert_eq!(fetched.body, "complete tweet text with detail-page context");
        assert_eq!(fetched.metrics.read_count, Some(500));
        assert_eq!(
            fetched.metadata.get("source").and_then(|v| v.as_str()),
            Some("detail")
        );
    }

    #[tokio::test]
    async fn upsert_many_inserts_all() {
        // Arrange
        let repo = SqliteDiscoveredRepository::in_memory().unwrap();
        let items: Vec<_> = (0..5)
            .map(|i| sample_item(&format!("id-{i}"), 10 * i))
            .collect();

        // Act
        let n = repo.upsert_many(&items).await.unwrap();
        let recent = repo.recent(Platform::Toutiao, 10).await.unwrap();

        // Assert
        assert_eq!(n, 5);
        assert_eq!(recent.len(), 5);
    }

    #[tokio::test]
    async fn get_returns_none_for_missing() {
        // Arrange
        let repo = SqliteDiscoveredRepository::in_memory().unwrap();

        // Act
        let fetched = repo.get(Platform::Toutiao, "nope").await.unwrap();

        // Assert
        assert!(fetched.is_none());
    }

    #[tokio::test]
    async fn recent_orders_by_capture_time_desc() {
        // Arrange
        let repo = SqliteDiscoveredRepository::in_memory().unwrap();
        let mut a = sample_item("a", 1);
        let mut b = sample_item("b", 2);
        a.captured_at = Utc.timestamp_millis_opt(1_000_000).single().unwrap();
        b.captured_at = Utc.timestamp_millis_opt(2_000_000).single().unwrap();
        repo.upsert(&a).await.unwrap();
        repo.upsert(&b).await.unwrap();

        // Act
        let recent = repo.recent(Platform::Toutiao, 10).await.unwrap();

        // Assert
        assert_eq!(recent[0].item_id, "b");
        assert_eq!(recent[1].item_id, "a");
    }
}
