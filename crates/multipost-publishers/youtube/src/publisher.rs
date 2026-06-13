//! `Publisher` implementation for YouTube.
//!
//! Ports `scripts/youtube/{05*,06,08,10,11}.py` into Rust. Specifically:
//!   - `refresh_credentials` mirrors common.py's get_access_token() cache logic
//!   - `check_auth` mirrors 06_my_channel.py
//!   - `publish` mirrors 08_upload_video.py (resumable upload)
//!   - `confirm` mirrors the status-polling pattern from 02_video_info.py
//!   - `delete` mirrors 11_delete_video.py

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;

use multipost_core::{
    AuthStatus, Capabilities, ConfirmStatus, Content, ContentKind, MediaPayload, Platform,
    PublishContext, PublishError, PublishHandle, Publisher, Result, Visibility,
};

use crate::auth::{OAuthCredentials, OAuthTokens, refresh_token as do_refresh};
use crate::{API_BASE, studio};

const UPLOAD_BASE: &str = "https://www.googleapis.com/upload/youtube/v3";
const REFRESH_MARGIN_SECS: i64 = 60;

/// YouTube `Publisher` implementation.
///
/// Stateless; one instance can serve many accounts. The per-account credentials
/// live in `PublishContext::credentials`. OAuth client credentials are held
/// here because they're shared across all accounts of this platform.
pub struct YouTubePublisher {
    http: reqwest::Client,
    oauth: Option<OAuthCredentials>,
}

impl YouTubePublisher {
    /// Construct a new publisher.
    pub fn new(http: reqwest::Client, oauth: Option<OAuthCredentials>) -> Self {
        Self { http, oauth }
    }

    fn oauth(&self) -> Result<&OAuthCredentials> {
        self.oauth.as_ref().ok_or_else(|| {
            PublishError::AuthExpired("youtube oauth not configured for Data API credentials")
        })
    }

    async fn upload_thumbnail(
        &self,
        access_token: &str,
        video_id: &str,
        thumbnail: &MediaPayload,
    ) -> Result<()> {
        let resp = self
            .http
            .post(format!("{UPLOAD_BASE}/thumbnails/set"))
            .query(&[("videoId", video_id)])
            .bearer_auth(access_token)
            .header("Content-Type", &thumbnail.mime_type)
            .header("Content-Length", thumbnail.bytes.len().to_string())
            .body(thumbnail.bytes.clone())
            .send()
            .await
            .map_err(|e| PublishError::Transient(format!("youtube thumbnail upload: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(PublishError::Other(anyhow::anyhow!(
                "youtube thumbnail upload HTTP {status}: {body}"
            )));
        }
        tracing::info!(video_id = %video_id, filename = %thumbnail.filename, "youtube: thumbnail uploaded");
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct ChannelListResponse {
    items: Option<Vec<ChannelItem>>,
}

#[derive(Debug, Deserialize)]
struct ChannelItem {
    id: String,
    snippet: Option<ChannelSnippet>,
}

#[derive(Debug, Deserialize)]
struct ChannelSnippet {
    title: String,
    #[serde(rename = "customUrl")]
    custom_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct VideoResource {
    id: String,
    status: Option<VideoStatus>,
    #[serde(rename = "processingDetails")]
    processing_details: Option<ProcessingDetails>,
}

#[derive(Debug, Deserialize)]
struct VideoStatus {
    #[serde(rename = "uploadStatus")]
    upload_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProcessingDetails {
    #[serde(rename = "processingStatus")]
    processing_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct VideoListResponse {
    items: Option<Vec<VideoResource>>,
}

/// Result of checking a YouTube watch page from an unauthenticated client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicVideoVerification {
    /// YouTube video id being verified.
    pub video_id: String,
    /// Canonical public watch URL.
    pub url: String,
    /// Whether YouTube reports the watch page as playable.
    pub playable: bool,
    /// Whether the page explicitly marks the video as private.
    pub is_private: Option<bool>,
    /// Raw YouTube `playabilityStatus.status`, when present.
    pub playability_status: Option<String>,
    /// Watch-page title, when present.
    pub title: Option<String>,
    /// Watch-page owner channel name, when present.
    pub owner_channel_name: Option<String>,
    /// Human-readable reason when the video is not publicly playable.
    pub reason: Option<String>,
}

impl PublicVideoVerification {
    /// Return true only when an anonymous watch-page request proves public playback.
    pub fn is_publicly_available(&self) -> bool {
        self.playable && self.is_private == Some(false)
    }
}

/// Verify a YouTube video using the unauthenticated public watch page.
pub async fn verify_public_video(
    http: &reqwest::Client,
    video_id: &str,
) -> Result<PublicVideoVerification> {
    if video_id.trim().is_empty() {
        return Err(PublishError::Other(anyhow::anyhow!(
            "youtube verify requires a video id"
        )));
    }
    let url = format!("https://www.youtube.com/watch?v={video_id}");
    let resp = http
        .get(&url)
        .header(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (X11; Linux x86_64) multipost/0.1",
        )
        .send()
        .await
        .map_err(|e| PublishError::Transient(format!("youtube public verify: {e}")))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| PublishError::Transient(format!("read youtube verify body: {e}")))?;
    if !status.is_success() {
        return Ok(PublicVideoVerification {
            video_id: video_id.to_string(),
            url,
            playable: false,
            is_private: None,
            playability_status: None,
            title: None,
            owner_channel_name: None,
            reason: Some(format!("HTTP {status}")),
        });
    }
    let playability_status = extract_after(&body, r#""playabilityStatus":{"status":""#);
    let is_private = if body.contains(r#""isPrivate":false"#) {
        Some(false)
    } else if body.contains(r#""isPrivate":true"#) {
        Some(true)
    } else {
        None
    };
    let playable = playability_status.as_deref() == Some("OK");
    let reason = if playable && is_private == Some(false) {
        None
    } else if body.contains("Private video") {
        Some("private video".to_string())
    } else {
        playability_status
            .as_ref()
            .map(|s| format!("playabilityStatus={s}"))
            .or_else(|| Some("public watch page did not expose playable status".to_string()))
    };
    Ok(PublicVideoVerification {
        video_id: video_id.to_string(),
        url,
        playable,
        is_private,
        playability_status,
        title: extract_title_tag(&body),
        owner_channel_name: extract_after(&body, r#""ownerChannelName":""#),
        reason,
    })
}

fn extract_after(body: &str, marker: &str) -> Option<String> {
    let rest = body.split_once(marker)?.1;
    let raw: String = rest
        .chars()
        .take_while(|c| *c != '"' && *c != '\n' && *c != '\r')
        .collect();
    if raw.is_empty() {
        None
    } else {
        Some(raw.replace("\\u0026", "&"))
    }
}

fn extract_title_tag(body: &str) -> Option<String> {
    let start = body.find("<title>")? + "<title>".len();
    let end = body[start..].find("</title>")? + start;
    Some(
        body[start..end]
            .replace(" - YouTube", "")
            .replace("&amp;", "&")
            .replace("&#39;", "'")
            .replace("&quot;", "\""),
    )
    .filter(|s| !s.is_empty())
}

fn read_access_token(creds: &serde_json::Value) -> Result<&str> {
    creds
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| PublishError::Other(anyhow::anyhow!("credentials missing access_token")))
}

fn read_refresh_token(creds: &serde_json::Value) -> Result<&str> {
    creds
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| PublishError::AuthExpired("youtube (no refresh_token to renew with)"))
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn visibility_to_privacy(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "public",
        Visibility::Followers => "unlisted", // YouTube has no "followers-only"; closest is unlisted
        Visibility::Unlisted => "unlisted",
        Visibility::Private => "private",
    }
}

fn extract_title(content: &Content) -> &str {
    if !content.text.is_empty() {
        // Title-only mode: first line of `text` is title, rest is description.
        // We keep the simple split here; callers wanting different mapping can
        // set platform-specific overrides.
        content.text.lines().next().unwrap_or(&content.text)
    } else {
        "(untitled)"
    }
}

fn extract_description(content: &Content) -> &str {
    if let Some(idx) = content.text.find('\n') {
        // Skip the title line and trim leading whitespace.
        content.text[idx + 1..].trim_start()
    } else {
        ""
    }
}

fn select_video_payload(media: &[MediaPayload]) -> Result<&MediaPayload> {
    let video = media
        .iter()
        .find(|payload| payload.mime_type.starts_with("video/"))
        .ok_or_else(|| PublishError::Rejected("YouTube needs a video media payload".into()))?;
    if video.bytes.is_empty() {
        return Err(PublishError::Rejected("video payload is empty".into()));
    }
    Ok(video)
}

fn select_thumbnail_payload(media: &[MediaPayload]) -> Option<&MediaPayload> {
    media
        .iter()
        .find(|payload| payload.mime_type.starts_with("image/") && !payload.bytes.is_empty())
}

#[async_trait]
impl Publisher for YouTubePublisher {
    fn platform(&self) -> Platform {
        Platform::YouTube
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            max_text_chars: Some(5000), // YouTube description limit
            max_images: Some(1),        // custom thumbnail
            video_supported: true,
            video_max_seconds: Some(12 * 3600),
            schedule_supported: true,
            edit_supported: true,
            delete_supported: true,
        }
    }

    async fn refresh_credentials(
        &self,
        credentials: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        if studio::StudioCredentials::is_studio(credentials) {
            return Ok(None);
        }
        let expires_at = credentials
            .get("expires_at")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if expires_at > now_secs() + REFRESH_MARGIN_SECS {
            return Ok(None);
        }
        let refresh = read_refresh_token(credentials)?;
        let oauth = self.oauth()?;
        let new_tokens: OAuthTokens = do_refresh(&self.http, oauth, refresh)
            .await
            .map_err(|e| PublishError::Other(anyhow::anyhow!("refresh failed: {e}")))?;
        let scope = credentials
            .get("scope")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::String(new_tokens.scope.clone()));
        Ok(Some(serde_json::json!({
            "access_token": new_tokens.access_token,
            "refresh_token": new_tokens.refresh_token.unwrap_or_else(|| refresh.to_string()),
            "expires_at": new_tokens.expires_at,
            "scope": scope,
        })))
    }

    async fn check_auth(&self, ctx: &PublishContext<'_>) -> Result<AuthStatus> {
        if studio::StudioCredentials::is_studio(ctx.credentials) {
            let creds = studio::parse_credentials(ctx.credentials)?;
            return studio::check_auth(&creds).await;
        }
        let access_token = read_access_token(ctx.credentials)?;

        let resp = self
            .http
            .get(format!("{API_BASE}/channels"))
            .query(&[("part", "snippet"), ("mine", "true")])
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| PublishError::Transient(format!("YouTube /channels: {e}")))?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Ok(AuthStatus::Expired);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(PublishError::Other(anyhow::anyhow!(
                "YouTube /channels HTTP {status}: {body}"
            )));
        }

        let body: ChannelListResponse = resp
            .json()
            .await
            .map_err(|e| PublishError::Other(anyhow::anyhow!("parse /channels: {e}")))?;
        match body.items.as_deref() {
            Some([first, ..]) => {
                if let Some(s) = &first.snippet {
                    tracing::debug!(channel_id = %first.id, title = %s.title,
                                    handle = ?s.custom_url, "youtube /channels?mine=true ok");
                }
                Ok(AuthStatus::Active)
            }
            _ => Ok(AuthStatus::Pending),
        }
    }

    async fn publish(
        &self,
        ctx: &mut PublishContext<'_>,
        content: &Content,
    ) -> Result<PublishHandle> {
        // YouTube requires exactly one video payload.
        if !matches!(
            content.kind,
            ContentKind::ShortVideo | ContentKind::LongVideo
        ) {
            return Err(PublishError::Rejected(format!(
                "YouTube requires a video; got kind={:?}",
                content.kind
            )));
        }
        let video = select_video_payload(&ctx.media)?;
        let thumbnail = select_thumbnail_payload(&ctx.media);
        if studio::StudioCredentials::is_studio(ctx.credentials) {
            let creds = studio::parse_credentials(ctx.credentials)?;
            return studio::publish(&creds, content, video, thumbnail).await;
        }
        let access_token = read_access_token(ctx.credentials)?;

        let title = extract_title(content);
        let description = extract_description(content);
        let metadata = serde_json::json!({
            "snippet": {
                "title": title,
                "description": description,
                "tags": content.hashtags,
                "categoryId": "22", // People & Blogs; TODO(phase-1c): configurable
            },
            "status": {
                "privacyStatus": visibility_to_privacy(content.visibility),
                "selfDeclaredMadeForKids": false,
                "embeddable": true,
            },
        });

        tracing::info!(
            title,
            bytes = video.bytes.len(),
            mime = %video.mime_type,
            "youtube: starting resumable upload"
        );

        // Phase 1: initialize resumable upload.
        let init = self
            .http
            .post(format!("{UPLOAD_BASE}/videos"))
            .query(&[("uploadType", "resumable"), ("part", "snippet,status")])
            .bearer_auth(access_token)
            .header("Content-Type", "application/json; charset=UTF-8")
            .header("X-Upload-Content-Type", &video.mime_type)
            .header("X-Upload-Content-Length", video.bytes.len().to_string())
            .body(
                serde_json::to_vec(&metadata)
                    .map_err(|e| PublishError::Other(anyhow::anyhow!("serialize metadata: {e}")))?,
            )
            .send()
            .await
            .map_err(|e| PublishError::Transient(format!("youtube init upload: {e}")))?;
        if !init.status().is_success() {
            let status = init.status();
            let body = init.text().await.unwrap_or_default();
            return Err(PublishError::Other(anyhow::anyhow!(
                "youtube init upload HTTP {status}: {body}"
            )));
        }
        let upload_url = init
            .headers()
            .get("location")
            .and_then(|h| h.to_str().ok())
            .ok_or_else(|| {
                PublishError::Other(anyhow::anyhow!("init response missing Location header"))
            })?
            .to_string();

        // Phase 2: single-shot PUT of the video bytes.
        // TODO(phase-5): chunked upload with Content-Range for >100MB videos
        // and resume-on-failure.
        let put = self
            .http
            .put(&upload_url)
            .header("Content-Type", &video.mime_type)
            .header("Content-Length", video.bytes.len().to_string())
            .body(video.bytes.clone())
            .send()
            .await
            .map_err(|e| PublishError::Transient(format!("youtube upload PUT: {e}")))?;
        if !put.status().is_success() {
            let status = put.status();
            let body = put.text().await.unwrap_or_default();
            return Err(PublishError::Other(anyhow::anyhow!(
                "youtube upload PUT HTTP {status}: {body}"
            )));
        }
        let video_resource: serde_json::Value = put
            .json()
            .await
            .map_err(|e| PublishError::Other(anyhow::anyhow!("parse upload response: {e}")))?;
        let video_id = video_resource
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                PublishError::Other(anyhow::anyhow!(
                    "upload response missing video id: {video_resource}"
                ))
            })?
            .to_string();

        tracing::info!(video_id = %video_id, "youtube: upload complete");
        if let Some(thumbnail) = thumbnail {
            self.upload_thumbnail(access_token, &video_id, thumbnail)
                .await?;
        }
        let permalink = Some(format!("https://youtu.be/{video_id}"));
        Ok(PublishHandle {
            external_id: video_id,
            permalink,
        })
    }

    async fn confirm(
        &self,
        ctx: &PublishContext<'_>,
        handle: &PublishHandle,
    ) -> Result<ConfirmStatus> {
        if studio::StudioCredentials::is_studio(ctx.credentials) {
            if handle.external_id.is_empty() || handle.permalink.is_none() {
                return Err(PublishError::Other(anyhow::anyhow!(
                    "youtube studio publish did not return a concrete video id/permalink"
                )));
            }
            let verification = verify_public_video(&self.http, &handle.external_id).await?;
            if verification.is_publicly_available() {
                return Ok(ConfirmStatus::Confirmed {
                    permalink: Some(verification.url),
                });
            }
            tracing::info!(
                video_id = %handle.external_id,
                playable = verification.playable,
                is_private = ?verification.is_private,
                status = ?verification.playability_status,
                reason = ?verification.reason,
                "youtube studio public verify pending"
            );
            return Ok(ConfirmStatus::Pending);
        }
        let access_token = read_access_token(ctx.credentials)?;
        let resp = self
            .http
            .get(format!("{API_BASE}/videos"))
            .query(&[
                ("part", "status,processingDetails"),
                ("id", handle.external_id.as_str()),
            ])
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| PublishError::Transient(format!("youtube confirm: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(PublishError::Other(anyhow::anyhow!(
                "youtube /videos confirm HTTP {status}: {body}"
            )));
        }
        let body: VideoListResponse = resp
            .json()
            .await
            .map_err(|e| PublishError::Other(anyhow::anyhow!("parse confirm: {e}")))?;
        let item = body
            .items
            .as_deref()
            .and_then(|a| a.first())
            .ok_or_else(|| {
                PublishError::Other(anyhow::anyhow!(
                    "video {} disappeared between publish and confirm",
                    handle.external_id
                ))
            })?;

        let upload = item
            .status
            .as_ref()
            .and_then(|s| s.upload_status.as_deref())
            .unwrap_or("");
        let processing = item
            .processing_details
            .as_ref()
            .and_then(|p| p.processing_status.as_deref())
            .unwrap_or("");

        if upload == "failed" || processing == "failed" || processing == "terminated" {
            return Err(PublishError::Rejected(format!(
                "youtube processing failed (upload={upload}, processing={processing})"
            )));
        }
        if upload == "processed" && processing == "succeeded" {
            return Ok(ConfirmStatus::Confirmed {
                permalink: Some(format!("https://youtu.be/{}", item.id)),
            });
        }
        Ok(ConfirmStatus::Pending)
    }

    async fn delete(&self, ctx: &PublishContext<'_>, handle: &PublishHandle) -> Result<()> {
        if studio::StudioCredentials::is_studio(ctx.credentials) {
            return Err(PublishError::Other(anyhow::anyhow!(
                "youtube studio delete is not implemented; delete {} manually in YouTube Studio",
                handle
                    .permalink
                    .as_deref()
                    .unwrap_or(handle.external_id.as_str())
            )));
        }
        let access_token = read_access_token(ctx.credentials)?;
        let resp = self
            .http
            .delete(format!("{API_BASE}/videos"))
            .query(&[("id", handle.external_id.as_str())])
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| PublishError::Transient(format!("youtube delete: {e}")))?;
        if resp.status() == reqwest::StatusCode::NO_CONTENT
            || resp.status() == reqwest::StatusCode::OK
        {
            tracing::info!(video_id = %handle.external_id, "youtube: deleted");
            return Ok(());
        }
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(PublishError::Other(anyhow::anyhow!(
            "youtube delete HTTP {status}: {body}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_oauth() -> OAuthCredentials {
        OAuthCredentials {
            client_id: "id".into(),
            client_secret: "s".into(),
            redirect_uri: "http://localhost".into(),
        }
    }

    #[test]
    fn extract_title_first_line() {
        let c = Content {
            id: uuid::Uuid::nil(),
            user_id: uuid::Uuid::nil(),
            kind: ContentKind::LongVideo,
            text: "My Title\n\nLine 1 of desc\nLine 2".into(),
            hashtags: vec![],
            media: vec![],
            schedule_at: None,
            visibility: Visibility::Private,
            overrides: Default::default(),
            created_at: chrono::Utc::now(),
        };
        assert_eq!(extract_title(&c), "My Title");
        assert_eq!(extract_description(&c), "Line 1 of desc\nLine 2");
    }

    #[test]
    fn extract_title_no_newline() {
        let c = Content {
            id: uuid::Uuid::nil(),
            user_id: uuid::Uuid::nil(),
            kind: ContentKind::LongVideo,
            text: "Only line".into(),
            hashtags: vec![],
            media: vec![],
            schedule_at: None,
            visibility: Visibility::Private,
            overrides: Default::default(),
            created_at: chrono::Utc::now(),
        };
        assert_eq!(extract_title(&c), "Only line");
        assert_eq!(extract_description(&c), "");
    }

    #[test]
    fn visibility_mapping() {
        assert_eq!(visibility_to_privacy(Visibility::Public), "public");
        assert_eq!(visibility_to_privacy(Visibility::Private), "private");
        assert_eq!(visibility_to_privacy(Visibility::Unlisted), "unlisted");
        assert_eq!(visibility_to_privacy(Visibility::Followers), "unlisted");
    }

    #[test]
    fn selects_video_and_thumbnail_payloads_by_mime_type() {
        let payloads = vec![
            MediaPayload {
                filename: "notes.txt".into(),
                mime_type: "text/plain".into(),
                bytes: b"ignored".to_vec(),
            },
            MediaPayload {
                filename: "clip.mp4".into(),
                mime_type: "video/mp4".into(),
                bytes: vec![1, 2, 3],
            },
            MediaPayload {
                filename: "cover.jpg".into(),
                mime_type: "image/jpeg".into(),
                bytes: vec![4, 5, 6],
            },
        ];

        assert_eq!(
            select_video_payload(&payloads).unwrap().filename,
            "clip.mp4"
        );
        assert_eq!(
            select_thumbnail_payload(&payloads).unwrap().filename,
            "cover.jpg"
        );
    }

    #[test]
    fn rejects_missing_video_payload() {
        let payloads = vec![MediaPayload {
            filename: "cover.jpg".into(),
            mime_type: "image/jpeg".into(),
            bytes: vec![1],
        }];

        assert!(select_video_payload(&payloads).is_err());
    }

    #[test]
    fn dummy_constructs() {
        let _ = dummy_oauth();
    }
}
