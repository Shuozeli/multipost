//! `Publisher` implementation for Bilibili.
//!
//! - `check_auth`: REST probe via `api.bilibili.com/x/space/myinfo` using
//!   cached cookies.
//! - `publish`: upload video via Bilibili's chunked REST API, then submit
//!   with metadata (title, description, tags, category).
//! - `confirm`: Bilibili videos go through a review pipeline; we report
//!   `Confirmed` immediately (the BV ID is assigned at submit time).

use async_trait::async_trait;
use multipost_core::{
    AuthStatus, Capabilities, ConfirmStatus, Content, Platform, PublishContext, PublishError,
    PublishHandle, Publisher, Result,
};

use crate::api::{self, DEFAULT_TID, VideoMeta};
use crate::credentials::BilibiliCredentials;

/// Bilibili publisher. Stateless — all per-account config lives in
/// `PublishContext::credentials`.
pub struct BilibiliPublisher;

impl BilibiliPublisher {
    /// Construct a new publisher.
    pub fn new() -> Self {
        Self
    }
}

impl Default for BilibiliPublisher {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_credentials(value: &serde_json::Value) -> Result<BilibiliCredentials> {
    serde_json::from_value::<BilibiliCredentials>(value.clone()).map_err(|e| {
        PublishError::Other(anyhow::anyhow!(
            "credentials don't deserialize as BilibiliCredentials: {e}"
        ))
    })
}

#[async_trait]
impl Publisher for BilibiliPublisher {
    fn platform(&self) -> Platform {
        Platform::Bilibili
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            max_text_chars: None,
            max_images: None,
            video_supported: true,
            // Bilibili allows up to 10 hours for most users.
            video_max_seconds: Some(36000),
            schedule_supported: false,
            edit_supported: false,
            delete_supported: false,
        }
    }

    async fn refresh_credentials(
        &self,
        _credentials: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        // Cookie-based auth — no refresh mechanism.
        Ok(None)
    }

    async fn check_auth(&self, ctx: &PublishContext<'_>) -> Result<AuthStatus> {
        let creds = parse_credentials(ctx.credentials)?;
        if !creds.has_cookies() {
            return Ok(AuthStatus::Pending);
        }
        match api::check_session(&creds).await {
            Ok(Some(_)) => Ok(AuthStatus::Active),
            Ok(None) => Ok(AuthStatus::Expired),
            Err(e) => Err(PublishError::Transient(format!(
                "bilibili session check: {e}"
            ))),
        }
    }

    async fn publish(
        &self,
        ctx: &mut PublishContext<'_>,
        content: &Content,
    ) -> Result<PublishHandle> {
        let creds = parse_credentials(ctx.credentials)?;
        if !creds.has_cookies() {
            return Err(PublishError::Rejected(
                "bilibili: no cookies in credentials — re-register the account".into(),
            ));
        }

        // We need exactly one video.
        if ctx.media.is_empty() {
            return Err(PublishError::Rejected(
                "bilibili: video upload requires a media attachment".into(),
            ));
        }

        let video = &ctx.media[0];
        let video_bytes = &video.bytes;
        let video_filename = if video.filename.is_empty() {
            "video.mp4"
        } else {
            &video.filename
        };

        // First line of `text` is the title, rest is description (same
        // convention as the YouTube publisher).
        let (title, desc) = match content.text.find('\n') {
            Some(idx) => (
                truncate(&content.text[..idx], 80),
                content.text[idx + 1..].trim_start(),
            ),
            None => (truncate(&content.text, 80), content.text.as_str()),
        };
        let tags = if content.hashtags.is_empty() {
            "视频".to_string()
        } else {
            content.hashtags.join(",")
        };

        let meta = VideoMeta {
            title: title.to_string(),
            desc: desc.to_string(),
            tag: tags,
            tid: DEFAULT_TID,
            copyright: 1, // original
            source: String::new(),
        };

        let result = api::upload_and_submit(&creds, video_bytes, video_filename, &meta, None)
            .await
            .map_err(PublishError::Other)?;

        Ok(PublishHandle {
            external_id: result.bvid.clone(),
            permalink: Some(format!("https://www.bilibili.com/video/{}", result.bvid)),
        })
    }

    async fn confirm(
        &self,
        _ctx: &PublishContext<'_>,
        handle: &PublishHandle,
    ) -> Result<ConfirmStatus> {
        // Bilibili assigns the BV ID at submit time. The video enters a
        // review pipeline, but we treat it as confirmed since we have the
        // permanent URL.
        Ok(ConfirmStatus::Confirmed {
            permalink: handle.permalink.clone(),
        })
    }

    async fn delete(&self, _ctx: &PublishContext<'_>, _handle: &PublishHandle) -> Result<()> {
        Err(PublishError::Rejected(
            "bilibili: delete not implemented (use the creator center manually)".into(),
        ))
    }
}

/// Truncate a string to at most `max` chars (not bytes).
fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}
