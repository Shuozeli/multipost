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

// ---- 微头条 (Weitoutiao) — Toutiao's short-form social post ----

/// Direct URL to the 微头条 publish editor.
pub const WEITOUTIAO_URL: &str = "https://mp.toutiao.com/profile_v4/weitoutiao/publish";

/// Where 微头条 posts list lives. Note: the editor's "manage" is just
/// the bare `/weitoutiao` dashboard, NOT `/weitoutiao/manage` (which
/// 404s into an empty page).
pub const WEITOUTIAO_MANAGE_URL: &str = "https://mp.toutiao.com/profile_v4/weitoutiao";

/// Text label on the 微头条 publish button. Auto-publish clicks this.
pub const WEITOUTIAO_PUBLISH_LABEL: &str = "发布";

/// The 微头条 toolbar's image button. Clicking it opens the
/// "上传图片" panel (`.upload-image-panel`) which lazily mounts the file
/// inputs. The button's only text is "图片" — the toolbar also has
/// 图片-labeled wrapper span/div elements that must NOT be clicked.
pub const WEITOUTIAO_IMAGE_BUTTON_LABEL: &str = "图片";

/// CSS for the upload panel's image `<input type="file">`. The panel has
/// two (a browse button + a drag target), both `accept="image/*"
/// multiple`; `querySelector` picks the first (browse), which is the one
/// the live probe confirmed triggers the upload on `change`.
pub const WEITOUTIAO_IMAGE_INPUT: &str = r#"input[type="file"][accept*="image"]"#;

/// The "上传图片" modal panel container — used to detect when it has
/// closed after clicking 确定 to insert the images.
pub const WEITOUTIAO_UPLOAD_PANEL: &str = ".upload-image-panel, .upload-handler-drag";

/// Label on the panel's confirm button that inserts the uploaded images
/// into the post.
pub const WEITOUTIAO_INSERT_CONFIRM_LABEL: &str = "确定";

/// 微头条 accepts up to 9 images per post.
pub const WEITOUTIAO_MAX_IMAGES: usize = 9;

// ---- 头条视频 / 西瓜视频 upload editor ----

/// Direct URL to the video upload editor.
pub const VIDEO_UPLOAD_URL: &str = "https://mp.toutiao.com/profile_v4/xigua/upload-video";

/// Hidden video file input in the upload editor.
pub const VIDEO_FILE_INPUT: &str = r#"input[type="file"][accept*="video"]"#;

/// Video title input placeholder.
pub const VIDEO_TITLE_INPUT: &str = r#"input[placeholder*="1～30"], input[placeholder*="1-30"]"#;

/// Video description textarea placeholder.
pub const VIDEO_DESCRIPTION_TEXTAREA: &str = r#"textarea[placeholder*="视频简介"]"#;

/// Video publish button label.
pub const VIDEO_PUBLISH_LABEL: &str = "发布";
