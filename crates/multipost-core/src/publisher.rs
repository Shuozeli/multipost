//! The `Publisher` trait every platform integration implements.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::content::Content;
use crate::error::Result;
use crate::platform::{AuthStatus, Capabilities, Platform};

/// Opaque handle to a published post on a platform.
///
/// For API platforms this wraps the platform's video/post ID.
/// For automation platforms it wraps whatever URL/ID we scraped after the upload UI returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishHandle {
    /// Platform-specific external identifier (e.g. YouTube video ID).
    pub external_id: String,
    /// User-facing permalink, if we have one.
    pub permalink: Option<String>,
}

/// Status returned by `Publisher::confirm` during status polling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfirmStatus {
    /// Platform finished processing; post is live.
    Confirmed {
        /// Final permalink, if the platform gave us one.
        permalink: Option<String>,
    },
    /// Still being processed; orchestrator should poll again later.
    Pending,
}

/// A media asset resolved into in-memory bytes for publishing.
///
/// The orchestrator pre-resolves [`Content::media`] refs into these
/// before invoking [`Publisher::publish`], so publishers don't need to
/// know about the storage layer.
#[derive(Debug, Clone)]
pub struct MediaPayload {
    /// Source filename (informational; used as the upload `filename`).
    pub filename: String,
    /// MIME type, e.g. `video/mp4`.
    pub mime_type: String,
    /// Raw bytes.
    pub bytes: Vec<u8>,
}

/// Per-call context passed to `Publisher::publish`.
///
/// Carries the account being posted from and any pre-resolved media.
pub struct PublishContext<'a> {
    /// Account row ID.
    pub account_id: Uuid,
    /// User_id (multi-tenant scope).
    pub user_id: Uuid,
    /// Decrypted account credentials JSON. Caller should run
    /// [`Publisher::refresh_credentials`] before each publish and
    /// persist any new value before constructing this context.
    pub credentials: &'a serde_json::Value,
    /// Media payloads pre-resolved from `Content.media[]`. Same order
    /// as `Content.media`.
    pub media: Vec<MediaPayload>,
}

/// What every platform integration implements.
///
/// All methods are async. Errors classify themselves via [`PublishError`].
#[async_trait]
pub trait Publisher: Send + Sync + 'static {
    /// Which platform this publisher targets.
    fn platform(&self) -> Platform;

    /// What this publisher accepts. Called at job-validation time.
    fn capabilities(&self) -> Capabilities;

    /// Refresh credentials if they're close to expiry.
    ///
    /// Returns:
    ///   - `Ok(Some(new_credentials_json))` if refresh happened; caller persists
    ///   - `Ok(None)` if no refresh was needed
    ///   - `Err(AuthExpired)` if refresh isn't possible (e.g. revoked refresh token)
    ///
    /// Idempotent; safe to call before every operation.
    async fn refresh_credentials(
        &self,
        credentials: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>>;

    /// Quick check: are the credentials still usable?
    ///
    /// Called by the orchestrator before publishing and by the periodic
    /// health-check job. Should be cheap (single API call or cookie ping).
    async fn check_auth(&self, ctx: &PublishContext<'_>) -> Result<AuthStatus>;

    /// Publish a piece of content. May be async on the platform side;
    /// returned handle is used by [`Publisher::confirm`] to poll status.
    async fn publish(
        &self,
        ctx: &mut PublishContext<'_>,
        content: &Content,
    ) -> Result<PublishHandle>;

    /// Poll the status of a previously-published post. Used for platforms
    /// where publish is async (e.g. WeChat MP `freepublish/submit`).
    ///
    /// Returns:
    ///   - `Ok(ConfirmStatus::Confirmed { permalink })` — done, post is live
    ///   - `Ok(ConfirmStatus::Pending)`                — still processing, keep polling
    ///   - `Err(...)`                                  — terminal failure
    async fn confirm(
        &self,
        ctx: &PublishContext<'_>,
        handle: &PublishHandle,
    ) -> Result<ConfirmStatus>;

    /// Delete a previously-published post. Best-effort.
    async fn delete(&self, ctx: &PublishContext<'_>, handle: &PublishHandle) -> Result<()>;
}
