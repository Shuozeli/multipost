//! `Publisher` implementation for Toutiao.
//!
//! - `check_auth`: REST probe — open a fresh tab at `mp.toutiao.com`,
//!   poll for redirect to `/profile_v4/*` (logged-in) vs login (expired).
//! - `publish`: open the publish editor, fill title + body via CDP,
//!   STOP before any submit. Toutiao auto-saves to 草稿箱 as you type;
//!   the user clicks 预览并发布 manually.
//! - `confirm` / `delete`: stubs for now; mirror the Douyin manage-page
//!   pattern when we wire them.

use async_trait::async_trait;
use multipost_core::{
    AuthStatus, Capabilities, ConfirmStatus, Content, MediaPayload, Platform, PublishContext,
    PublishError, PublishHandle, Publisher, Result,
};
use std::time::{Duration, Instant};

use crate::cdp::{BrowserSession, PageSession};
use crate::credentials::ToutiaoCredentials;
use crate::selectors;

/// Toutiao publisher. Stateless — all per-account config lives in
/// `PublishContext::credentials`.
pub struct ToutiaoPublisher;

impl ToutiaoPublisher {
    /// Construct a new publisher.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ToutiaoPublisher {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_credentials(value: &serde_json::Value) -> Result<ToutiaoCredentials> {
    serde_json::from_value::<ToutiaoCredentials>(value.clone()).map_err(|e| {
        PublishError::Other(anyhow::anyhow!(
            "credentials don't deserialize as ToutiaoCredentials: {e}"
        ))
    })
}

/// Probe Toutiao's auth state by creating a fresh tab at `mp.toutiao.com`
/// and reading the redirected URL via the HTTP `/json` endpoint. Same
/// REST-only pattern as the Douyin check_auth — no WebSocket attach, so
/// it doesn't race with any stale CDP clients on existing tabs.
async fn probe_creator_url(cdp_url: &str) -> anyhow::Result<String> {
    let session = BrowserSession::connect(cdp_url).await?;
    let new_tab = session
        .create_tab("https://mp.toutiao.com/profile_v4")
        .await?;
    tracing::debug!(target_id = %new_tab.id, "toutiao: created probe tab");

    let mut url = new_tab.url.clone();
    // Toutiao SPA can take 15+ seconds to settle on a final URL.
    for _ in 0..25 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some(t) = session.get_target(&new_tab.id).await? {
            url = t.url.clone();
            if url.contains("/profile_v4/")
                || url.contains("login")
                || url.contains("passport")
            {
                break;
            }
        }
    }

    let _ = session.close_tab(&new_tab.id).await;
    Ok(url)
}

#[async_trait]
impl Publisher for ToutiaoPublisher {
    fn platform(&self) -> Platform {
        Platform::Toutiao
    }

    fn capabilities(&self) -> Capabilities {
        // From scripts/toutiao discovery. Toutiao's article editor caps
        // title at 30 chars; body is essentially unbounded (long-form
        // article is the whole point).
        Capabilities {
            max_text_chars: None,
            max_images: Some(20), // editor supports inline images; we don't use yet
            video_supported: false,
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
        // Browser-cookie auth — no tokens to refresh.
        Ok(None)
    }

    async fn check_auth(&self, ctx: &PublishContext<'_>) -> Result<AuthStatus> {
        let creds = parse_credentials(ctx.credentials)?;
        let url = probe_creator_url(&creds.cdp_url)
            .await
            .map_err(|e| PublishError::Transient(format!("toutiao probe: {e}")))?;
        tracing::info!(url = %url, "toutiao: probe-tab final URL");

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
        let (title, body) = split_title_and_body(content);

        // Toutiao caps the title at 30 chars; trim before posting.
        if title.chars().count() > 30 {
            return Err(PublishError::Rejected(format!(
                "toutiao: title is {} chars; cap is 30",
                title.chars().count()
            )));
        }
        if title.is_empty() {
            return Err(PublishError::Rejected(
                "toutiao: content.text must start with a non-empty title line".into(),
            ));
        }

        let session = BrowserSession::connect(&creds.cdp_url)
            .await
            .map_err(|e| PublishError::Transient(format!("toutiao connect: {e}")))?;
        let tab = session
            .create_tab(selectors::PUBLISH_URL)
            .await
            .map_err(|e| PublishError::Transient(format!("toutiao create_tab: {e}")))?;
        tracing::info!(target = %tab.id, "toutiao: opened publish editor tab");
        let mut page = session
            .open_page(&tab)
            .await
            .map_err(|e| PublishError::Transient(format!("toutiao open_page: {e}")))?;

        let result = run_publish_flow(&mut page, &title, &body).await;
        result.map(|_| PublishHandle {
            // Toutiao doesn't surface an aweme_id at publish time, and we
            // don't click 预览并发布 — so external_id is the article title
            // (same convention as Douyin's row-lookup-by-title approach).
            external_id: title.to_string(),
            permalink: None,
        })
    }

    async fn confirm(
        &self,
        _ctx: &PublishContext<'_>,
        _handle: &PublishHandle,
    ) -> Result<ConfirmStatus> {
        // Toutiao auto-saves to 草稿箱 as the body is typed. The publish
        // flow stops *before* 预览并发布, so there's no platform-side
        // moderation to poll. Treating as immediately Confirmed: the
        // draft exists; user finalizes manually.
        Ok(ConfirmStatus::Confirmed {
            permalink: Some(selectors::MANAGE_URL.to_string()),
        })
    }

    async fn delete(&self, ctx: &PublishContext<'_>, handle: &PublishHandle) -> Result<()> {
        let creds = parse_credentials(ctx.credentials)?;
        let title = handle.external_id.as_str();
        delete_draft_by_title(&creds, title)
            .await
            .map_err(|e| PublishError::Transient(format!("toutiao delete: {e}")))?;
        tracing::info!(title, "toutiao: deleted draft");
        Ok(())
    }
}

/// Open the 草稿箱 view, locate the draft row whose title matches `title`,
/// click its 删除 button, and confirm the modal. Mirrors the Douyin
/// `delete_row` pattern; uses Toutiao-specific selectors mapped from the
/// `mp.toutiao.com/profile_v4/graphic/articles` DOM.
async fn delete_draft_by_title(creds: &ToutiaoCredentials, title: &str) -> anyhow::Result<()> {
    let session = BrowserSession::connect(&creds.cdp_url).await?;
    let tab = session.create_tab(selectors::ARTICLES_URL).await?;
    let mut page = session.open_page(&tab).await?;

    // Click the 草稿箱 tab — the URL doesn't change, the tab swap is
    // React-state-only.
    let drafts_label = serde_json::to_string(selectors::DRAFTS_TAB_LABEL)?;
    let click_drafts_js = format!(
        r#"(() => {{
            const want = {drafts_label};
            const cand = Array.from(document.querySelectorAll('a, span, button, li, div'))
                .find(el => el.children.length === 0 && (el.innerText || '').trim() === want);
            if (!cand) return {{ok: false, reason: 'no 草稿箱 tab'}};
            cand.click();
            return {{ok: true}};
        }})()"#
    );
    // Articles page is XHR-heavy; the 草稿箱 tab may not be in the DOM
    // immediately. Poll up to 30s for the click to land.
    let start = Instant::now();
    loop {
        let r = page.evaluate(&click_drafts_js).await?;
        if r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            anyhow::bail!("could not find 草稿箱 tab on /graphic/articles");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    tracing::info!("toutiao: clicked 草稿箱 tab");

    // Wait for at least one draft row to render.
    let row_cls_json = serde_json::to_string(selectors::DRAFT_ROW_CLASS)?;
    let wait_js = format!(
        r#"(() => document.querySelectorAll(`[class*=${{JSON.stringify({row_cls_json}).slice(1, -1)}}]`).length)()"#,
    );
    // The above is overcooked — simpler:
    let wait_rows_js = format!(
        r#"document.querySelectorAll('[class*="{row_class}"]').length"#,
        row_class = selectors::DRAFT_ROW_CLASS,
    );
    let _ = wait_js; // unused; keep `wait_rows_js`
    let start = Instant::now();
    loop {
        let n = page
            .evaluate(&wait_rows_js)
            .await?
            .as_u64()
            .unwrap_or(0);
        if n > 0 {
            tracing::info!(rows = n, "toutiao: draft rows visible");
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            anyhow::bail!("草稿箱 has no rendered draft rows after 30s");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Click 删除 inside the row matching `title`.
    let title_json = serde_json::to_string(title)?;
    let del_label_json = serde_json::to_string(selectors::ROW_DELETE_LABEL)?;
    let row_cls_str = selectors::DRAFT_ROW_CLASS;
    let click_del_js = format!(
        r#"(() => {{
            const want = {title_json};
            const delLabel = {del_label_json};
            const rows = Array.from(document.querySelectorAll('[class*="{row_cls_str}"]'));
            for (const row of rows) {{
                const titleLeaf = row.querySelector('.title, a.title');
                const t = (titleLeaf?.innerText || '').trim();
                if (t !== want && !t.startsWith(want)) continue;
                // Find 删除 inside this row
                const btn = Array.from(row.querySelectorAll('*'))
                    .find(el => el.children.length === 0 && (el.innerText || '').trim() === delLabel);
                if (!btn) return {{ok: false, reason: 'no 删除 button in matching row'}};
                btn.click();
                return {{ok: true}};
            }}
            return {{ok: false, reason: 'no row matches title'}};
        }})()"#
    );
    let r = page.evaluate(&click_del_js).await?;
    if !r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        anyhow::bail!("click 删除 failed: {r}");
    }
    tracing::info!(title, "toutiao: clicked 删除");

    // Confirm the modal.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let confirm_js = r#"(() => {
        const wanted = ['确定', '确认', '确认删除', '删除'];
        for (const b of document.querySelectorAll('button')) {
            if (b.offsetParent === null) continue;
            const t = (b.innerText || '').trim();
            if (wanted.includes(t)) { b.click(); return {ok: true, text: t}; }
        }
        return {ok: false};
    })()"#;
    let r = page.evaluate(confirm_js).await?;
    if !r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        anyhow::bail!("confirm modal click failed: {r}");
    }
    tracing::info!("toutiao: confirmed delete modal");

    // Best-effort: wait briefly for the row to disappear.
    let check_js = format!(
        r#"(() => {{
            const want = {title_json};
            const rows = Array.from(document.querySelectorAll('[class*="{row_cls_str}"]'));
            return !rows.some(r => {{
                const t = (r.querySelector('.title, a.title')?.innerText || '').trim();
                return t === want || t.startsWith(want);
            }});
        }})()"#
    );
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

/// Split `content.text` into (title, body):
/// - title is the first non-empty line, with a leading `# ` heading
///   marker stripped if present (so callers can pass either a
///   bare title or a fully-formed markdown document).
/// - body is everything after that line; markdown markers are stripped
///   so Toutiao's rich-text editor receives plain text.
fn split_title_and_body(content: &Content) -> (String, String) {
    let mut lines = content.text.lines();
    let title = loop {
        match lines.next() {
            None => break String::new(),
            Some(ln) => {
                let trimmed = ln.trim();
                if trimmed.is_empty() {
                    continue;
                }
                break trimmed
                    .strip_prefix("# ")
                    .unwrap_or(trimmed)
                    .to_string();
            }
        }
    };
    let rest: String = lines.collect::<Vec<_>>().join("\n");
    let body = strip_markdown(rest.trim_start_matches('\n'));
    (title, body)
}

/// Plain-text projection of markdown — same rules as the Python
/// `strip_markdown()` in `scripts/toutiao/03_push_article.py`.
/// Keeps section headers as standalone lines, drops the ## / ### / **bold**
/// markers, preserves paragraph breaks.
fn strip_markdown(md: &str) -> String {
    let mut out = Vec::new();
    for ln in md.lines() {
        let l = if let Some(rest) = ln.strip_prefix("### ") {
            rest.to_string()
        } else if let Some(rest) = ln.strip_prefix("## ") {
            rest.to_string()
        } else {
            ln.to_string()
        };
        out.push(strip_bold_inline(&l));
    }
    out.join("\n")
}

/// Strip `**bold**` markers, keeping the inner text. Naive — does not
/// handle escaped asterisks, nested marks, or `*italic*`. Sufficient for
/// our financial-digest article style.
fn strip_bold_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '*' && chars.peek() == Some(&'*') {
            chars.next(); // consume the second '*'
            continue;
        }
        out.push(c);
    }
    out
}

async fn run_publish_flow(
    page: &mut PageSession,
    title: &str,
    body: &str,
) -> Result<()> {
    // Wait for the publish editor to mount. Toutiao's SPA hydrates the
    // title input + the contenteditable body within ~5s on a warm Chrome,
    // longer on a cold one — give it 45s headroom.
    let deadline = Duration::from_secs(45);
    let start = Instant::now();
    loop {
        let probe = page
            .evaluate(
                r#"(() => {
                    const titleEl = document.querySelector('textarea[placeholder*="标题"]')
                        || document.querySelector('input[placeholder*="标题"]');
                    const editors = Array.from(document.querySelectorAll('[contenteditable="true"]'));
                    const bodyEl = editors.find(el => {
                        const r = el.getBoundingClientRect();
                        return r.width > 400 && r.height > 100;
                    });
                    return { title: !!titleEl, body: !!bodyEl };
                })()"#,
            )
            .await
            .map_err(|e| PublishError::Transient(format!("toutiao probe form: {e}")))?;
        let ok_title = probe.get("title").and_then(|v| v.as_bool()).unwrap_or(false);
        let ok_body = probe.get("body").and_then(|v| v.as_bool()).unwrap_or(false);
        if ok_title && ok_body {
            break;
        }
        if start.elapsed() > deadline {
            return Err(PublishError::Transient(
                "toutiao publish editor did not mount within 45s".into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    }
    tracing::info!("toutiao: publish editor mounted");

    // Dismiss any onboarding tooltips.
    let _ = dismiss_tooltips(page).await;

    fill_title(page, title)
        .await
        .map_err(|e| PublishError::Transient(format!("toutiao title fill: {e}")))?;

    fill_body(page, body)
        .await
        .map_err(|e| PublishError::Transient(format!("toutiao body fill: {e}")))?;

    Ok(())
}

async fn dismiss_tooltips(page: &mut PageSession) -> anyhow::Result<()> {
    let labels = serde_json::to_string(selectors::TOOLTIP_DISMISS_LABELS)?;
    let js = format!(
        r#"(() => {{
            const labels = {labels};
            let n = 0;
            for (const el of document.querySelectorAll('button, span, div[role="button"]')) {{
                const t = (el.innerText || '').trim();
                if (labels.includes(t) && el.offsetParent !== null) {{
                    el.click();
                    n++;
                }}
            }}
            return n;
        }})()"#
    );
    let r = page.evaluate(&js).await?;
    tracing::info!(dismissed = ?r, "toutiao: tooltip pass");
    Ok(())
}

/// Set the title via React-aware input setter + dispatch input event.
async fn fill_title(page: &mut PageSession, title: &str) -> anyhow::Result<()> {
    let value = serde_json::to_string(title)?;
    let js = format!(
        r#"(() => {{
            const want = {value};
            const el = document.querySelector('textarea[placeholder*="标题"]')
                || document.querySelector('input[placeholder*="标题"]');
            if (!el) return {{ok: false, reason: 'no title element'}};
            const proto = el.tagName === 'TEXTAREA'
                ? window.HTMLTextAreaElement.prototype
                : window.HTMLInputElement.prototype;
            const setter = Object.getOwnPropertyDescriptor(proto, 'value').set;
            setter.call(el, want);
            el.dispatchEvent(new Event('input', {{bubbles: true}}));
            el.dispatchEvent(new Event('change', {{bubbles: true}}));
            return {{ok: true, value: el.value}};
        }})()"#
    );
    let r = page.evaluate(&js).await?;
    if !r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        anyhow::bail!("title fill failed: {r}");
    }
    Ok(())
}

/// Fill the contenteditable body via `execCommand('insertText')`.
/// Same chunking as the Python script (800-char chunks) to keep
/// individual `Runtime.evaluate` calls under WebSocket frame limits.
async fn fill_body(page: &mut PageSession, body: &str) -> anyhow::Result<()> {
    if body.is_empty() {
        return Ok(());
    }
    // Focus the body editor first.
    let focus_js = r#"(() => {
        const editors = Array.from(document.querySelectorAll('[contenteditable="true"]'));
        const body = editors.find(el => {
            const r = el.getBoundingClientRect();
            return r.width > 400 && r.height > 100;
        });
        if (!body) return {ok: false, reason: 'no body editor'};
        body.focus();
        return {ok: true};
    })()"#;
    let r = page.evaluate(focus_js).await?;
    if !r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        anyhow::bail!("focus body failed: {r}");
    }
    // Chunk into 800-char windows so Runtime.evaluate doesn't carry a
    // multi-MB JS string when bodies get long.
    for chunk in body.chars().collect::<Vec<_>>().chunks(800) {
        let piece: String = chunk.iter().collect();
        let val = serde_json::to_string(&piece)?;
        let js = format!(
            r#"(() => {{
                document.execCommand('insertText', false, {val});
                return true;
            }})()"#
        );
        let _ = page.evaluate(&js).await?;
        // Small pause so the editor commits batched input events between
        // chunks; Lexical/ProseMirror sometimes drops chunks fired in a
        // tight loop.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

#[allow(dead_code)]
fn _link_media_payload(_: &MediaPayload) {} // keep type imported for future image-attach work

#[cfg(test)]
mod tests {
    use super::*;
    use multipost_core::{ContentKind, Visibility};
    use uuid::Uuid;

    fn mk(text: &str) -> Content {
        Content {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
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
    fn split_strips_h1_marker() {
        // Arrange
        let c = mk("# 财经早报 2026年5月21日\n\nparagraph one\n\nparagraph two");

        // Act
        let (title, body) = split_title_and_body(&c);

        // Assert
        assert_eq!(title, "财经早报 2026年5月21日");
        assert!(body.contains("paragraph one"));
        assert!(body.contains("paragraph two"));
    }

    #[test]
    fn split_handles_bare_title() {
        // Arrange — no `# ` prefix.
        let c = mk("Bare title\n\nthe body");

        // Act
        let (title, body) = split_title_and_body(&c);

        // Assert
        assert_eq!(title, "Bare title");
        assert_eq!(body, "the body");
    }

    #[test]
    fn strip_markdown_drops_bold_and_section_marks() {
        // Arrange
        let md = "## 一、 国际\n\n* **要点**：内容\n* 另一条";

        // Act
        let s = strip_markdown(md);

        // Assert
        assert!(s.contains("一、 国际"), "section header should remain");
        assert!(!s.contains("**"), "bold markers should be stripped");
        assert!(s.contains("要点"), "inner bold text should remain");
    }

    #[test]
    fn capabilities_sane() {
        // Arrange/Act
        let p = ToutiaoPublisher::new();
        let c = p.capabilities();

        // Assert
        assert_eq!(p.platform(), Platform::Toutiao);
        assert!(!c.video_supported);
        assert_eq!(c.max_text_chars, None);
    }
}
