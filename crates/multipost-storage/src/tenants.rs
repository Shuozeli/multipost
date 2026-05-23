//! Tenant repository.
//!
//! A `Tenant` is the downstream caller scope. Every gRPC handler reads
//! `tenant_id` from request extensions (injected by the auth interceptor)
//! and uses it where the old `bootstrap_user` constant used to live.
//!
//! Storage shape mirrors `accounts.rs` — file-backed JSON, swappable for
//! Postgres later. Tenant management is CLI-direct (no gRPC service)
//! to avoid the bootstrap chicken-and-egg of "you need a tenant to make
//! a tenant".

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

/// One tenant: a downstream caller scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantRecord {
    /// Tenant ID.
    pub id: Uuid,
    /// Human-readable name (only shown in `tenants list`, never authoritative).
    pub name: String,
    /// SHA-256 hashes of all currently-valid API keys, format
    /// `"sha256:<lowercase-hex>"`. Multiple to support rotation: add a
    /// new key, grace period, then revoke the old one.
    pub key_hashes: Vec<String>,
    /// Created timestamp.
    pub created_at: DateTime<Utc>,
    /// Last-modified timestamp (any key add / revoke / rename).
    pub updated_at: DateTime<Utc>,
}

/// Tenant repo errors.
#[derive(Debug, Error)]
pub enum TenantError {
    /// I/O failure on the file-backed store.
    #[error("file io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialization failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Lookup by ID found nothing.
    #[error("no tenant with id {0}")]
    NotFound(Uuid),
    /// `revoke_key` was given a prefix that doesn't match any key on the
    /// target tenant. Distinguished from `NotFound` so the CLI can give a
    /// clearer error.
    #[error("no key on tenant {tenant} matches prefix {prefix:?}")]
    NoMatchingKey {
        /// Tenant ID.
        tenant: Uuid,
        /// The prefix the caller supplied.
        prefix: String,
    },
    /// Refused to revoke the only remaining key — would lock the tenant
    /// out. Caller should add a replacement key first.
    #[error("refusing to revoke the only remaining key on tenant {0}")]
    LastKey(Uuid),
}

/// Result alias.
pub type TenantResult<T> = Result<T, TenantError>;

/// Hash a plaintext API key the same way it's stored on disk.
pub fn hash_key(plaintext: &str) -> String {
    let digest = Sha256::digest(plaintext.as_bytes());
    format!("sha256:{:x}", digest)
}

/// JSON-file-backed tenant store. Same pattern as `FileBackedAccountRepository`.
///
/// The `Mutex<State>` keeps the whole tenant set in memory, so the
/// hot-path key lookup ([`resolve_key`]) is a single in-memory scan.
/// For an N of single-digit tenants this is fine; revisit when we
/// migrate to Postgres.
///
/// [`resolve_key`]: FileBackedTenantRepository::resolve_key
pub struct FileBackedTenantRepository {
    path: PathBuf,
    state: Mutex<State>,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    tenants: HashMap<Uuid, TenantRecord>,
}

impl FileBackedTenantRepository {
    /// Open (or create) the tenants store at `path`.
    pub fn open(path: impl AsRef<Path>) -> TenantResult<Self> {
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

    fn save(&self, state: &State) -> TenantResult<()> {
        let tmp = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(state)?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Create a new tenant with one fresh API key. Returns the record
    /// and the **plaintext key** — this is the only time the caller will
    /// ever see it. Operator must copy it into wherever they configure
    /// downstream services.
    pub fn create(&self, name: String) -> TenantResult<(TenantRecord, String)> {
        let plaintext = mint_key();
        let now = Utc::now();
        let rec = TenantRecord {
            id: Uuid::new_v4(),
            name,
            key_hashes: vec![hash_key(&plaintext)],
            created_at: now,
            updated_at: now,
        };
        let mut state = self.state.lock().expect("tenant-store mutex poisoned");
        state.tenants.insert(rec.id, rec.clone());
        self.save(&state)?;
        Ok((rec, plaintext))
    }

    /// All tenants. (Small N — no pagination yet.)
    pub fn list(&self) -> TenantResult<Vec<TenantRecord>> {
        let state = self.state.lock().expect("tenant-store mutex poisoned");
        Ok(state.tenants.values().cloned().collect())
    }

    /// Look up by tenant ID.
    pub fn get(&self, id: Uuid) -> TenantResult<Option<TenantRecord>> {
        let state = self.state.lock().expect("tenant-store mutex poisoned");
        Ok(state.tenants.get(&id).cloned())
    }

    /// Add a new key to an existing tenant. Returns the plaintext.
    /// Old keys remain valid until [`revoke_key`] is called — that's the
    /// rotation grace period.
    ///
    /// [`revoke_key`]: FileBackedTenantRepository::revoke_key
    pub fn add_key(&self, id: Uuid) -> TenantResult<String> {
        let plaintext = mint_key();
        let hash = hash_key(&plaintext);
        let mut state = self.state.lock().expect("tenant-store mutex poisoned");
        let rec = state.tenants.get_mut(&id).ok_or(TenantError::NotFound(id))?;
        rec.key_hashes.push(hash);
        rec.updated_at = Utc::now();
        self.save(&state)?;
        Ok(plaintext)
    }

    /// Revoke a key by hash-prefix match. Refuses to remove the last
    /// remaining key (which would orphan the tenant — add a replacement first).
    pub fn revoke_key(&self, id: Uuid, hash_prefix: &str) -> TenantResult<()> {
        let mut state = self.state.lock().expect("tenant-store mutex poisoned");
        let rec = state.tenants.get_mut(&id).ok_or(TenantError::NotFound(id))?;
        let matches: Vec<usize> = rec
            .key_hashes
            .iter()
            .enumerate()
            .filter(|(_, h)| h.starts_with(hash_prefix))
            .map(|(i, _)| i)
            .collect();
        if matches.is_empty() {
            return Err(TenantError::NoMatchingKey {
                tenant: id,
                prefix: hash_prefix.to_string(),
            });
        }
        if rec.key_hashes.len() == matches.len() {
            return Err(TenantError::LastKey(id));
        }
        // Remove highest-index-first so earlier indices stay valid.
        for i in matches.iter().rev() {
            rec.key_hashes.remove(*i);
        }
        rec.updated_at = Utc::now();
        self.save(&state)
    }

    /// Synchronous key lookup used by the auth interceptor. Returns the
    /// tenant ID whose `key_hashes` contains the supplied hash, if any.
    ///
    /// On a miss against the in-memory cache, we reload from disk once
    /// and retry. This lets the running server pick up tenants the CLI
    /// just created via direct file write, without needing a restart or
    /// a file-watcher. Hit path stays in-memory.
    pub fn resolve_key(&self, hash: &str) -> Option<Uuid> {
        if let Some(id) = self.scan_in_memory(hash) {
            return Some(id);
        }
        // Miss — reload from disk and retry.
        if self.reload_from_disk().is_ok() {
            return self.scan_in_memory(hash);
        }
        None
    }

    fn scan_in_memory(&self, hash: &str) -> Option<Uuid> {
        let state = self.state.lock().expect("tenant-store mutex poisoned");
        state
            .tenants
            .values()
            .find(|t| t.key_hashes.iter().any(|h| h == hash))
            .map(|t| t.id)
    }

    fn reload_from_disk(&self) -> TenantResult<()> {
        if !self.path.exists() {
            return Ok(());
        }
        let bytes = std::fs::read(&self.path)?;
        if bytes.is_empty() {
            return Ok(());
        }
        let fresh: State = serde_json::from_slice(&bytes)?;
        *self.state.lock().expect("tenant-store mutex poisoned") = fresh;
        Ok(())
    }
}

/// Generate a fresh API key: 32 random bytes, base64-url encoded
/// (43 chars, no padding). Equivalent strength to a v4 UUID's randomness
/// but URL-safe and obviously a credential.
fn mint_key() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64_url(&bytes)
}

/// Tiny base64url encoder so we don't pull in `base64` for one call site.
fn base64_url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((bytes.len() * 4).div_ceil(3));
    let chunks = bytes.chunks_exact(3);
    let rem = chunks.remainder();
    for c in chunks {
        let n = u32::from(c[0]) << 16 | u32::from(c[1]) << 8 | u32::from(c[2]);
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        out.push(ALPHABET[(n & 0x3f) as usize] as char);
    }
    match rem.len() {
        1 => {
            let n = u32::from(rem[0]) << 16;
            out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        }
        2 => {
            let n = u32::from(rem[0]) << 16 | u32::from(rem[1]) << 8;
            out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_repo() -> (FileBackedTenantRepository, TempDir) {
        // Arrange
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("tenants.json");
        let repo = FileBackedTenantRepository::open(&path).unwrap();
        (repo, dir)
    }

    #[test]
    fn create_then_resolve_round_trip() {
        // Arrange
        let (repo, _dir) = fresh_repo();

        // Act
        let (rec, plaintext) = repo.create("acme".into()).unwrap();
        let resolved = repo.resolve_key(&hash_key(&plaintext));

        // Assert
        assert_eq!(resolved, Some(rec.id));
        assert_eq!(rec.key_hashes.len(), 1);
        assert!(rec.key_hashes[0].starts_with("sha256:"));
    }

    #[test]
    fn resolve_unknown_key_returns_none() {
        // Arrange
        let (repo, _dir) = fresh_repo();
        repo.create("acme".into()).unwrap();

        // Act
        let resolved = repo.resolve_key("sha256:deadbeef");

        // Assert
        assert_eq!(resolved, None);
    }

    #[test]
    fn add_key_lets_old_and_new_both_resolve() {
        // Arrange
        let (repo, _dir) = fresh_repo();
        let (rec, old_plain) = repo.create("acme".into()).unwrap();

        // Act
        let new_plain = repo.add_key(rec.id).unwrap();

        // Assert
        assert_eq!(repo.resolve_key(&hash_key(&old_plain)), Some(rec.id));
        assert_eq!(repo.resolve_key(&hash_key(&new_plain)), Some(rec.id));
    }

    #[test]
    fn revoke_key_drops_the_match_and_leaves_others() {
        // Arrange
        let (repo, _dir) = fresh_repo();
        let (rec, old_plain) = repo.create("acme".into()).unwrap();
        let new_plain = repo.add_key(rec.id).unwrap();
        let old_hash = hash_key(&old_plain);

        // Act — revoke by full hash.
        repo.revoke_key(rec.id, &old_hash).unwrap();

        // Assert
        assert_eq!(repo.resolve_key(&old_hash), None);
        assert_eq!(repo.resolve_key(&hash_key(&new_plain)), Some(rec.id));
    }

    #[test]
    fn revoke_last_key_is_refused() {
        // Arrange
        let (repo, _dir) = fresh_repo();
        let (rec, only_plain) = repo.create("acme".into()).unwrap();

        // Act
        let result = repo.revoke_key(rec.id, &hash_key(&only_plain));

        // Assert
        assert!(matches!(result, Err(TenantError::LastKey(_))));
    }

    #[test]
    fn revoke_with_unmatched_prefix_errors() {
        // Arrange
        let (repo, _dir) = fresh_repo();
        let (rec, _) = repo.create("acme".into()).unwrap();

        // Act
        let result = repo.revoke_key(rec.id, "sha256:ffffffff");

        // Assert
        assert!(matches!(result, Err(TenantError::NoMatchingKey { .. })));
    }

    #[test]
    fn persisted_across_reopens() {
        // Arrange
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("tenants.json");
        let (rec, plain) = {
            let repo = FileBackedTenantRepository::open(&path).unwrap();
            repo.create("acme".into()).unwrap()
        };

        // Act — reopen the file in a fresh repo.
        let repo2 = FileBackedTenantRepository::open(&path).unwrap();

        // Assert
        assert_eq!(repo2.resolve_key(&hash_key(&plain)), Some(rec.id));
        assert_eq!(repo2.list().unwrap().len(), 1);
    }
}
