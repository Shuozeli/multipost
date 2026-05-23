//! Media repository.
//!
//! Phase 1c: file-backed JSON metadata + a media directory for blob storage.
//! Phase 5 (future): swap blob storage for S3 (rustfs) and metadata for Postgres.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::accounts::{AccountError, AccountResult};

/// Metadata for one stored media blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaRecord {
    /// Media ID (same as the filename stem).
    pub id: Uuid,
    /// Owning user.
    pub user_id: Uuid,
    /// MIME type (e.g. `video/mp4`).
    pub mime_type: String,
    /// Source filename (informational).
    pub filename: String,
    /// On-disk path. Absolute.
    pub path: PathBuf,
    /// File size in bytes.
    pub size_bytes: i64,
    /// SHA-256 of the file content.
    pub sha256: String,
    /// When it was uploaded.
    pub created_at: DateTime<Utc>,
}

/// CRUD for media records.
#[async_trait]
pub trait MediaRepository: Send + Sync + 'static {
    /// Look up a media record by ID, scoped to a user.
    async fn get(&self, user_id: Uuid, id: Uuid) -> AccountResult<Option<MediaRecord>>;

    /// Insert a media record.
    async fn insert(&self, record: MediaRecord) -> AccountResult<()>;

    /// Delete a media record AND its backing file.
    async fn delete(&self, user_id: Uuid, id: Uuid) -> AccountResult<()>;
}

/// JSON-file-backed media repository. Blobs live in `media_dir`.
pub struct FileBackedMediaRepository {
    state_path: PathBuf,
    media_dir: PathBuf,
    state: Mutex<State>,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    media: HashMap<Uuid, MediaRecord>,
}

impl FileBackedMediaRepository {
    /// Open (or create) a JSON-backed media store.
    ///
    /// `state_path` is a JSON file of metadata; `media_dir` holds the
    /// raw blobs as `<uuid>.<ext>`.
    pub fn open(state_path: impl AsRef<Path>, media_dir: impl AsRef<Path>) -> AccountResult<Self> {
        let state_path = state_path.as_ref().to_path_buf();
        let media_dir = media_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&media_dir)?;
        let state = if state_path.exists() {
            let bytes = std::fs::read(&state_path)?;
            if bytes.is_empty() {
                State::default()
            } else {
                serde_json::from_slice(&bytes)?
            }
        } else {
            if let Some(parent) = state_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            State::default()
        };
        Ok(Self {
            state_path,
            media_dir,
            state: Mutex::new(state),
        })
    }

    /// Directory where blob files live.
    pub fn media_dir(&self) -> &Path {
        &self.media_dir
    }

    fn save(&self, state: &State) -> AccountResult<()> {
        let tmp = self.state_path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(state)?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &self.state_path)?;
        Ok(())
    }
}

#[async_trait]
impl MediaRepository for FileBackedMediaRepository {
    async fn get(&self, user_id: Uuid, id: Uuid) -> AccountResult<Option<MediaRecord>> {
        let state = self.state.lock().expect("media-store mutex poisoned");
        Ok(state
            .media
            .get(&id)
            .filter(|m| m.user_id == user_id)
            .cloned())
    }

    async fn insert(&self, record: MediaRecord) -> AccountResult<()> {
        let mut state = self.state.lock().expect("media-store mutex poisoned");
        state.media.insert(record.id, record);
        self.save(&state)
    }

    async fn delete(&self, user_id: Uuid, id: Uuid) -> AccountResult<()> {
        let mut state = self.state.lock().expect("media-store mutex poisoned");
        if let Some(m) = state.media.get(&id) {
            if m.user_id == user_id {
                let path = m.path.clone();
                state.media.remove(&id);
                self.save(&state)?;
                if path.exists() {
                    std::fs::remove_file(&path).map_err(AccountError::Io)?;
                }
            }
        }
        Ok(())
    }
}
