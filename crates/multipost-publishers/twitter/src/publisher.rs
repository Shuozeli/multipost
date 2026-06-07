//! `Publisher` implementation for Twitter / X.
//!
//! - `check_auth`: REST probe — open a fresh tab at `x.com/home`, poll
//!   for redirect to `/i/flow/login` (logged out) vs staying on `/home`
//!   (logged in).
//! - `publish`: open the inline composer, fill via `execCommand`,
//!   click the inline Post button. Twitter's success signal is the
//!   composer clearing back to empty + a network round-trip; we poll
//!   for the textarea to come back empty within a deadline.
//! - `confirm`: returns `Confirmed` immediately. Twitter posts are
//!   live the moment Post clicks (no moderation queue), and we already
//!   verified the post landed in `publish` before returning.
//! - `delete`: navigate to `/<handle>`, find the tweet by body-prefix
//!   in a `<article role="article">`, click its caret, click "Delete"
//!   in the menu, confirm the modal.

use async_trait::async_trait;
use multipost_core::{
    AuthStatus, Capabilities, ConfirmStatus, Content, ContentKind, MediaPayload, Platform,
    PublishContext, PublishError, PublishHandle, Publisher, Result,
};
use rand::Rng;
use std::time::{Duration, Instant};

use crate::cdp::{BrowserSession, PageSession};
use crate::credentials::TwitterCredentials;
use crate::selectors;

/// Twitter publisher. Stateless — all per-account config lives in
/// `PublishContext::credentials`.
pub struct TwitterPublisher;

impl TwitterPublisher {
    /// Construct a new publisher.
    pub fn new() -> Self {
        Self
    }
}

impl Default for TwitterPublisher {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_credentials(value: &serde_json::Value) -> Result<TwitterCredentials> {
    serde_json::from_value::<TwitterCredentials>(value.clone()).map_err(|e| {
        PublishError::Other(anyhow::anyhow!(
            "credentials don't deserialize as TwitterCredentials: {e}"
        ))
    })
}

/// Probe Twitter's auth state by creating a fresh tab at `x.com/home`
/// and reading the URL after the SPA settles. Same REST-only pattern
/// as the Toutiao / Douyin check_auth — no WebSocket attach, so it
/// doesn't race stale CDP clients on existing tabs.
async fn probe_home_url(cdp_url: &str) -> anyhow::Result<String> {
    let session = BrowserSession::connect(cdp_url).await?;
    let new_tab = session.create_tab(selectors::COMPOSE_URL).await?;
    tracing::debug!(target_id = %new_tab.id, "twitter: created probe tab");

    let mut url = new_tab.url.clone();
    for _ in 0..25 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some(t) = session.get_target(&new_tab.id).await? {
            url = t.url.clone();
            if selectors::LOGIN_URL_SIGNATURES
                .iter()
                .any(|s| url.contains(s))
                || selectors::LOGGED_IN_URL_SIGNATURES
                    .iter()
                    .any(|s| url.contains(s))
            {
                break;
            }
        }
    }

    let _ = session.close_tab(&new_tab.id).await;
    Ok(url)
}

#[async_trait]
impl Publisher for TwitterPublisher {
    fn platform(&self) -> Platform {
        Platform::Twitter
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            max_text_chars: Some(selectors::MAX_TWEET_CHARS),
            max_images: Some(selectors::MAX_TWEET_IMAGES),
            video_supported: false, // would need a separate flow
            video_max_seconds: None,
            schedule_supported: false,
            edit_supported: false,
            delete_supported: true,
        }
    }

    async fn refresh_credentials(
        &self,
        _credentials: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        // Browser-cookie auth — nothing to refresh.
        Ok(None)
    }

    async fn check_auth(&self, ctx: &PublishContext<'_>) -> Result<AuthStatus> {
        let creds = parse_credentials(ctx.credentials)?;
        let url = probe_home_url(&creds.cdp_url)
            .await
            .map_err(|e| PublishError::Transient(format!("twitter probe: {e}")))?;
        tracing::info!(url = %url, "twitter: probe-tab final URL");
        if selectors::LOGIN_URL_SIGNATURES
            .iter()
            .any(|s| url.contains(s))
        {
            return Ok(AuthStatus::Expired);
        }
        if selectors::LOGGED_IN_URL_SIGNATURES
            .iter()
            .any(|s| url.contains(s))
        {
            return Ok(AuthStatus::Active);
        }
        Ok(AuthStatus::Pending)
    }

    async fn publish(
        &self,
        ctx: &mut PublishContext<'_>,
        content: &Content,
    ) -> Result<PublishHandle> {
        let body = content.text.trim().to_string();
        validate_tweet(content.kind, &body, &ctx.media)?;

        let creds = parse_credentials(ctx.credentials)?;
        let session = BrowserSession::connect(&creds.cdp_url)
            .await
            .map_err(|e| PublishError::Transient(format!("twitter connect: {e}")))?;
        let tab = session
            .create_tab(selectors::COMPOSE_URL)
            .await
            .map_err(|e| PublishError::Transient(format!("twitter create_tab: {e}")))?;
        tracing::info!(target = %tab.id, "twitter: opened compose tab");
        let mut page = session
            .open_page(&tab)
            .await
            .map_err(|e| PublishError::Transient(format!("twitter open_page: {e}")))?;

        run_publish_flow(&mut page, &body, &ctx.media)
            .await
            .map_err(|e| PublishError::Transient(format!("twitter publish: {e}")))?;

        // Success: close the compose tab so it doesn't accumulate on
        // the Chrome host across many publishes. (Failure path leaves
        // it open for debugging.)
        let _ = session.close_tab(&tab.id).await;

        // External ID = first 30 chars of body, same prefix-match
        // convention used by Toutiao / Douyin / 微头条 delete.
        let external_id: String = body.chars().take(30).collect();
        Ok(PublishHandle {
            external_id,
            permalink: Some(format!("https://x.com/{}", creds.handle)),
        })
    }

    async fn confirm(
        &self,
        _ctx: &PublishContext<'_>,
        handle: &PublishHandle,
    ) -> Result<ConfirmStatus> {
        // Twitter posts are live immediately. The publish flow already
        // verified the composer cleared, so by the time confirm() runs
        // the tweet is on the timeline.
        Ok(ConfirmStatus::Confirmed {
            permalink: handle.permalink.clone(),
        })
    }

    async fn delete(&self, ctx: &PublishContext<'_>, handle: &PublishHandle) -> Result<()> {
        let creds = parse_credentials(ctx.credentials)?;
        let prefix = handle.external_id.as_str();
        delete_tweet_by_prefix(&creds, prefix)
            .await
            .map_err(|e| PublishError::Transient(format!("twitter delete: {e}")))?;
        tracing::info!(prefix, "twitter: deleted tweet");
        Ok(())
    }
}

/// Sleep a random duration in `[lo_ms, hi_ms]`. The RNG is sampled and
/// dropped before the `.await`, so no `!Send` guard is held across it.
async fn jitter(lo_ms: u64, hi_ms: u64) {
    let ms = rand::thread_rng().gen_range(lo_ms..=hi_ms);
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

/// Viewport-pixel center of the first element matching `selector`, or
/// `None` if it isn't present / has zero size. Used to aim real mouse
/// clicks (trusted input) at the composer and Post button.
async fn element_center(
    page: &mut PageSession,
    selector: &str,
) -> anyhow::Result<Option<(f64, f64)>> {
    let js = format!(
        r#"(() => {{
            const el = document.querySelector({sel});
            if (!el) return null;
            const r = el.getBoundingClientRect();
            if (r.width === 0 && r.height === 0) return null;
            return {{ x: r.x + r.width / 2, y: r.y + r.height / 2 }};
        }})()"#,
        sel = serde_json::to_string(selector)?
    );
    let v = page.evaluate(&js).await?;
    match (
        v.get("x").and_then(|x| x.as_f64()),
        v.get("y").and_then(|y| y.as_f64()),
    ) {
        (Some(x), Some(y)) => Ok(Some((x, y))),
        _ => Ok(None),
    }
}

/// Type `body` into the focused composer one Unicode scalar at a time
/// via trusted `Input.insertText`, with randomized inter-keystroke
/// delays and occasional longer "thinking" pauses.
///
/// This is the core anti-detection change: a human types at an irregular
/// cadence and every keystroke is a trusted input event, whereas the old
/// `execCommand` fill dumped the whole string in one untrusted call —
/// exactly the pattern Twitter's automation heuristics flag.
async fn type_humanlike(page: &mut PageSession, body: &str) -> anyhow::Result<()> {
    for ch in body.chars() {
        page.insert_text(&ch.to_string()).await?;
        // Sample the pause decision before awaiting (keeps RNG !Send-safe).
        let long_pause = rand::thread_rng().gen_bool(0.06);
        if long_pause {
            jitter(280, 760).await; // mid-sentence beat
        } else {
            jitter(45, 140).await; // normal key-to-key
        }
    }
    Ok(())
}

/// Drive the inline composer on `/home` the way a person would: wait for
/// mount, real-mouse-click into the textarea, type the body character by
/// character with human cadence (trusted input events), pause, then
/// real-mouse-click Post. Verify the composer cleared (success) and
/// distinguish Twitter's automation-block toast from a generic failure.
async fn run_publish_flow(
    page: &mut PageSession,
    body: &str,
    media: &[MediaPayload],
) -> anyhow::Result<()> {
    // Wait for the inline composer to mount. Twitter's React shell
    // takes 2-5s on a warm Chrome, longer on cold.
    let deadline = Duration::from_secs(45);
    let start = Instant::now();
    loop {
        let probe = page
            .evaluate(
                r#"(() => {
                    const ta = document.querySelector('[data-testid="tweetTextarea_0"]');
                    const btn = document.querySelector('[data-testid="tweetButtonInline"]');
                    return { ta: !!ta, btn: !!btn };
                })()"#,
            )
            .await?;
        let ok_ta = probe.get("ta").and_then(|v| v.as_bool()).unwrap_or(false);
        let ok_btn = probe.get("btn").and_then(|v| v.as_bool()).unwrap_or(false);
        if ok_ta && ok_btn {
            break;
        }
        if start.elapsed() > deadline {
            anyhow::bail!("twitter inline composer did not mount within 45s");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    tracing::info!("twitter: composer mounted");

    // Foreground the tab so real mouse events hit-test correctly (a
    // background tab no-ops CDP clicks) and so the session looks like a
    // human's active compose tab. Best-effort — typing still works even
    // if this fails, only the real click depends on it.
    if let Err(e) = page.bring_to_front().await {
        tracing::warn!(error = %e, "twitter: Page.bringToFront failed; real click may not land");
    }

    // A beat to "read" the page before touching anything.
    jitter(500, 1400).await;

    // Focus the composer with a REAL mouse click (trusted) rather than
    // el.focus(). Twitter's composer is Draft.js — it commits on the
    // beforeinput/input path, which the subsequent Input.insertText
    // keystrokes feed. Fall back to JS focus only if we can't measure
    // the box (e.g. mid-relayout).
    match element_center(page, selectors::TEXTAREA).await? {
        Some((x, y)) => {
            page.real_click(x, y).await?;
        }
        None => {
            let focus_js = format!(
                r#"(() => {{ const el = document.querySelector({s}); if (el) el.focus(); return !!el; }})()"#,
                s = serde_json::to_string(selectors::TEXTAREA)?
            );
            page.evaluate(&focus_js).await?;
        }
    }
    jitter(220, 600).await;

    // Type the body one scalar at a time with human cadence.
    type_humanlike(page, body).await?;
    tracing::info!(
        chars = body.chars().count(),
        "twitter: body typed (humanlike)"
    );

    // Attach images, if any. The composer toolbar — including the hidden
    // media `<input type=file>` — is mounted now that we've focused +
    // typed; stream the bytes in over CDP (the Chrome is remote) and wait
    // for Twitter's upload round-trip before clicking Post (clicking early
    // drops the media). Empty-body image-only tweets skip typing above but
    // the real-mouse focus still expanded the composer.
    if !media.is_empty() {
        let start = Instant::now();
        loop {
            let present = page
                .evaluate(r#"!!document.querySelector('[data-testid="fileInput"]')"#)
                .await?
                .as_bool()
                .unwrap_or(false);
            if present {
                break;
            }
            if start.elapsed() > Duration::from_secs(15) {
                anyhow::bail!("twitter: media file input never mounted (composer not expanded?)");
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        let files: Vec<(&str, &str, &[u8])> = media
            .iter()
            .map(|m| {
                (
                    m.filename.as_str(),
                    m.mime_type.as_str(),
                    m.bytes.as_slice(),
                )
            })
            .collect();
        page.upload_files_to_input(selectors::FILE_INPUT, &files)
            .await?;
        tracing::info!(n = media.len(), "twitter: images set on file input");
        wait_for_media_attached(page, media.len()).await?;
    }

    // Wait for the Post button to enable (Draft commits the input
    // asynchronously — we poll briefly). The per-keystroke Input.insertText
    // intermittently fails to register in Draft.js, leaving the button
    // disabled (observed 3x on 2026-05-25). If it doesn't enable within a
    // short window, fall back ONCE to a clear + execCommand('insertText')
    // bulk fill — the proven Draft-committing path — then keep polling.
    let wait_btn_deadline = Duration::from_secs(12);
    let start = Instant::now();
    let mut execcommand_fallback_done = false;
    let body_json = serde_json::to_string(body)?;
    loop {
        let r = page
            .evaluate(
                r#"(() => {
                    const btn = document.querySelector('[data-testid="tweetButtonInline"]');
                    if (!btn) return { found: false };
                    return {
                        found: true,
                        disabled: btn.disabled || btn.getAttribute('aria-disabled') === 'true',
                    };
                })()"#,
            )
            .await?;
        let found = r.get("found").and_then(|v| v.as_bool()).unwrap_or(false);
        let disabled = r.get("disabled").and_then(|v| v.as_bool()).unwrap_or(true);
        if found && !disabled {
            break;
        }
        // Button still disabled after ~3s -> per-keystroke typing didn't hit
        // Draft. Use the proven prototype path from scripts/twitter:
        // focus + execCommand('insertText') in one page-side call. Do not
        // precede it with selectAll/delete; that can desync Twitter's
        // React/Draft model from the visible DOM and leave the model empty.
        if !execcommand_fallback_done && start.elapsed() > Duration::from_secs(3) {
            let fill_js = format!(
                r#"(() => {{
                    const el = document.querySelector('[data-testid="tweetTextarea_0"]');
                    if (!el) return {{ ok: false }};
                    el.focus();
                    document.execCommand('insertText', false, {body_json});
                    return {{ ok: true, text: (el.innerText || '').slice(0, 120) }};
                }})()"#,
            );
            let fr = page.evaluate(&fill_js).await?;
            execcommand_fallback_done = true;
            tracing::warn!(result = %fr, "twitter: per-keystroke typing left Post disabled; used execCommand fill fallback");
        }
        if start.elapsed() > wait_btn_deadline {
            anyhow::bail!(
                "Post button stayed disabled — body fill may not have hit Draft state (execCommand fallback also failed)"
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // A short "review" pause before committing — humans don't fire Post
    // the instant the button lights up.
    jitter(500, 1400).await;

    // Click Post — a REAL mouse click first (trusted). The inline composer's
    // Post button does not always register a CDP mouse click; tweets that
    // posted fine earlier used btn.click(). So we verify and, if the composer
    // hasn't cleared, fall back to a JS .click(). The fallback only fires
    // while the composer STILL holds our content, so it cannot double-post.
    match element_center(page, selectors::POST_BUTTON).await? {
        Some((x, y)) => page.real_click(x, y).await?,
        None => anyhow::bail!("twitter: Post button vanished before click"),
    }
    tracing::info!("twitter: clicked Post (real mouse)");

    const JS_CLICK: &str = r#"(() => {
        const btn = document.querySelector('[data-testid="tweetButtonInline"]');
        if (!btn) return { ok: false, reason: 'no button' };
        if (btn.disabled || btn.getAttribute('aria-disabled') === 'true')
            return { ok: false, reason: 'disabled' };
        btn.click();
        return { ok: true };
    })()"#;

    // Verify submission (Twitter's success signal: composer text gone AND
    // any media attachments cleared), watch for the automation-block toast,
    // and fall back to a JS click if the real click didn't take. 30s budget
    // covers a media upload round-trip.
    let deadline = Duration::from_secs(30);
    let start = Instant::now();
    let mut js_fallback_done = false;
    loop {
        let r = page
            .evaluate(
                r#"(() => {
                    const el = document.querySelector('[data-testid="tweetTextarea_0"]');
                    const state = !el ? 'gone'
                        : ((el.innerText || '').trim().length === 0 ? 'empty' : 'filled');
                    const att = document.querySelector('[data-testid="attachments"]');
                    const attEmpty = !att || att.querySelectorAll('img').length === 0;
                    const pageText = (document.body && document.body.innerText || '');
                    const blocked = /looks like it might be automated|can.t complete this action|something went wrong/i.test(pageText);
                    return { state, attEmpty, blocked };
                })()"#,
            )
            .await?;
        let state = r.get("state").and_then(|v| v.as_str()).unwrap_or("");
        let att_empty = r.get("attEmpty").and_then(|v| v.as_bool()).unwrap_or(true);
        let blocked = r.get("blocked").and_then(|v| v.as_bool()).unwrap_or(false);
        if blocked {
            anyhow::bail!(
                "twitter: blocked by automation detection — Twitter refused the post \
                 ('looks like it might be automated'); back off and retry later"
            );
        }
        // Success = composer text gone AND any media attachments cleared.
        // Image-only tweets start with empty text, so the attachments
        // clearing is the real signal (text alone would false-positive).
        let submitted = (state == "empty" || state == "gone") && att_empty;
        if submitted {
            tracing::info!(
                state,
                fellback = js_fallback_done,
                "twitter: post submitted"
            );
            return Ok(());
        }
        // Not submitted yet -> the real click didn't take. Our content
        // (text and/or attachments) is still in the composer, so a JS click
        // is safe (nothing has gone out) and matches the path that worked
        // before the humanized rewrite. Covers image-only tweets too, where
        // the text is empty but the attachments still pin the draft.
        if !js_fallback_done && start.elapsed() > Duration::from_secs(4) {
            let jr = page.evaluate(JS_CLICK).await?;
            js_fallback_done = true;
            tracing::warn!(result = %jr, "twitter: real click didn't submit; tried JS .click() fallback");
        }
        if start.elapsed() > deadline {
            // Diagnostics over screenshots (rule #19): dump composer + button
            // state, any visible toast, and the URL so failures are debuggable.
            let diag = page
                .evaluate(
                    r#"(() => {
                        const el = document.querySelector('[data-testid="tweetTextarea_0"]');
                        const btn = document.querySelector('[data-testid="tweetButtonInline"]');
                        const toast = document.querySelector('[data-testid="toast"], [role="alert"]');
                        return {
                            composer_chars: el ? (el.innerText || '').trim().length : -1,
                            btn_present: !!btn,
                            btn_disabled: btn ? (btn.disabled || btn.getAttribute('aria-disabled') === 'true') : null,
                            toast: toast ? (toast.innerText || '').slice(0, 200) : '',
                            url: location.href,
                        };
                    })()"#,
                )
                .await
                .unwrap_or(serde_json::Value::Null);
            tracing::error!(diagnostics = %diag, "twitter: composer never cleared — page state dump");
            anyhow::bail!(
                "twitter: composer never cleared after real+JS click — post failed (diag: {diag})"
            );
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Wait for `n` images to finish attaching to the composer after we set
/// them on the file input. Twitter uploads media to its CDN asynchronously
/// and shows a progress bar while doing so; clicking Post before that
/// finishes drops the media. Ready = `n` previews rendered and no upload
/// progress bar remains.
async fn wait_for_media_attached(page: &mut PageSession, n: usize) -> anyhow::Result<()> {
    let deadline = Duration::from_secs(60);
    let start = Instant::now();
    loop {
        let r = page
            .evaluate(
                r#"(() => {
                    const att = document.querySelector('[data-testid="attachments"]');
                    const inAtt = att ? att.querySelectorAll('img').length : 0;
                    const blobs = document.querySelectorAll('img[src^="blob:"]').length;
                    // Scope the upload spinner to the attachments container —
                    // a page-wide [role=progressbar] also matches timeline
                    // loaders, which never clear and would hang the wait.
                    const progress = att ? att.querySelectorAll('[role="progressbar"]').length : 0;
                    return { previews: Math.max(inAtt, blobs), progress };
                })()"#,
            )
            .await?;
        let previews = r.get("previews").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let progress = r.get("progress").and_then(|v| v.as_u64()).unwrap_or(0);
        if previews >= n && progress == 0 {
            tracing::info!(previews, "twitter: media attached");
            // Brief settle so Twitter commits the media IDs to the draft.
            tokio::time::sleep(Duration::from_millis(500)).await;
            return Ok(());
        }
        if start.elapsed() > deadline {
            anyhow::bail!(
                "twitter: media did not finish attaching within 60s \
                 (previews={previews}, progress={progress})"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Navigate to `/<handle>`, find the tweet whose first 30 chars match
/// `prefix`, open its caret menu, click Delete, confirm.
async fn delete_tweet_by_prefix(creds: &TwitterCredentials, prefix: &str) -> anyhow::Result<()> {
    let session = BrowserSession::connect(&creds.cdp_url).await?;
    let url = format!("https://x.com/{}", creds.handle);
    let tab = session.create_tab(&url).await?;
    let mut page = session.open_page(&tab).await?;

    // Wait for the timeline to render some articles.
    let start = Instant::now();
    loop {
        let n = page
            .evaluate(r#"document.querySelectorAll('article[role="article"]').length"#)
            .await?
            .as_u64()
            .unwrap_or(0);
        if n > 0 {
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            anyhow::bail!(
                "twitter: profile timeline empty after 30s — wrong handle or rate-limited?"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Locate the matching article's caret coords.
    let prefix_json = serde_json::to_string(prefix)?;
    let locate_js = format!(
        r#"(() => {{
            const want = {prefix_json};
            const articles = Array.from(document.querySelectorAll('article[role="article"]'));
            for (const art of articles) {{
                if (!(art.innerText || '').includes(want)) continue;
                const caret = art.querySelector('[data-testid="caret"]');
                if (!caret) continue;
                const r = caret.getBoundingClientRect();
                return {{x: r.x + r.width / 2, y: r.y + r.height / 2}};
            }}
            return null;
        }})()"#
    );
    let coord = page.evaluate(&locate_js).await?;
    let (cx, cy) = match (
        coord.get("x").and_then(|v| v.as_f64()),
        coord.get("y").and_then(|v| v.as_f64()),
    ) {
        (Some(x), Some(y)) => (x, y),
        _ => anyhow::bail!("twitter: no tweet matches prefix {prefix:?}"),
    };

    // Real-click the caret to open the menu. Twitter's caret is a
    // button so a JS click would work too, but using real input
    // keeps the path consistent with hover-revealed UI elsewhere.
    page.real_click(cx, cy).await?;
    tokio::time::sleep(Duration::from_millis(600)).await;
    tracing::info!(prefix, "twitter: opened caret menu");

    // Click the Delete menu item. The menu is a top-level portal so
    // we scan the whole page for an element whose direct text is
    // "Delete" and which is currently visible.
    let click_del_js = r#"(() => {
        const items = Array.from(document.querySelectorAll('div[role="menuitem"], [role="menuitem"]'))
            .filter(e => e.offsetParent !== null);
        const del = items.find(e => (e.innerText || '').trim() === 'Delete');
        if (!del) return {ok: false, reason: 'no Delete menuitem'};
        del.click();
        return {ok: true};
    })()"#;
    let r = page.evaluate(click_del_js).await?;
    if !r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        anyhow::bail!("click Delete failed: {r}");
    }
    tracing::info!("twitter: clicked Delete menu item");

    // Confirm modal — single "Delete" button on the dialog.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let confirm_js = r#"(() => {
        const cands = Array.from(document.querySelectorAll('button, div[role="button"]'))
            .filter(e => e.offsetParent !== null && (e.innerText || '').trim() === 'Delete');
        if (cands.length === 0) return {ok: false};
        // Pick the LAST visible Delete — the confirm dialog mounts on
        // top of the timeline so its Delete is later in DOM order.
        cands[cands.length - 1].click();
        return {ok: true};
    })()"#;
    let r = page.evaluate(confirm_js).await?;
    if !r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        anyhow::bail!("confirm Delete modal click failed: {r}");
    }
    tracing::info!("twitter: confirmed delete modal");

    // Best-effort: wait for the tweet to disappear from the timeline.
    let check_gone_js = format!(
        r#"(() => {{
            const want = {prefix_json};
            return !Array.from(document.querySelectorAll('article[role="article"]'))
                .some(a => (a.innerText || '').includes(want));
        }})()"#
    );
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if page
            .evaluate(&check_gone_js)
            .await?
            .as_bool()
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let _ = session.close_tab(&tab.id).await;
    Ok(())
}

/// Validate a tweet's content + attachments before we open a browser.
/// `body` must already be trimmed. Pure (no I/O) so it's unit-testable.
fn validate_tweet(kind: ContentKind, body: &str, media: &[MediaPayload]) -> Result<()> {
    // Twitter is short-form — long-form articles don't fit.
    if matches!(kind, ContentKind::Article) {
        return Err(PublishError::Rejected(
            "twitter: long-form Article content doesn't fit Twitter; \
             submit without --title for a tweet"
                .into(),
        ));
    }
    // A tweet needs *something*: text, image(s), or both. Image-only
    // tweets are valid, so empty body is only rejected with no media.
    if body.is_empty() && media.is_empty() {
        return Err(PublishError::Rejected(
            "twitter: nothing to post (empty body and no media)".into(),
        ));
    }
    if body.chars().count() > selectors::MAX_TWEET_CHARS {
        return Err(PublishError::Rejected(format!(
            "twitter: body is {} chars; cap is {}",
            body.chars().count(),
            selectors::MAX_TWEET_CHARS,
        )));
    }
    if media.len() > selectors::MAX_TWEET_IMAGES {
        return Err(PublishError::Rejected(format!(
            "twitter: {} images attached; cap is {}",
            media.len(),
            selectors::MAX_TWEET_IMAGES,
        )));
    }
    // Browser-driven upload only handles images; a video would need
    // Twitter's separate chunked-upload flow.
    if let Some(bad) = media.iter().find(|m| !m.mime_type.starts_with("image/")) {
        return Err(PublishError::Rejected(format!(
            "twitter: media {:?} is {}, only image/* is supported",
            bad.filename, bad.mime_type
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_credentials() {
        // Arrange
        let json = serde_json::json!({
            "cdp_url": "http://chrome-host:9222",
            "handle": "multipost_dev",
        });

        // Act
        let creds = parse_credentials(&json).unwrap();

        // Assert
        assert_eq!(creds.cdp_url, "http://chrome-host:9222");
        assert_eq!(creds.handle, "multipost_dev");
        assert!(creds.display_name.is_empty());
    }

    fn img(mime: &str) -> MediaPayload {
        MediaPayload {
            filename: format!("pic.{}", mime.rsplit('/').next().unwrap_or("bin")),
            mime_type: mime.to_string(),
            bytes: vec![0u8; 4],
        }
    }

    #[test]
    fn validate_rejects_article_kind() {
        // Arrange — Article is reserved for long-form; doesn't fit Twitter.
        let body = "anything";

        // Act
        let r = validate_tweet(ContentKind::Article, body, &[]);

        // Assert
        assert!(matches!(r, Err(PublishError::Rejected(_))));
    }

    #[test]
    fn validate_allows_image_only_tweet() {
        // Arrange — empty body but one image is a valid image-only tweet.
        let media = vec![img("image/png")];

        // Act
        let r = validate_tweet(ContentKind::Image, "", &media);

        // Assert
        assert!(r.is_ok());
    }

    #[test]
    fn validate_rejects_empty_body_and_no_media() {
        // Arrange — nothing to post.
        // Act
        let r = validate_tweet(ContentKind::Text, "", &[]);

        // Assert
        assert!(matches!(r, Err(PublishError::Rejected(_))));
    }

    #[test]
    fn validate_rejects_overlong_body() {
        // Arrange
        let body: String = "x".repeat(selectors::MAX_TWEET_CHARS + 1);

        // Act
        let r = validate_tweet(ContentKind::Text, &body, &[]);

        // Assert
        assert!(matches!(r, Err(PublishError::Rejected(_))));
    }

    #[test]
    fn validate_rejects_too_many_images() {
        // Arrange — 5 images exceeds Twitter's cap of 4.
        let media: Vec<_> = (0..selectors::MAX_TWEET_IMAGES + 1)
            .map(|_| img("image/jpeg"))
            .collect();

        // Act
        let r = validate_tweet(ContentKind::Carousel, "caption", &media);

        // Assert
        assert!(matches!(r, Err(PublishError::Rejected(_))));
    }

    #[test]
    fn validate_rejects_non_image_media() {
        // Arrange — browser upload only supports images, not video.
        let media = vec![img("video/mp4")];

        // Act
        let r = validate_tweet(ContentKind::Image, "caption", &media);

        // Assert
        assert!(matches!(r, Err(PublishError::Rejected(_))));
    }

    #[test]
    fn validate_accepts_text_plus_max_images() {
        // Arrange — text + exactly 4 images is the upper bound.
        let media: Vec<_> = (0..selectors::MAX_TWEET_IMAGES)
            .map(|_| img("image/png"))
            .collect();

        // Act
        let r = validate_tweet(ContentKind::Image, "caption", &media);

        // Assert
        assert!(r.is_ok());
    }
}
