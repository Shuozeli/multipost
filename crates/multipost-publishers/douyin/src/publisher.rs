//! `Publisher` implementation for Douyin.
//!
//! Phase 4a: REST-only `check_auth`.
//! Phase 4b (this commit): `publish` stages the video over SCP and drives
//! the creator center via CDP up to (but not including) the final 发布
//! click. `confirm` / `delete` still pending — they'll arrive once we
//! verify the publish click path against a real account.

use async_trait::async_trait;
use multipost_core::{
    AuthStatus, Capabilities, ConfirmStatus, Content, MediaPayload, Platform, PublishContext,
    PublishError, PublishHandle, Publisher, Result, Visibility,
};
use std::time::{Duration, Instant};

use crate::cdp::{BrowserSession, PageSession};
use crate::credentials::DouyinCredentials;
use crate::staging::{StagedFile, cleanup_file, stage_file};
use crate::{CREATOR_BASE, UPLOAD_URL, selectors};

/// Douyin publisher.
///
/// Stateless. Per-account configuration (specifically: the CDP HTTP endpoint
/// for the Chrome profile that's logged into this Douyin account) lives in
/// `PublishContext::credentials`.
pub struct DouyinPublisher;

impl DouyinPublisher {
    /// Construct a new publisher.
    pub fn new() -> Self {
        Self
    }
}

impl Default for DouyinPublisher {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_credentials(value: &serde_json::Value) -> Result<DouyinCredentials> {
    serde_json::from_value::<DouyinCredentials>(value.clone()).map_err(|e| {
        PublishError::Other(anyhow::anyhow!(
            "credentials don't deserialize as DouyinCredentials: {e}"
        ))
    })
}

/// Probe Douyin's auth state by creating a fresh tab at `CREATOR_BASE`
/// and reading the redirected URL via the HTTP `/json` endpoint.
///
/// REST-only path — no WebSocket — which avoids contention with any
/// stale CDP clients attached to existing creator.douyin.com tabs.
async fn probe_creator_url(cdp_url: &str) -> anyhow::Result<String> {
    use std::time::Duration;
    let session = BrowserSession::connect(cdp_url).await?;
    let new_tab = session.create_tab(CREATOR_BASE).await?;
    tracing::debug!(target_id = %new_tab.id, "douyin: created probe tab");

    let mut url = new_tab.url.clone();
    // Douyin's SPA sometimes takes 15+ seconds to route from the splash
    // to /creator-micro/. Poll generously so we don't false-negative
    // an Active session.
    for _ in 0..25 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some(t) = session.get_target(&new_tab.id).await? {
            url = t.url.clone();
            // Break as soon as we see a /creator-micro/* URL (logged in)
            // or a login/passport URL (logged out) — anything other than
            // the splash root.
            if url.contains("/creator-micro/") || url.contains("login") || url.contains("passport")
            {
                break;
            }
        }
    }

    let _ = session.close_tab(&new_tab.id).await;
    Ok(url)
}

#[async_trait]
impl Publisher for DouyinPublisher {
    fn platform(&self) -> Platform {
        Platform::Douyin
    }

    fn capabilities(&self) -> Capabilities {
        // From scripts/douyin/05_explore_upload.py + the publish form probe.
        Capabilities {
            // Description editor's "0 / 1000" counter.
            max_text_chars: Some(1000),
            // Cover image; the description editor accepts inline images too
            // but we treat cover-only for now.
            max_images: Some(1),
            video_supported: true,
            // The upload page advertises 60s+ recommended; the hard cap is
            // 15 minutes for standard accounts, longer for verified.
            video_max_seconds: Some(15 * 60),
            // Toutiao + Douyin both expose "定时发布" — but Douyin requires
            // additional verification we haven't wired yet, so claim false.
            schedule_supported: false,
            edit_supported: false,
            delete_supported: true,
        }
    }

    async fn refresh_credentials(
        &self,
        _credentials: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        // Browser sessions don't have tokens to refresh. Auth lives in the
        // Chrome profile's cookies; freshness is checked at `check_auth` time.
        Ok(None)
    }

    async fn check_auth(&self, ctx: &PublishContext<'_>) -> Result<AuthStatus> {
        let creds = parse_credentials(ctx.credentials)?;
        let url = probe_creator_url(&creds.cdp_url)
            .await
            .map_err(|e| PublishError::Transient(format!("douyin probe: {e}")))?;
        tracing::info!(url = %url, "douyin: probe-tab final URL");

        let logged_in = selectors::LOGGED_IN_URL_SIGNATURES
            .iter()
            .any(|sig| url.contains(sig));
        if logged_in {
            Ok(AuthStatus::Active)
        } else if url.contains("login") || url.contains("passport") {
            Ok(AuthStatus::Expired)
        } else {
            Ok(AuthStatus::Pending)
        }
    }

    async fn publish(
        &self,
        ctx: &mut PublishContext<'_>,
        content: &Content,
    ) -> Result<PublishHandle> {
        let creds = parse_credentials(ctx.credentials)?;
        let media = first_video(&ctx.media)?;
        let (title, description) = split_title_body(&content.text);

        // 1. Stash bytes to a local tempfile so SCP can read them.
        let local = write_local_tempfile(media)
            .map_err(|e| PublishError::Other(anyhow::anyhow!("local stage: {e}")))?;
        let staged = stage_file(&creds, local.path())
            .await
            .map_err(|e| PublishError::Transient(format!("douyin stage: {e}")))?;
        tracing::info!(remote = %staged.remote_path, "douyin: staged video on chrome host");

        // 2. Drive Chrome. On any failure, clean up the staged file before
        //    returning.
        let outcome = drive_upload(&creds, &staged, &title, &description, content.visibility).await;
        // Best-effort cleanup either way.
        let _ = cleanup_file(&creds, &staged).await;
        outcome
    }

    async fn confirm(
        &self,
        ctx: &PublishContext<'_>,
        handle: &PublishHandle,
    ) -> Result<ConfirmStatus> {
        let creds = parse_credentials(ctx.credentials)?;
        let title = handle.external_id.as_str();
        let (status, manage_url) = inspect_row(&creds, title)
            .await
            .map_err(|e| PublishError::Transient(format!("douyin confirm: {e}")))?;
        tracing::info!(title, status = ?status, "douyin confirm");
        match status {
            RowStatus::Published => Ok(ConfirmStatus::Confirmed {
                permalink: Some(manage_url),
            }),
            // Both "still in review" and "not yet listed" → keep polling;
            // the row may appear with 审核中 first and transition to 已发布.
            RowStatus::Reviewing | RowStatus::NotFound => Ok(ConfirmStatus::Pending),
            RowStatus::Rejected => Err(PublishError::Rejected(format!(
                "Douyin moderation rejected '{title}' (审核未通过)"
            ))),
        }
    }

    async fn delete(&self, ctx: &PublishContext<'_>, handle: &PublishHandle) -> Result<()> {
        let creds = parse_credentials(ctx.credentials)?;
        let title = handle.external_id.as_str();
        delete_row(&creds, title)
            .await
            .map_err(|e| PublishError::Transient(format!("douyin delete: {e}")))?;
        tracing::info!(title, "douyin: deleted work");
        Ok(())
    }
}

/// What we read off a manage-page row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowStatus {
    Published,
    Reviewing,
    Rejected,
    NotFound,
}

/// Open a fresh manage tab, find the row whose innerText contains `title`,
/// and return its status + the manage URL we landed on.
async fn inspect_row(
    creds: &DouyinCredentials,
    title: &str,
) -> anyhow::Result<(RowStatus, String)> {
    let session = BrowserSession::connect(&creds.cdp_url).await?;
    let tab = session.create_tab(selectors::MANAGE_URL).await?;
    let mut page = session.open_page(&tab).await?;

    let title_json = serde_json::to_string(title)?;
    let statuses_json = serde_json::to_string(selectors::ROW_STATUSES)?;
    // Find the row's title leaf via class `info-title-text-…` and match
    // either exact (post-moderation Douyin shows just the title) or prefix
    // (pre-moderation it concatenates title + description into the same
    // leaf separated by a space). Walk up to the smallest ancestor that
    // also contains one of `ROW_STATUSES`.
    let js = format!(
        r#"(() => {{
            const wantTitle = {title_json};
            const statuses = {statuses_json};
            const titleEls = Array.from(document.querySelectorAll('[class*="info-title-text"]')).filter(el => {{
                const t = (el.innerText || '').trim();
                return t === wantTitle || t.startsWith(wantTitle + ' ') || t.startsWith(wantTitle + '\n');
            }});
            if (titleEls.length === 0) return {{found: false}};
            let row = titleEls[0];
            for (let i = 0; i < 10; i++) {{
                if (!row.parentElement) break;
                row = row.parentElement;
                const text = row.innerText || '';
                const status = statuses.find(s => text.includes(s));
                if (status) return {{found: true, status}};
            }}
            return {{found: true, status: null}};
        }})()"#
    );

    // Poll for the row to appear (the manage XHR usually resolves in
    // 5-15s, but cold fresh tabs can take longer). We poll the actual
    // walk-up-from-title JS rather than a static selector, since the
    // outer container exists immediately and races the XHR.
    let start = Instant::now();
    let mut last_result = serde_json::Value::Null;
    while start.elapsed() < Duration::from_secs(60) {
        last_result = page.evaluate(&js).await?;
        if last_result
            .get("found")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let url_v = page.evaluate("location.href").await?;
    let manage_url = url_v.as_str().unwrap_or(selectors::MANAGE_URL).to_string();

    let status = if !last_result
        .get("found")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        RowStatus::NotFound
    } else {
        match last_result.get("status").and_then(|v| v.as_str()) {
            Some("已发布") => RowStatus::Published,
            Some("审核中") => RowStatus::Reviewing,
            Some("未通过") | Some("审核未通过") => RowStatus::Rejected,
            _ => RowStatus::Reviewing,
        }
    };
    let _ = session.close_tab(&tab.id).await;
    Ok((status, manage_url))
}

/// Open the manage page, find the row matching `title`, click 删除作品, then
/// confirm the modal.
async fn delete_row(creds: &DouyinCredentials, title: &str) -> anyhow::Result<()> {
    let session = BrowserSession::connect(&creds.cdp_url).await?;
    let tab = session.create_tab(selectors::MANAGE_URL).await?;
    let mut page = session.open_page(&tab).await?;

    wait_for_node(
        &mut page,
        &format!("[class*=\"{}\"]", selectors::MANAGE_ROW_CLASS),
        Duration::from_secs(45),
    )
    .await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let title_json = serde_json::to_string(title)?;
    let del_label_json = serde_json::to_string(selectors::ROW_DELETE_LABEL)?;
    let click_delete_js = format!(
        r#"(() => {{
            const wantTitle = {title_json};
            const delLabel = {del_label_json};
            // Same prefix-match-from-title approach as inspect_row.
            const titleEls = Array.from(document.querySelectorAll('[class*="info-title-text"]')).filter(el => {{
                const t = (el.innerText || '').trim();
                return t === wantTitle || t.startsWith(wantTitle + ' ') || t.startsWith(wantTitle + '\n');
            }});
            if (titleEls.length === 0) return {{ok: false, reason: 'no title element'}};
            let row = titleEls[0];
            for (let i = 0; i < 10; i++) {{
                if (!row.parentElement) break;
                row = row.parentElement;
                if ((row.innerText || '').includes(delLabel)) {{
                    const candidates = Array.from(row.querySelectorAll('*'))
                        .filter(el => el.children.length === 0 && (el.innerText || '').trim() === delLabel);
                    if (candidates.length === 0) continue;
                    candidates[0].click();
                    return {{ok: true}};
                }}
            }}
            return {{ok: false, reason: 'no matching row with delete affordance'}};
        }})()"#
    );
    let click_result = page.evaluate(&click_delete_js).await?;
    if !click_result
        .get("ok")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        anyhow::bail!("click 删除作品 failed: {click_result}");
    }
    tracing::info!(title, "douyin: clicked 删除作品");

    // Wait for the confirmation modal, then click 确定 / 删除.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let confirm_js = r#"(() => {
        // Find a button inside the most recent modal whose text is the
        // affirmative confirm.
        const wanted = ['确定', '删除', '确认删除'];
        for (const b of document.querySelectorAll('button')) {
            if (b.offsetParent === null) continue;
            const text = (b.innerText || '').trim();
            if (wanted.includes(text)) {
                b.click();
                return {ok: true, text};
            }
        }
        return {ok: false};
    })()"#;
    let confirm_result = page.evaluate(confirm_js).await?;
    if !confirm_result
        .get("ok")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        anyhow::bail!("confirm 删除 modal click failed: {confirm_result}");
    }
    tracing::info!("douyin: confirmed delete modal");

    // Wait briefly for the row to disappear (best-effort): true iff there's
    // no longer any `info-title-text-…` leaf whose text starts with the title.
    let title_json2 = serde_json::to_string(title)?;
    let check_js = format!(
        r#"(() => {{
            const want = {title_json2};
            return !Array.from(document.querySelectorAll('[class*="info-title-text"]')).some(el => {{
                const t = (el.innerText || '').trim();
                return t === want || t.startsWith(want + ' ') || t.startsWith(want + '\n');
            }});
        }})()"#
    );
    // Up to 10s for the deletion to reflect.
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if page.evaluate(&check_js).await?.as_bool().unwrap_or(false) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let _ = session.close_tab(&tab.id).await;
    Ok(())
}

/// Pick the first video MediaPayload (we only ever upload one video at a time).
fn first_video(media: &[MediaPayload]) -> Result<&MediaPayload> {
    media
        .iter()
        .find(|m| m.mime_type.starts_with("video/"))
        .ok_or_else(|| {
            PublishError::Rejected("douyin requires exactly one video media payload".into())
        })
}

/// Douyin caps the title at ~30 chars on the input. We split the content's
/// `text` on the first newline: line 1 → title (truncated), rest → body.
fn split_title_body(text: &str) -> (String, String) {
    let (head, rest) = match text.find('\n') {
        Some(i) => (&text[..i], &text[i + 1..]),
        None => (text, ""),
    };
    let title: String = head.chars().take(30).collect();
    (title, rest.trim_start().to_string())
}

/// Write the in-memory bytes to a tempfile and keep it alive for the SCP.
fn write_local_tempfile(media: &MediaPayload) -> anyhow::Result<tempfile::NamedTempFile> {
    use std::io::Write;
    let suffix = std::path::Path::new(&media.filename)
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_else(|| ".mp4".to_string());
    let mut f = tempfile::Builder::new()
        .prefix("multipost-douyin-")
        .suffix(&suffix)
        .tempfile()?;
    f.write_all(&media.bytes)?;
    f.flush()?;
    Ok(f)
}

/// Open a fresh upload tab, drive the full upload → fill → publish flow,
/// and return a handle whose `external_id` is the Douyin work ID parsed
/// from the post-publish manage URL.
async fn drive_upload(
    creds: &DouyinCredentials,
    staged: &StagedFile,
    title: &str,
    description: &str,
    visibility: Visibility,
) -> Result<PublishHandle> {
    let session = BrowserSession::connect(&creds.cdp_url)
        .await
        .map_err(|e| PublishError::Transient(format!("douyin connect: {e}")))?;
    // Open a fresh tab directly at the upload page. PUT /json/new takes a
    // URL, so navigation happens server-side — no WS Page.navigate needed.
    let tab = session
        .create_tab(UPLOAD_URL)
        .await
        .map_err(|e| PublishError::Transient(format!("douyin create_tab: {e}")))?;
    tracing::info!(target = %tab.id, "douyin: opened upload tab");

    let mut page = session
        .open_page(&tab)
        .await
        .map_err(|e| PublishError::Transient(format!("douyin open_page: {e}")))?;

    let result = run_upload_flow(&mut page, staged, title, description, visibility).await;
    // On success, close the upload tab so it doesn't accumulate on the
    // Chrome host across many publishes. On failure, leave it open so
    // the user can inspect what state Douyin left the form in.
    if result.is_ok() {
        let _ = session.close_tab(&tab.id).await;
    }
    // Douyin doesn't surface a stable aweme_id in either the post-publish URL
    // or the manage row DOM — confirm/delete match the work by title instead.
    // Store the title as external_id so the orchestrator can pass it back.
    result.map(|outcome| PublishHandle {
        external_id: title.to_string(),
        permalink: outcome.permalink,
    })
}

/// What `run_upload_flow` returns. `permalink` is the manage URL; for now
/// we have no way to deep-link to the specific work.
struct UploadOutcome {
    permalink: Option<String>,
}

async fn run_upload_flow(
    page: &mut PageSession,
    staged: &StagedFile,
    title: &str,
    description: &str,
    visibility: Visibility,
) -> Result<UploadOutcome> {
    let input_node = wait_for_node(page, selectors::FILE_INPUT, Duration::from_secs(30))
        .await
        .map_err(|e| PublishError::Transient(format!("douyin file input: {e}")))?;
    tracing::info!(node = input_node, "douyin: located file input");

    page.set_file_input_files(input_node, std::slice::from_ref(&staged.remote_path))
        .await
        .map_err(|e| PublishError::Transient(format!("douyin setFileInputFiles: {e}")))?;

    wait_for_url_match(page, "/creator-micro/content/post", Duration::from_secs(60))
        .await
        .map_err(|e| PublishError::Transient(format!("douyin upload transition: {e}")))?;

    let _ = dismiss_tooltips(page).await;

    // ---- Wait for video upload to complete on Douyin's servers ----
    // After setFileInputFiles, the page transitions to the post editor and
    // Douyin starts uploading the video to its CDN. The upload progress is
    // shown in the UI. We MUST wait for this to complete before filling the
    // form or clicking publish, otherwise Douyin silently discards the
    // incomplete upload.
    //
    // Indicators:
    //   - Upload in progress: page contains "上传中" or a percentage like "42%"
    //   - Upload complete: page contains "重新上传" or "替换视频"
    //   - Upload failed: page contains "上传失败"
    wait_for_video_upload_complete(page, Duration::from_secs(300))
        .await
        .map_err(|e| PublishError::Transient(format!("douyin video upload: {e}")))?;

    // Form hydrates async after the URL flips — wait for both the title
    // input and the description editor to be in the DOM before filling.
    // Bumped to 60s — on busy Chromes Douyin sometimes delays mounting
    // the SPA form by 30+ s after the URL transition.
    wait_for_node(page, selectors::TITLE_INPUT, Duration::from_secs(60))
        .await
        .map_err(|e| PublishError::Transient(format!("douyin title input: {e}")))?;
    wait_for_node(page, selectors::DESCRIPTION_EDITOR, Duration::from_secs(60))
        .await
        .map_err(|e| PublishError::Transient(format!("douyin description editor: {e}")))?;

    fill_title(page, title)
        .await
        .map_err(|e| PublishError::Transient(format!("douyin title: {e}")))?;
    fill_description(page, description)
        .await
        .map_err(|e| PublishError::Transient(format!("douyin description: {e}")))?;

    // Pick the visibility radio. If the user requested 公开/好友可见, we must
    // click it (Douyin's default isn't safe to rely on for explicit asks).
    // For 私密/未列出, individual accounts often don't expose the radio at
    // all — in that case we refuse to publish so we never accidentally post
    // a "private" upload as public.
    let vis_result = select_visibility(page, visibility).await?;
    if !vis_result.matched {
        match visibility {
            Visibility::Private | Visibility::Unlisted => {
                return Err(PublishError::Rejected(format!(
                    "Douyin form has no '{}' radio (individual accounts may lack this option). \
                     Refusing to publish to avoid leaking a 'private' upload as public.",
                    selectors::VISIBILITY_PRIVATE_LABELS.join("' / '")
                )));
            }
            _ => {
                tracing::warn!(
                    visibility = ?visibility,
                    "douyin: visibility radio not found; proceeding with Douyin's account default"
                );
            }
        }
    }

    // Wait for the 发布 button to become enabled (file encoding finishes
    // server-side after the upload), then click it.
    wait_for_publish_enabled(page, Duration::from_secs(180))
        .await
        .map_err(|e| PublishError::Transient(format!("douyin publish enable: {e}")))?;
    click_publish(page)
        .await
        .map_err(|e| PublishError::Transient(format!("douyin publish click: {e}")))?;

    // After clicking, Douyin redirects to the manage page with the work ID
    // in the URL, e.g.
    //   /creator-micro/content/manage?type=video
    //   /creator-micro/content/manage/video?modal_id=<id>  (rare)
    wait_for_url_match(
        page,
        "/creator-micro/content/manage",
        Duration::from_secs(240),
    )
    .await
    .map_err(|e| PublishError::Transient(format!("douyin post-publish redirect: {e}")))?;

    let final_url_v = page
        .evaluate("location.href")
        .await
        .map_err(|e| PublishError::Transient(format!("douyin location.href: {e}")))?;
    let final_url = final_url_v.as_str().unwrap_or("").to_string();
    tracing::info!(url = %final_url, "douyin: post-publish manage URL");

    Ok(UploadOutcome {
        permalink: Some(final_url),
    })
}

/// Outcome of a visibility radio click attempt.
struct VisibilitySelection {
    /// True iff we found and clicked the radio whose text matched.
    matched: bool,
}

/// Click the visibility radio that matches the requested `Visibility`.
async fn select_visibility(page: &mut PageSession, vis: Visibility) -> Result<VisibilitySelection> {
    let labels: &[&str] = match vis {
        Visibility::Public => &[selectors::VISIBILITY_PUBLIC_LABEL],
        Visibility::Followers => &[selectors::VISIBILITY_FRIENDS_LABEL],
        Visibility::Unlisted | Visibility::Private => selectors::VISIBILITY_PRIVATE_LABELS,
    };
    // Match against `<label class="radio-*">…<span>公开</span></label>` (Semi
    // Design's radio markup), as confirmed by `scripts/douyin/07_probe_form.py`.
    // `want` is a list of accepted labels (Douyin renames these across UI
    // versions, e.g. 私密 -> 仅自己可见), and we click the first exact match.
    let label_json = serde_json::to_string(labels)
        .map_err(|e| PublishError::Other(anyhow::anyhow!("serialize label: {e}")))?;
    let js = format!(
        r#"(() => {{
            const want = {label_json};
            for (const el of document.querySelectorAll('label')) {{
                const text = (el.innerText || '').trim();
                if (want.includes(text) && el.offsetParent !== null) {{
                    el.click();
                    return {{ok: true, label: text}};
                }}
            }}
            return {{ok: false}};
        }})()"#,
    );
    let r = page
        .evaluate(&js)
        .await
        .map_err(|e| PublishError::Transient(format!("douyin visibility js: {e}")))?;
    let matched = r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    tracing::info!(visibility = ?vis, matched, "douyin: visibility selection");
    Ok(VisibilitySelection { matched })
}

/// Poll until the video finishes uploading to Douyin's CDN.
///
/// Douyin's upload page shows progress indicators while the video is being
/// processed server-side. We poll the page text for completion markers:
///   - "重新上传" or "替换视频" → upload complete
///   - "上传失败" → upload failed
///   - "上传中" or a bare percentage → still uploading
///
/// This function blocks until the upload completes or the deadline expires.
/// Without this wait, clicking 发布 on an incomplete upload causes Douyin
/// to silently discard the video (it appears to publish but never shows up
/// in the manage page).
async fn wait_for_video_upload_complete(
    page: &mut PageSession,
    deadline: Duration,
) -> anyhow::Result<()> {
    let start = Instant::now();
    let js = r#"(() => {
        const text = document.body.innerText || '';
        if (text.includes('重新上传') || text.includes('替换视频')) return 'complete';
        if (text.includes('上传失败')) return 'failed';
        if (text.includes('上传中')) return 'uploading';
        // Check for percentage pattern (e.g. "42%") which indicates upload progress
        if (/\d+%/.test(text)) return 'uploading';
        return 'waiting';
    })()"#;

    let mut last_state = String::new();
    loop {
        let result = page.evaluate(js).await?;
        let state = result.as_str().unwrap_or("unknown").to_string();

        if state != last_state {
            tracing::info!(state = %state, elapsed = ?start.elapsed(), "douyin: video upload state");
            last_state = state.clone();
        }

        match state.as_str() {
            "complete" => {
                tracing::info!(elapsed = ?start.elapsed(), "douyin: video upload complete");
                return Ok(());
            }
            "failed" => {
                anyhow::bail!("Douyin video upload failed (上传失败)");
            }
            _ => {
                if start.elapsed() > deadline {
                    anyhow::bail!(
                        "timed out waiting for video upload to complete (last state: {state}, elapsed: {:?})",
                        start.elapsed()
                    );
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

/// Poll for the 发布 button to become enabled.
async fn wait_for_publish_enabled(
    page: &mut PageSession,
    deadline: Duration,
) -> anyhow::Result<()> {
    let start = Instant::now();
    let js = r#"(() => {
        // Find a button whose visible text is exactly '发布'.
        for (const b of document.querySelectorAll('button')) {
            const text = (b.innerText || '').trim();
            if (text === '发布' && b.offsetParent !== null) {
                return {found: true, disabled: b.disabled || b.getAttribute('aria-disabled') === 'true'};
            }
        }
        return {found: false};
    })()"#;
    loop {
        let r = page.evaluate(js).await?;
        let found = r.get("found").and_then(|v| v.as_bool()).unwrap_or(false);
        let disabled = r.get("disabled").and_then(|v| v.as_bool()).unwrap_or(true);
        if found && !disabled {
            tracing::info!("douyin: publish button enabled");
            return Ok(());
        }
        if start.elapsed() > deadline {
            anyhow::bail!("timed out waiting for 发布 button to enable (last: {r})");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Click the 发布 button.
async fn click_publish(page: &mut PageSession) -> anyhow::Result<()> {
    let js = r#"(() => {
        for (const b of document.querySelectorAll('button')) {
            const text = (b.innerText || '').trim();
            if (text === '发布' && b.offsetParent !== null && !b.disabled) {
                b.click();
                return {ok: true};
            }
        }
        return {ok: false};
    })()"#;
    let r = page.evaluate(js).await?;
    if !r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        anyhow::bail!("publish click failed: {r}");
    }
    tracing::info!("douyin: clicked 发布");
    Ok(())
}

/// Poll for a node matching `selector` to appear under the document root.
///
/// Tolerates the CDP race where the document is replaced (SPA navigation,
/// hot reload) between `get_document` and `query_selector` — in that case
/// `query_selector` returns "Could not find node with given id" and we
/// just retry with a fresh document on the next iteration.
async fn wait_for_node(
    page: &mut PageSession,
    selector: &str,
    deadline: Duration,
) -> anyhow::Result<u64> {
    let start = Instant::now();
    loop {
        let doc = match page.get_document().await {
            Ok(d) => d,
            Err(_) if start.elapsed() <= deadline => {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            Err(e) => return Err(e),
        };
        match page.query_selector(doc, selector).await {
            Ok(0) => {
                if start.elapsed() > deadline {
                    anyhow::bail!("timed out waiting for selector {selector:?}");
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Ok(id) => return Ok(id),
            Err(e) => {
                // CDP -32000 "Could not find node with given id" → document
                // was navigated under us; loop and re-fetch the document.
                if start.elapsed() > deadline {
                    return Err(e);
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// Poll the page URL until it contains `needle`.
async fn wait_for_url_match(
    page: &mut PageSession,
    needle: &str,
    deadline: Duration,
) -> anyhow::Result<()> {
    let start = Instant::now();
    loop {
        let url = page.evaluate("location.href").await?;
        let url = url.as_str().unwrap_or("");
        if url.contains(needle) {
            tracing::info!(url, "douyin: URL transitioned");
            return Ok(());
        }
        if start.elapsed() > deadline {
            anyhow::bail!("timed out waiting for URL {needle:?} (last seen: {url})");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Find any visible tooltip-dismiss button by its label text and click it.
async fn dismiss_tooltips(page: &mut PageSession) -> anyhow::Result<()> {
    let js = format!(
        r#"(() => {{
            const labels = {labels};
            let dismissed = 0;
            for (const el of document.querySelectorAll('button, span, div[role="button"]')) {{
                const text = (el.innerText || '').trim();
                if (labels.includes(text) && el.offsetParent !== null) {{
                    el.click();
                    dismissed++;
                }}
            }}
            return dismissed;
        }})()"#,
        labels = serde_json::to_string(selectors::TOOLTIP_DISMISS_LABELS)?
    );
    let n = page.evaluate(&js).await?;
    tracing::info!(dismissed = ?n, "douyin: tooltip pass");
    Ok(())
}

/// Set the title input's value and dispatch an input event so React picks it up.
async fn fill_title(page: &mut PageSession, title: &str) -> anyhow::Result<()> {
    let js = format!(
        r#"(() => {{
            const input = document.querySelector({sel});
            if (!input) return {{ok: false, reason: 'no input'}};
            const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set;
            setter.call(input, {value});
            input.dispatchEvent(new Event('input', {{bubbles: true}}));
            input.dispatchEvent(new Event('change', {{bubbles: true}}));
            return {{ok: true, value: input.value}};
        }})()"#,
        sel = serde_json::to_string(selectors::TITLE_INPUT)?,
        value = serde_json::to_string(title)?,
    );
    let result = page.evaluate(&js).await?;
    if !result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        anyhow::bail!("title fill failed: {result}");
    }
    Ok(())
}

/// Fill the ProseMirror description editor via execCommand('insertText').
async fn fill_description(page: &mut PageSession, body: &str) -> anyhow::Result<()> {
    if body.is_empty() {
        return Ok(());
    }
    // Chunk to keep the JS string under the WebSocket frame limits.
    for chunk in body.chars().collect::<Vec<_>>().chunks(800) {
        let piece: String = chunk.iter().collect();
        let js = format!(
            r#"(() => {{
                const editor = document.querySelector({sel});
                if (!editor) return {{ok: false, reason: 'no editor'}};
                editor.focus();
                document.execCommand('insertText', false, {value});
                return {{ok: true}};
            }})()"#,
            sel = serde_json::to_string(selectors::DESCRIPTION_EDITOR)?,
            value = serde_json::to_string(&piece)?,
        );
        let result = page.evaluate(&js).await?;
        if !result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
            anyhow::bail!("description fill failed: {result}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_credentials_round_trip() {
        let json = serde_json::json!({
            "cdp_url": "http://localhost:9333",
            "douyin_id": "12345",
            "nickname": "辉昀空",
        });
        let creds = parse_credentials(&json).unwrap();
        assert_eq!(creds.cdp_url, "http://localhost:9333");
        assert_eq!(creds.douyin_id, "12345");
        assert_eq!(creds.nickname, "辉昀空");
    }

    #[test]
    fn parse_credentials_minimal() {
        // douyin_id / nickname are optional.
        let json = serde_json::json!({"cdp_url": "http://localhost:9333"});
        let creds = parse_credentials(&json).unwrap();
        assert_eq!(creds.cdp_url, "http://localhost:9333");
        assert!(creds.douyin_id.is_empty());
    }

    #[test]
    fn capabilities_sane() {
        let pub_ = DouyinPublisher::new();
        let c = pub_.capabilities();
        assert!(c.video_supported);
        assert_eq!(pub_.platform(), Platform::Douyin);
        // Description editor's hard cap.
        assert_eq!(c.max_text_chars, Some(1000));
    }
}
