//! Storage for profile-stats snapshots collected by
//! [`StatsCollector`](multipost_core::StatsCollector)s.
//!
//! Backed by SQLite (the same `~/.multipost/discovered.sqlite` file or a
//! dedicated one). Unlike [`discovered`](crate::discovered) — which keeps
//! one row per item and upserts the latest metrics — stats are stored as
//! **timestamped snapshots**: every collect run appends new rows, so
//! callers can chart growth over time. There are two tables:
//!
//! - `account_stats`: one row per (account, collect-run).
//! - `post_stats`: one row per (account, post, collect-run).

use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use multipost_core::{AccountStats, Platform, PostStats};
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use uuid::Uuid;

/// Errors returned by the SQLite-backed stats repository.
#[derive(Debug, Error)]
pub enum StatsError {
    /// Underlying SQLite error.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Failed to encode/decode JSON metadata.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Repository mutex was poisoned.
    #[error("mutex poisoned")]
    Poisoned,
    /// Platform string in the database is unknown.
    #[error("unknown platform: {0}")]
    UnknownPlatform(String),
}

/// Result alias for stats storage operations.
pub type StatsResult<T> = std::result::Result<T, StatsError>;

/// Repository surface for storing and querying stats snapshots.
#[async_trait]
pub trait StatsRepository: Send + Sync {
    /// Append one account-level snapshot for `account_id`.
    async fn insert_account(&self, account_id: Uuid, stats: &AccountStats) -> StatsResult<()>;

    /// Append a batch of per-post snapshots for `account_id`.
    async fn insert_posts(&self, account_id: Uuid, posts: &[PostStats]) -> StatsResult<usize>;

    /// Most recent account snapshot for `(platform, account_id)`.
    async fn latest_account(
        &self,
        platform: Platform,
        account_id: Uuid,
    ) -> StatsResult<Option<AccountStats>>;

    /// Up to `limit` account snapshots, newest first — the growth series.
    async fn account_history(
        &self,
        platform: Platform,
        account_id: Uuid,
        limit: usize,
    ) -> StatsResult<Vec<AccountStats>>;

    /// The latest snapshot for each of the account's posts, newest first,
    /// capped at `limit`.
    async fn latest_posts(
        &self,
        platform: Platform,
        account_id: Uuid,
        limit: usize,
    ) -> StatsResult<Vec<PostStats>>;
}

/// SQLite-backed implementation.
pub struct SqliteStatsRepository {
    conn: Mutex<Connection>,
}

impl SqliteStatsRepository {
    /// Open (or create) a SQLite database at `path` and ensure the schema
    /// exists. The parent directory must already exist.
    pub fn open(path: impl AsRef<Path>) -> StatsResult<Self> {
        let conn = Connection::open(path)?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory SQLite database. Useful for tests.
    pub fn in_memory() -> StatsResult<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn init_schema(conn: &Connection) -> StatsResult<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS account_stats (
                id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                platform            TEXT    NOT NULL,
                account_id          TEXT    NOT NULL,
                captured_at         INTEGER NOT NULL,
                followers           INTEGER,
                following           INTEGER,
                post_count          INTEGER,
                total_views         INTEGER,
                total_income        REAL,
                yesterday_followers INTEGER,
                yesterday_views     INTEGER,
                yesterday_income    REAL,
                metadata            TEXT    NOT NULL DEFAULT '{}'
             );
             CREATE INDEX IF NOT EXISTS idx_account_stats_recent
                ON account_stats (account_id, captured_at DESC);

             CREATE TABLE IF NOT EXISTS post_stats (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                platform     TEXT    NOT NULL,
                account_id   TEXT    NOT NULL,
                post_id      TEXT    NOT NULL,
                captured_at  INTEGER NOT NULL,
                title        TEXT    NOT NULL DEFAULT '',
                post_type    TEXT    NOT NULL DEFAULT '',
                created_at   INTEGER,
                impressions  INTEGER,
                reads        INTEGER,
                likes        INTEGER,
                comments     INTEGER,
                shares       INTEGER,
                bookmarks    INTEGER,
                plays        INTEGER,
                metadata     TEXT    NOT NULL DEFAULT '{}'
             );
             CREATE INDEX IF NOT EXISTS idx_post_stats_recent
                ON post_stats (account_id, post_id, captured_at DESC);",
        )?;
        Ok(())
    }
}

#[async_trait]
impl StatsRepository for SqliteStatsRepository {
    async fn insert_account(&self, account_id: Uuid, stats: &AccountStats) -> StatsResult<()> {
        let conn = self.conn.lock().map_err(|_| StatsError::Poisoned)?;
        let metadata = serde_json::to_string(&stats.metadata)?;
        conn.execute(
            "INSERT INTO account_stats (
                platform, account_id, captured_at, followers, following,
                post_count, total_views, total_income, yesterday_followers,
                yesterday_views, yesterday_income, metadata
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                platform_to_str(stats.platform),
                account_id.to_string(),
                stats.captured_at.timestamp(),
                stats.followers,
                stats.following,
                stats.post_count,
                stats.total_views,
                stats.total_income,
                stats.yesterday_followers,
                stats.yesterday_views,
                stats.yesterday_income,
                metadata,
            ],
        )?;
        Ok(())
    }

    async fn insert_posts(&self, account_id: Uuid, posts: &[PostStats]) -> StatsResult<usize> {
        let mut conn = self.conn.lock().map_err(|_| StatsError::Poisoned)?;
        let tx = conn.transaction()?;
        for p in posts {
            let metadata = serde_json::to_string(&p.metadata)?;
            tx.execute(
                "INSERT INTO post_stats (
                    platform, account_id, post_id, captured_at, title, post_type,
                    created_at, impressions, reads, likes, comments, shares,
                    bookmarks, plays, metadata
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    platform_to_str(p.platform),
                    account_id.to_string(),
                    p.post_id,
                    p.captured_at.timestamp(),
                    p.title,
                    p.post_type,
                    p.created_at.map(|t| t.timestamp()),
                    p.impressions,
                    p.reads,
                    p.likes,
                    p.comments,
                    p.shares,
                    p.bookmarks,
                    p.plays,
                    metadata,
                ],
            )?;
        }
        tx.commit()?;
        Ok(posts.len())
    }

    async fn latest_account(
        &self,
        platform: Platform,
        account_id: Uuid,
    ) -> StatsResult<Option<AccountStats>> {
        let conn = self.conn.lock().map_err(|_| StatsError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT platform, captured_at, followers, following, post_count,
                    total_views, total_income, yesterday_followers,
                    yesterday_views, yesterday_income, metadata
             FROM account_stats
             WHERE platform = ?1 AND account_id = ?2
             ORDER BY captured_at DESC LIMIT 1",
        )?;
        let row = stmt
            .query_row(
                params![platform_to_str(platform), account_id.to_string()],
                row_to_account,
            )
            .optional()?;
        match row {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    async fn account_history(
        &self,
        platform: Platform,
        account_id: Uuid,
        limit: usize,
    ) -> StatsResult<Vec<AccountStats>> {
        let conn = self.conn.lock().map_err(|_| StatsError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT platform, captured_at, followers, following, post_count,
                    total_views, total_income, yesterday_followers,
                    yesterday_views, yesterday_income, metadata
             FROM account_stats
             WHERE platform = ?1 AND account_id = ?2
             ORDER BY captured_at DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![
                platform_to_str(platform),
                account_id.to_string(),
                limit as i64
            ],
            row_to_account,
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r??);
        }
        Ok(out)
    }

    async fn latest_posts(
        &self,
        platform: Platform,
        account_id: Uuid,
        limit: usize,
    ) -> StatsResult<Vec<PostStats>> {
        let conn = self.conn.lock().map_err(|_| StatsError::Poisoned)?;
        // Latest snapshot per post_id (correlated MAX(captured_at)).
        let mut stmt = conn.prepare(
            "SELECT platform, post_id, captured_at, title, post_type, created_at,
                    impressions, reads, likes, comments, shares, bookmarks,
                    plays, metadata
             FROM post_stats ps
             WHERE platform = ?1 AND account_id = ?2
               AND captured_at = (
                   SELECT MAX(captured_at) FROM post_stats
                   WHERE account_id = ps.account_id AND post_id = ps.post_id
               )
             ORDER BY captured_at DESC, id DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![
                platform_to_str(platform),
                account_id.to_string(),
                limit as i64
            ],
            row_to_post,
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r??);
        }
        Ok(out)
    }
}

fn ts_to_dt(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).single().unwrap_or_else(Utc::now)
}

fn row_to_account(row: &rusqlite::Row) -> rusqlite::Result<StatsResult<AccountStats>> {
    let plat_str: String = row.get(0)?;
    let metadata_str: String = row.get(10)?;
    Ok((|| {
        Ok(AccountStats {
            platform: platform_from_str(&plat_str)?,
            captured_at: ts_to_dt(row.get::<_, i64>(1)?),
            followers: row.get(2)?,
            following: row.get(3)?,
            post_count: row.get(4)?,
            total_views: row.get(5)?,
            total_income: row.get(6)?,
            yesterday_followers: row.get(7)?,
            yesterday_views: row.get(8)?,
            yesterday_income: row.get(9)?,
            metadata: serde_json::from_str(&metadata_str)?,
        })
    })())
}

fn row_to_post(row: &rusqlite::Row) -> rusqlite::Result<StatsResult<PostStats>> {
    let plat_str: String = row.get(0)?;
    let created: Option<i64> = row.get(5)?;
    let metadata_str: String = row.get(13)?;
    Ok((|| {
        Ok(PostStats {
            platform: platform_from_str(&plat_str)?,
            post_id: row.get(1)?,
            captured_at: ts_to_dt(row.get::<_, i64>(2)?),
            title: row.get(3)?,
            post_type: row.get(4)?,
            created_at: created.map(ts_to_dt),
            impressions: row.get(6)?,
            reads: row.get(7)?,
            likes: row.get(8)?,
            comments: row.get(9)?,
            shares: row.get(10)?,
            bookmarks: row.get(11)?,
            plays: row.get(12)?,
            metadata: serde_json::from_str(&metadata_str)?,
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

fn platform_from_str(s: &str) -> StatsResult<Platform> {
    Ok(match s {
        "youtube" => Platform::YouTube,
        "wx_gzh" => Platform::WxGzh,
        "twitter" => Platform::Twitter,
        "douyin" => Platform::Douyin,
        "toutiao" => Platform::Toutiao,
        other => return Err(StatsError::UnknownPlatform(other.to_string())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(followers: i64, captured: i64) -> AccountStats {
        let mut a = AccountStats::new(Platform::Toutiao, ts_to_dt(captured));
        a.followers = Some(followers);
        a.total_income = Some(6.21);
        a
    }

    fn post(id: &str, impressions: i64, captured: i64) -> PostStats {
        let mut p = PostStats::new(Platform::Toutiao, id.to_string(), ts_to_dt(captured));
        p.impressions = Some(impressions);
        p.title = "t".into();
        p
    }

    #[tokio::test]
    async fn latest_account_returns_newest_snapshot() {
        // Arrange
        let repo = SqliteStatsRepository::in_memory().unwrap();
        let acc = Uuid::nil();
        repo.insert_account(acc, &acct(19, 1000)).await.unwrap();
        repo.insert_account(acc, &acct(20, 2000)).await.unwrap();

        // Act
        let latest = repo
            .latest_account(Platform::Toutiao, acc)
            .await
            .unwrap()
            .unwrap();

        // Assert
        assert_eq!(latest.followers, Some(20));
        assert_eq!(latest.total_income, Some(6.21));
    }

    #[tokio::test]
    async fn account_history_is_newest_first() {
        // Arrange
        let repo = SqliteStatsRepository::in_memory().unwrap();
        let acc = Uuid::nil();
        repo.insert_account(acc, &acct(19, 1000)).await.unwrap();
        repo.insert_account(acc, &acct(20, 2000)).await.unwrap();

        // Act
        let hist = repo
            .account_history(Platform::Toutiao, acc, 10)
            .await
            .unwrap();

        // Assert
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].followers, Some(20));
        assert_eq!(hist[1].followers, Some(19));
    }

    #[tokio::test]
    async fn latest_posts_keeps_one_row_per_post() {
        // Arrange — two snapshots of the same post, plus a second post.
        let repo = SqliteStatsRepository::in_memory().unwrap();
        let acc = Uuid::nil();
        repo.insert_posts(acc, &[post("a", 100, 1000)])
            .await
            .unwrap();
        repo.insert_posts(acc, &[post("a", 150, 2000), post("b", 5, 2000)])
            .await
            .unwrap();

        // Act
        let posts = repo.latest_posts(Platform::Toutiao, acc, 10).await.unwrap();

        // Assert — post "a" appears once, with its newest impressions.
        assert_eq!(posts.len(), 2);
        let a = posts.iter().find(|p| p.post_id == "a").unwrap();
        assert_eq!(a.impressions, Some(150));
    }
}
