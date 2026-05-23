//! DOM selectors / text matchers for mp.toutiao.com.
//!
//! Mapped from `scripts/toutiao/03_push_article.py`. The Toutiao
//! publish editor uses a ProseMirror/Lexical-style contenteditable for
//! the body, so we fill it via `execCommand('insertText')` rather than
//! mutating innerHTML.

/// Direct URL to the article publish editor.
pub const PUBLISH_URL: &str =
    "https://mp.toutiao.com/profile_v4/graphic/publish?source=mp_creation_main";

/// Where the works list lives. The 草稿箱 sub-tab is what we navigate
/// `delete` against — Toutiao's auto-save lands new content there, not
/// in the default "已发布" view.
pub const MANAGE_URL: &str = "https://mp.toutiao.com/profile_v4/graphic/manage";

/// Path to the article list page that hosts the 草稿箱 tab.
pub const ARTICLES_URL: &str = "https://mp.toutiao.com/profile_v4/graphic/articles";

/// Per-draft row container under the 草稿箱 tab.
/// Class fragment we walk up to from the title leaf — full class is
/// `article-draft-item draft-item`.
pub const DRAFT_ROW_CLASS: &str = "article-draft-item";

/// Label text on the per-row delete button (Toutiao renders it as
/// `a.op-button` inside the row).
pub const ROW_DELETE_LABEL: &str = "删除";

/// 草稿箱 tab label — we click this after landing on `ARTICLES_URL` to
/// switch from the default "已发布" view.
pub const DRAFTS_TAB_LABEL: &str = "草稿箱";

/// URL signatures that confirm a logged-in `mp.toutiao.com` session.
/// The dashboard lives at `/profile_v4/index`; specific tools are under
/// `/graphic`, `/video`, `/weitoutiao`, etc. — any path under
/// `/profile_v4/` (other than the login/passport flow) means we're in.
pub const LOGGED_IN_URL_SIGNATURES: &[&str] = &["/profile_v4/"];

/// Onboarding tooltip labels to dismiss before interacting with the form.
pub const TOOLTIP_DISMISS_LABELS: &[&str] = &["我知道了", "知道了", "关闭", "跳过"];

/// CSS for the article title input. Matches by placeholder substring
/// since the rendered class names are auto-generated and change per
/// deploy.
pub const TITLE_INPUT: &str = r#"textarea[placeholder*="标题"], input[placeholder*="标题"]"#;

/// CSS for the body editor. There are multiple `contenteditable=true`
/// elements on the page (toolbar fields, captions, etc.); we further
/// filter by bounding box (≥400×100) in the JS scan to pick the main
/// body editor.
pub const BODY_EDITOR: &str = r#"[contenteditable="true"]"#;
