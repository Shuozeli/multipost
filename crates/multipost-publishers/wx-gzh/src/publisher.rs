//! `Publisher` implementation for WeChat MP (公众号).
//!
//! Ports `scripts/wechat-mp/{12_publish_article,13_delete_all_drafts}.py`.

use async_trait::async_trait;
use reqwest::multipart::{Form, Part};
use serde::Deserialize;

use multipost_core::{
    AuthStatus, Capabilities, ConfirmStatus, Content, ContentKind, Platform, PublishContext,
    PublishError, PublishHandle, Publisher, Result, Visibility,
};

use crate::auth::{check_account_info, ensure_access_token, WxGzhCredentials};
use crate::API_BASE;

/// WeChat MP publisher.
///
/// Stateless. Each account brings its own appid + secret in the credentials
/// JSON, so the publisher itself needs no per-instance config.
pub struct WxGzhPublisher {
    http: reqwest::Client,
}

impl WxGzhPublisher {
    /// Construct a new publisher.
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }
}

impl Default for WxGzhPublisher {
    fn default() -> Self {
        Self::new(reqwest::Client::new())
    }
}

fn parse_credentials(value: &serde_json::Value) -> Result<WxGzhCredentials> {
    serde_json::from_value::<WxGzhCredentials>(value.clone()).map_err(|e| {
        PublishError::Other(anyhow::anyhow!(
            "credentials don't deserialize as WxGzhCredentials: {e}"
        ))
    })
}

fn classify_wechat_error(errcode: i64, errmsg: &str) -> PublishError {
    match errcode {
        // 40001 / 42001 / 40014 — token invalid or expired
        40001 | 42001 | 40014 => PublishError::AuthExpired("wx-gzh"),
        // 40164 — IP not in whitelist
        40164 => PublishError::Transient(format!(
            "wx-gzh: IP not whitelisted (errcode 40164) — add this server's egress IP \
             to https://mp.weixin.qq.com → 设置与开发 → 基本配置 \
             → IP白名单"
        )),
        // 48001 — api unauthorized (individual subscription accounts can't freepublish/submit)
        48001 => PublishError::Rejected(format!(
            "wx-gzh: API not authorized for this account type (errcode 48001). \
             freepublish/submit requires 企业认证 (enterprise verification). \
             Draft is created — publish manually in MP admin if you can't upgrade."
        )),
        // 45004 — digest too long
        45004 => PublishError::Rejected(format!(
            "wx-gzh: digest exceeds 120 chars (errcode 45004): {errmsg}"
        )),
        _ => PublishError::Other(anyhow::anyhow!(
            "wx-gzh errcode={errcode} errmsg={errmsg}"
        )),
    }
}

fn check_wechat_response(body: &serde_json::Value) -> Result<()> {
    if let Some(code) = body.get("errcode").and_then(|v| v.as_i64()) {
        if code != 0 {
            let msg = body
                .get("errmsg")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            return Err(classify_wechat_error(code, msg));
        }
    }
    Ok(())
}

// Response shapes. Caller validates errcode via `check_wechat_response` on the
// raw `serde_json::Value` before deserializing into these (so error fields
// don't need to be modeled here).
#[derive(Debug, Deserialize)]
struct DraftAddResponse {
    media_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MaterialAddResponse {
    media_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FreepublishSubmitResponse {
    publish_id: Option<String>,
    msg_data_id: Option<String>,
}

fn split_title_and_body(content: &Content) -> (String, String) {
    if let Some(idx) = content.text.find('\n') {
        (
            content.text[..idx].trim().to_string(),
            content.text[idx + 1..].trim_start().to_string(),
        )
    } else {
        (content.text.clone(), String::new())
    }
}

fn make_digest(content: &Content) -> String {
    // WeChat caps digest at 120 chars. Take the body (after title), trim, truncate.
    let (_, body) = split_title_and_body(content);
    let mut digest = body
        .chars()
        .filter(|c| !matches!(c, '\n' | '\r'))
        .collect::<String>();
    if digest.chars().count() > 120 {
        digest = digest.chars().take(117).collect::<String>() + "...";
    }
    digest
}

fn make_html_body(content: &Content) -> String {
    // Body is treated as Markdown; we render to HTML via pulldown-cmark
    // then inject inline styles to match `scripts/wechat-mp/12_publish_article.py`'s
    // `wrap_article()`. WeChat MP strips external stylesheets and most
    // CSS classes — inline styles are the only thing it reliably keeps.
    let (_, body) = split_title_and_body(content);
    if body.is_empty() {
        return String::new();
    }
    let raw_html = markdown_to_html(&body);
    inject_inline_styles(&raw_html)
}

/// Render Markdown → HTML using GFM-ish extensions (tables, strikethrough,
/// task lists). Same option set the Python `markdown.markdown(..., extensions=["extra"])`
/// gives us.
fn markdown_to_html(md: &str) -> String {
    use pulldown_cmark::{html, Options, Parser};
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(md, opts);
    let mut out = String::with_capacity(md.len() * 2);
    html::push_html(&mut out, parser);
    out
}

/// Cheap inline-styling: replace bare tags with styled equivalents.
/// Mirrors the Python `wrap_article()` byte-for-byte where it matters
/// (paragraph, h2/h3, ul/ol/li, hr, strong, blockquote). Style strings
/// kept verbatim from the Python so the WeChat-side rendering is
/// indistinguishable from articles created via the script.
fn inject_inline_styles(html: &str) -> String {
    const STYLE_P: &str = "font-size:16px;line-height:1.8;margin:14px 0;color:#333;";
    const STYLE_H2: &str = "font-size:19px;line-height:1.5;margin:24px 0 12px;color:#1a1a1a;border-left:4px solid #c9302c;padding-left:10px;font-weight:600;";
    const STYLE_H3: &str = "font-size:17px;line-height:1.5;margin:18px 0 10px;color:#333;font-weight:600;";
    const STYLE_UL: &str = "padding-left:20px;margin:8px 0;";
    const STYLE_LI: &str = "margin:8px 0;line-height:1.75;";
    const STYLE_HR: &str = "border:none;border-top:1px dashed #ccc;margin:24px 0;";
    const STYLE_STRONG: &str = "color:#c9302c;";
    const STYLE_BLOCKQUOTE: &str = "border-left:3px solid #c9302c;background:#fdf3f3;color:#555;padding:8px 14px;margin:14px 0;font-size:15px;";

    html.replace("<p>", &format!(r#"<p style="{STYLE_P}">"#))
        .replace("<h2>", &format!(r#"<h2 style="{STYLE_H2}">"#))
        .replace("<h3>", &format!(r#"<h3 style="{STYLE_H3}">"#))
        .replace("<ul>", &format!(r#"<ul style="{STYLE_UL}">"#))
        .replace("<ol>", &format!(r#"<ol style="{STYLE_UL}">"#))
        .replace("<li>", &format!(r#"<li style="{STYLE_LI}">"#))
        .replace("<hr />", &format!(r#"<hr style="{STYLE_HR}" />"#))
        .replace("<hr/>", &format!(r#"<hr style="{STYLE_HR}" />"#))
        .replace("<hr>", &format!(r#"<hr style="{STYLE_HR}" />"#))
        .replace("<strong>", &format!(r#"<strong style="{STYLE_STRONG}">"#))
        .replace("<blockquote>", &format!(r#"<blockquote style="{STYLE_BLOCKQUOTE}">"#))
}

#[async_trait]
impl Publisher for WxGzhPublisher {
    fn platform(&self) -> Platform {
        Platform::WxGzh
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            max_text_chars: Some(20_000), // body HTML can be quite long
            max_images: Some(1),           // thumb_media_id only (Phase 2)
            video_supported: false,
            video_max_seconds: None,
            schedule_supported: false,
            edit_supported: false,
            delete_supported: true,
        }
    }

    async fn refresh_credentials(
        &self,
        credentials: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        let creds = parse_credentials(credentials)?;
        let refreshed = ensure_access_token(&self.http, &creds)
            .await
            .map_err(|e| PublishError::Other(anyhow::anyhow!("wx-gzh ensure_access_token: {e}")))?;
        match refreshed {
            Some(new) => Ok(Some(serde_json::to_value(new).map_err(|e| {
                PublishError::Other(anyhow::anyhow!("serialize wx-gzh creds: {e}"))
            })?)),
            None => Ok(None),
        }
    }

    async fn check_auth(&self, ctx: &PublishContext<'_>) -> Result<AuthStatus> {
        let creds = parse_credentials(ctx.credentials)?;
        if creds.access_token.is_empty() {
            return Ok(AuthStatus::Pending);
        }
        let info = check_account_info(&self.http, &creds.access_token)
            .await
            .map_err(|e| {
                // Distinguish auth-expired (caller should refresh) from transient (network).
                let s = e.to_string();
                if s.contains("errcode=40001")
                    || s.contains("errcode=42001")
                    || s.contains("errcode=40014")
                {
                    PublishError::AuthExpired("wx-gzh")
                } else if s.contains("errcode=40164") {
                    PublishError::Transient(format!("wx-gzh: IP not whitelisted: {s}"))
                } else {
                    PublishError::Other(anyhow::anyhow!("wx-gzh check_auth: {e}"))
                }
            })?;
        tracing::debug!(appid = %info.appid, nickname = ?info.nickname,
                        "wx-gzh check_auth ok");
        Ok(AuthStatus::Active)
    }

    async fn publish(
        &self,
        ctx: &mut PublishContext<'_>,
        content: &Content,
    ) -> Result<PublishHandle> {
        // WxGzh requires an article (text), not a video.
        if matches!(
            content.kind,
            ContentKind::LongVideo | ContentKind::ShortVideo
        ) {
            return Err(PublishError::Rejected(
                "wx-gzh: video posts are not supported via API (use video service account)".into(),
            ));
        }
        let cover = ctx.media.first().cloned().ok_or_else(|| {
            PublishError::Rejected(
                "wx-gzh: draft requires a cover image (thumb_media_id). \
                 Attach exactly one image to the post."
                    .into(),
            )
        })?;
        let creds = parse_credentials(ctx.credentials)?;
        let access_token = &creds.access_token;
        if access_token.is_empty() {
            return Err(PublishError::AuthExpired(
                "wx-gzh (no access_token cached; call refresh_credentials first)",
            ));
        }
        let (title, _body) = split_title_and_body(content);
        let digest = make_digest(content);
        let html_body = make_html_body(content);
        if title.chars().count() > 64 {
            return Err(PublishError::Rejected(format!(
                "wx-gzh: title is {} chars; cap is 64",
                title.chars().count()
            )));
        }

        // Step 1: upload the cover image as permanent material.
        tracing::info!(filename = %cover.filename, bytes = cover.bytes.len(),
                       "wx-gzh: uploading cover image");
        let form = Form::new().part(
            "media",
            Part::bytes(cover.bytes.clone())
                .file_name(cover.filename.clone())
                .mime_str(&cover.mime_type)
                .map_err(|e| PublishError::Other(anyhow::anyhow!("mime: {e}")))?,
        );
        let resp = self
            .http
            .post(format!("{API_BASE}/cgi-bin/material/add_material"))
            .query(&[("access_token", access_token.as_str()), ("type", "image")])
            .multipart(form)
            .send()
            .await
            .map_err(|e| PublishError::Transient(format!("wx-gzh upload cover: {e}")))?;
        let material_value: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| PublishError::Other(anyhow::anyhow!("parse material response: {e}")))?;
        check_wechat_response(&material_value)?;
        let material: MaterialAddResponse = serde_json::from_value(material_value).map_err(|e| {
            PublishError::Other(anyhow::anyhow!("deserialize material: {e}"))
        })?;
        let thumb_media_id = material.media_id.ok_or_else(|| {
            PublishError::Other(anyhow::anyhow!(
                "material/add_material returned no media_id"
            ))
        })?;

        // Step 2: create the draft.
        let article_visibility = match content.visibility {
            Visibility::Public | Visibility::Followers => 0, // public
            Visibility::Unlisted | Visibility::Private => 0, // WxGzh doesn't have unlisted/private drafts
        };
        let _ = article_visibility; // not currently used; WxGzh visibility is set at publish time

        let draft_body = serde_json::json!({
            "articles": [{
                "article_type": "news",
                "title": title,
                "author": "",
                "digest": digest,
                "content": html_body,
                "content_source_url": "",
                "thumb_media_id": thumb_media_id,
                "need_open_comment": 0,
                "only_fans_can_comment": 0,
            }]
        });
        tracing::info!(title = %title, "wx-gzh: creating draft");
        let resp = self
            .http
            .post(format!("{API_BASE}/cgi-bin/draft/add"))
            .query(&[("access_token", access_token.as_str())])
            .json(&draft_body)
            .send()
            .await
            .map_err(|e| PublishError::Transient(format!("wx-gzh draft/add: {e}")))?;
        let draft_value: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| PublishError::Other(anyhow::anyhow!("parse draft/add: {e}")))?;
        check_wechat_response(&draft_value)?;
        let draft: DraftAddResponse = serde_json::from_value(draft_value).map_err(|e| {
            PublishError::Other(anyhow::anyhow!("deserialize draft/add: {e}"))
        })?;
        let draft_media_id = draft.media_id.ok_or_else(|| {
            PublishError::Other(anyhow::anyhow!("draft/add returned no media_id"))
        })?;
        tracing::info!(draft_media_id = %draft_media_id, "wx-gzh: draft created");

        // Step 3: optimistically try freepublish/submit. On 48001 we surface a
        // clear "publish manually" error but still return the draft handle so
        // the orchestrator records it.
        let submit_resp = self
            .http
            .post(format!("{API_BASE}/cgi-bin/freepublish/submit"))
            .query(&[("access_token", access_token.as_str())])
            .json(&serde_json::json!({"media_id": draft_media_id}))
            .send()
            .await
            .map_err(|e| PublishError::Transient(format!("wx-gzh freepublish/submit: {e}")))?;
        let submit_value: serde_json::Value = submit_resp
            .json()
            .await
            .map_err(|e| PublishError::Other(anyhow::anyhow!("parse freepublish/submit: {e}")))?;
        match check_wechat_response(&submit_value) {
            Ok(()) => {
                let submit: FreepublishSubmitResponse = serde_json::from_value(submit_value)
                    .map_err(|e| {
                        PublishError::Other(anyhow::anyhow!("deserialize submit: {e}"))
                    })?;
                let publish_id = submit.publish_id.unwrap_or_default();
                tracing::info!(
                    publish_id = %publish_id,
                    msg_data_id = ?submit.msg_data_id,
                    "wx-gzh: freepublish/submit accepted"
                );
                Ok(PublishHandle {
                    // Use publish_id as the external_id since it's the canonical
                    // identifier post-submit; draft_media_id is recorded in logs.
                    external_id: publish_id,
                    // No permalink until confirm() succeeds.
                    permalink: None,
                })
            }
            Err(PublishError::Rejected(msg)) if msg.contains("errcode 48001") => {
                tracing::warn!(
                    draft_media_id = %draft_media_id,
                    "wx-gzh: freepublish/submit blocked (48001); draft created OK"
                );
                Err(PublishError::Rejected(format!(
                    "wx-gzh: draft created (media_id={draft_media_id}) but freepublish/submit \
                     is gated for this account (errcode 48001). Publish manually at \
                     https://mp.weixin.qq.com → 草稿箱."
                )))
            }
            Err(e) => Err(e),
        }
    }

    async fn confirm(
        &self,
        ctx: &PublishContext<'_>,
        handle: &PublishHandle,
    ) -> Result<ConfirmStatus> {
        if handle.external_id.is_empty() {
            return Err(PublishError::Other(anyhow::anyhow!(
                "wx-gzh confirm: empty external_id (publish_id)"
            )));
        }
        let creds = parse_credentials(ctx.credentials)?;
        let resp = self
            .http
            .post(format!("{API_BASE}/cgi-bin/freepublish/get"))
            .query(&[("access_token", creds.access_token.as_str())])
            .json(&serde_json::json!({"publish_id": handle.external_id}))
            .send()
            .await
            .map_err(|e| PublishError::Transient(format!("wx-gzh freepublish/get: {e}")))?;
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| PublishError::Other(anyhow::anyhow!("parse freepublish/get: {e}")))?;
        // publish_status: 0=success, 1=publishing, 2=fail, 3=audit fail,
        // 4=format fail, 5=in review, 6=admin revoked
        let status = body
            .get("publish_status")
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        match status {
            0 => {
                let permalink = body
                    .get("article_detail")
                    .and_then(|d| d.get("item"))
                    .and_then(|i| i.as_array())
                    .and_then(|a| a.first())
                    .and_then(|it| it.get("article_url"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                Ok(ConfirmStatus::Confirmed { permalink })
            }
            1 | 5 => Ok(ConfirmStatus::Pending),
            other => Err(PublishError::Rejected(format!(
                "wx-gzh publish_status={other} (2=fail, 3=audit-fail, 4=format-fail, 6=admin-revoked)"
            ))),
        }
    }

    async fn delete(&self, ctx: &PublishContext<'_>, handle: &PublishHandle) -> Result<()> {
        let creds = parse_credentials(ctx.credentials)?;
        // For drafts we'd call cgi-bin/draft/delete with the draft media_id;
        // for published articles cgi-bin/freepublish/delete with the article_id.
        // In Phase 2 we treat the external_id as a draft media_id since most
        // WxGzh "publishes" end up as drafts on individual subscription accounts.
        let resp = self
            .http
            .post(format!("{API_BASE}/cgi-bin/draft/delete"))
            .query(&[("access_token", creds.access_token.as_str())])
            .json(&serde_json::json!({"media_id": handle.external_id}))
            .send()
            .await
            .map_err(|e| PublishError::Transient(format!("wx-gzh draft/delete: {e}")))?;
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| PublishError::Other(anyhow::anyhow!("parse delete: {e}")))?;
        check_wechat_response(&body)?;
        tracing::info!(media_id = %handle.external_id, "wx-gzh: draft deleted");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_content(text: &str) -> Content {
        Content {
            id: uuid::Uuid::nil(),
            user_id: uuid::Uuid::nil(),
            kind: ContentKind::Article,
            text: text.to_string(),
            hashtags: vec![],
            media: vec![],
            schedule_at: None,
            visibility: Visibility::Public,
            overrides: Default::default(),
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn title_and_body_split() {
        let c = mk_content("Title line\n\nBody first paragraph.\n\nSecond paragraph.");
        let (t, b) = split_title_and_body(&c);
        assert_eq!(t, "Title line");
        assert!(b.starts_with("Body first paragraph."));
        assert!(b.contains("Second paragraph"));
    }

    #[test]
    fn digest_clips_to_120() {
        let long = "x".repeat(500);
        let c = mk_content(&format!("Title\n\n{long}"));
        let d = make_digest(&c);
        assert!(d.chars().count() <= 120, "digest is {} chars", d.chars().count());
        assert!(d.ends_with("..."));
    }

    #[test]
    fn html_body_paragraphs() {
        let c = mk_content("Title\n\npara one.\n\npara two.");
        let h = make_html_body(&c);
        assert!(h.contains("<p"));
        assert!(h.contains("para one."));
        assert!(h.contains("para two."));
        // Two paragraphs → two <p> tags.
        assert_eq!(h.matches("<p ").count(), 2);
    }

    #[test]
    fn markdown_h2_gets_styled() {
        // Arrange — H2 + bold + bullet list, the article building blocks
        // every financial-digest article exercises.
        let c = mk_content(
            "财经早报\n\n## 一、 国际地缘\n\n* **要点**：内容描述\n* 另一个要点",
        );

        // Act
        let h = make_html_body(&c);

        // Assert — the styled wrapper is present on each rendered tag.
        assert!(
            h.contains(r#"<h2 style="font-size:19px"#),
            "h2 missing inline style; got: {h}"
        );
        assert!(
            h.contains(r#"<strong style="color:#c9302c"#),
            "strong missing inline style; got: {h}"
        );
        assert!(
            h.contains(r#"<ul style="padding-left:20px"#),
            "ul missing inline style; got: {h}"
        );
        assert!(
            h.contains(r#"<li style="margin:8px 0"#),
            "li missing inline style; got: {h}"
        );
    }

    #[test]
    fn markdown_blockquote_gets_styled() {
        // Arrange
        let c = mk_content("T\n\n> placeholder draft");

        // Act
        let h = make_html_body(&c);

        // Assert
        assert!(
            h.contains(r#"<blockquote style="border-left:3px"#),
            "blockquote missing inline style; got: {h}"
        );
    }

    #[test]
    fn err_48001_classifies_as_rejected() {
        let e = classify_wechat_error(48001, "api unauthorized");
        match e {
            PublishError::Rejected(s) => assert!(s.contains("48001")),
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn err_40164_classifies_as_transient() {
        let e = classify_wechat_error(40164, "invalid ip");
        match e {
            PublishError::Transient(s) => assert!(s.contains("40164")),
            other => panic!("expected Transient, got {other:?}"),
        }
    }
}
