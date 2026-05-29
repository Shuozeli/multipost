//! Account repository.
//!
//! Phase 0: trait + in-memory stub.
//! Phase 1: file-backed JSON impl so accounts survive server restarts.
//! Phase 5 (future): replace with Postgres via quiver-orm / sqlx.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use multipost_core::{AuthStatus, Platform};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// What we know about a connected platform account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountRecord {
    /// Account row ID.
    pub id: Uuid,
    /// Owning user.
    pub user_id: Uuid,
    /// Platform.
    pub platform: Platform,
    /// User-facing handle (e.g. @username).
    pub display_name: String,
    /// Platform-side ID.
    pub external_id: String,
    /// Current auth status.
    pub auth_status: AuthStatus,
    /// Credentials as JSON — shape is publisher-specific.
    /// For YouTube: `{"access_token": ..., "refresh_token": ..., "expires_at": ...}`.
    pub credentials: serde_json::Value,
    /// When the row was created.
    pub created_at: DateTime<Utc>,
    /// When it was last used or refreshed.
    pub last_used_at: DateTime<Utc>,
}

/// Errors the account repo can surface beyond sqlx's own.
#[derive(Debug, Error)]
pub enum AccountError {
    /// I/O failure on the file-backed store.
    #[error("file io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialization failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Result alias for account ops.
pub type AccountResult<T> = Result<T, AccountError>;

/// CRUD for accounts.
#[async_trait]
pub trait AccountRepository: Send + Sync + 'static {
    /// List all accounts for a user, optionally filtered by platform.
    async fn list(
        &self,
        user_id: Uuid,
        platform: Option<Platform>,
    ) -> AccountResult<Vec<AccountRecord>>;

    /// Look up one account by ID, scoped to a user.
    async fn get(&self, user_id: Uuid, id: Uuid) -> AccountResult<Option<AccountRecord>>;

    /// Insert or update an account. The ID is the primary key.
    async fn upsert(&self, account: AccountRecord) -> AccountResult<()>;

    /// Remove an account.
    async fn delete(&self, user_id: Uuid, id: Uuid) -> AccountResult<()>;
}

// -------- In-memory stub (used by tests, server boot before file is loaded) --------

/// Empty stub used for tests and Phase-0 wiring. Returns `[]` for every call.
pub struct InMemoryAccountRepository;

#[async_trait]
impl AccountRepository for InMemoryAccountRepository {
    async fn list(
        &self,
        _user_id: Uuid,
        _platform: Option<Platform>,
    ) -> AccountResult<Vec<AccountRecord>> {
        Ok(Vec::new())
    }

    async fn get(&self, _user_id: Uuid, _id: Uuid) -> AccountResult<Option<AccountRecord>> {
        Ok(None)
    }

    async fn upsert(&self, _account: AccountRecord) -> AccountResult<()> {
        Ok(())
    }

    async fn delete(&self, _user_id: Uuid, _id: Uuid) -> AccountResult<()> {
        Ok(())
    }
}

// -------- File-backed (Phase 1) --------

/// JSON-file-backed account repository.
///
/// Loads the whole file into memory on first access, mutates in place,
/// writes the whole file on every change. Fine for the small N expected
/// in single-user mode; replaced with a real DB in Phase 5.
pub struct FileBackedAccountRepository {
    path: PathBuf,
    state: Mutex<State>,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    accounts: HashMap<Uuid, AccountRecord>,
}

impl FileBackedAccountRepository {
    /// Open (or create) a JSON-backed store at `path`.
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
impl AccountRepository for FileBackedAccountRepository {
    async fn list(
        &self,
        user_id: Uuid,
        platform: Option<Platform>,
    ) -> AccountResult<Vec<AccountRecord>> {
        let state = self.state.lock().expect("account-store mutex poisoned");
        Ok(state
            .accounts
            .values()
            .filter(|a| a.user_id == user_id)
            .filter(|a| platform.is_none_or(|p| a.platform == p))
            .cloned()
            .collect())
    }

    async fn get(&self, user_id: Uuid, id: Uuid) -> AccountResult<Option<AccountRecord>> {
        let state = self.state.lock().expect("account-store mutex poisoned");
        Ok(state
            .accounts
            .get(&id)
            .filter(|a| a.user_id == user_id)
            .cloned())
    }

    async fn upsert(&self, account: AccountRecord) -> AccountResult<()> {
        let mut state = self.state.lock().expect("account-store mutex poisoned");
        state.accounts.insert(account.id, account);
        self.save(&state)
    }

    async fn delete(&self, user_id: Uuid, id: Uuid) -> AccountResult<()> {
        let mut state = self.state.lock().expect("account-store mutex poisoned");
        if let Some(a) = state.accounts.get(&id)
            && a.user_id == user_id
        {
            state.accounts.remove(&id);
            self.save(&state)?;
        }
        Ok(())
    }
}
