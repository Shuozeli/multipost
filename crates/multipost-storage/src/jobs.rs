//! Job repository.
//!
//! Phase 1c: file-backed JSON, synchronous Submit drives jobs through the
//! state machine in the request thread. Phase 5 replaces with a real queue.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use multipost_core::JobState;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::accounts::AccountResult;

/// One publish job: one (content, account) pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    /// Job ID.
    pub id: Uuid,
    /// Owning user.
    pub user_id: Uuid,
    /// Content body (denormalized for Phase 1c simplicity).
    pub content: serde_json::Value,
    /// Account to publish from.
    pub account_id: Uuid,
    /// Media IDs to attach.
    pub media_ids: Vec<Uuid>,
    /// Current state.
    pub state: JobState,
    /// Retry attempts so far.
    pub attempts: i32,
    /// Last error message, if any.
    pub last_error: Option<String>,
    /// Platform-side post ID (e.g. YouTube video ID).
    pub external_id: Option<String>,
    /// User-facing permalink.
    pub permalink: Option<String>,
    /// SHA-256 of the dedup key (account_id, text, media_ids, visibility,
    /// schedule_at). Used by `Posts.Submit` to detect retried submissions
    /// within a short window and return the existing job rather than
    /// double-posting. `None` on legacy records written before Phase 5.
    #[serde(default)]
    pub content_hash: Option<String>,
    /// When the row was created.
    pub created_at: DateTime<Utc>,
    /// When the row was last updated.
    pub updated_at: DateTime<Utc>,
}

/// CRUD for jobs.
#[async_trait]
pub trait JobRepository: Send + Sync + 'static {
    /// Look up a job by ID.
    async fn get(&self, user_id: Uuid, id: Uuid) -> AccountResult<Option<JobRecord>>;

    /// List jobs for a user (newest first).
    async fn list(&self, user_id: Uuid, limit: usize) -> AccountResult<Vec<JobRecord>>;

    /// Insert a new job.
    async fn insert(&self, record: JobRecord) -> AccountResult<()>;

    /// Update an existing job (full row replace).
    async fn update(&self, record: JobRecord) -> AccountResult<()>;

    /// Find the most-recent non-Failed job for `user_id` whose
    /// `content_hash == hash` AND whose `created_at >= newer_than`.
    /// Returns `None` if no such job exists. Used by `Posts.Submit` for
    /// content-hash dedup; see §22.5 of design.md.
    async fn find_recent_by_hash(
        &self,
        user_id: Uuid,
        hash: &str,
        newer_than: DateTime<Utc>,
    ) -> AccountResult<Option<JobRecord>>;

    /// All jobs in `state`, across all tenants, with `created_at >=
    /// newer_than`. Used by the server's startup recovery scan to
    /// re-attach `poll_confirm_until_terminal` tasks for `Confirming`
    /// jobs that survived a crash or restart. See §22.7 of design.md.
    async fn find_in_state(
        &self,
        state: JobState,
        newer_than: DateTime<Utc>,
    ) -> AccountResult<Vec<JobRecord>>;
}

/// JSON-file-backed job repository.
pub struct FileBackedJobRepository {
    path: PathBuf,
    state: Mutex<State>,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    jobs: HashMap<Uuid, JobRecord>,
}

impl FileBackedJobRepository {
    /// Open (or create) a JSON-backed job store at `path`.
    pub fn open(path: impl AsRef<Path>) -> AccountResult<Self> {
        let path = path.as_ref().to_path_buf();
        let state = if path.exists() {
            let bytes = std::fs::read(&path)?;
            if bytes.is_empty() {
                State::default()
            } else {
                serde_json::from_slice(&bytes)?
            }
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            State::default()
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    fn save(&self, state: &State) -> AccountResult<()> {
        let tmp = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(state)?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[async_trait]
impl JobRepository for FileBackedJobRepository {
    async fn get(&self, user_id: Uuid, id: Uuid) -> AccountResult<Option<JobRecord>> {
        let state = self.state.lock().expect("job-store mutex poisoned");
        Ok(state
            .jobs
            .get(&id)
            .filter(|j| j.user_id == user_id)
            .cloned())
    }

    async fn list(&self, user_id: Uuid, limit: usize) -> AccountResult<Vec<JobRecord>> {
        let state = self.state.lock().expect("job-store mutex poisoned");
        let mut all: Vec<JobRecord> = state
            .jobs
            .values()
            .filter(|j| j.user_id == user_id)
            .cloned()
            .collect();
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        all.truncate(limit);
        Ok(all)
    }

    async fn insert(&self, record: JobRecord) -> AccountResult<()> {
        let mut state = self.state.lock().expect("job-store mutex poisoned");
        state.jobs.insert(record.id, record);
        self.save(&state)
    }

    async fn update(&self, record: JobRecord) -> AccountResult<()> {
        let mut state = self.state.lock().expect("job-store mutex poisoned");
        state.jobs.insert(record.id, record);
        self.save(&state)
    }

    async fn find_recent_by_hash(
        &self,
        user_id: Uuid,
        hash: &str,
        newer_than: DateTime<Utc>,
    ) -> AccountResult<Option<JobRecord>> {
        let state = self.state.lock().expect("job-store mutex poisoned");
        // Walk all jobs for this user, keep non-Failed matches in the
        // window, return the newest.
        let best = state
            .jobs
            .values()
            .filter(|j| j.user_id == user_id)
            .filter(|j| j.state != JobState::Failed)
            .filter(|j| j.created_at >= newer_than)
            .filter(|j| j.content_hash.as_deref() == Some(hash))
            .max_by_key(|j| j.created_at)
            .cloned();
        Ok(best)
    }

    async fn find_in_state(
        &self,
        want: JobState,
        newer_than: DateTime<Utc>,
    ) -> AccountResult<Vec<JobRecord>> {
        let s = self.state.lock().expect("job-store mutex poisoned");
        let mut out: Vec<JobRecord> = s
            .jobs
            .values()
            .filter(|j| j.state == want)
            .filter(|j| j.created_at >= newer_than)
            .cloned()
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }
}
