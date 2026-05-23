//! CSS selectors for creator.douyin.com.
//!
//! All selectors were captured by `scripts/douyin/{02,05,06,07}_*.py` against
//! a logged-in creator center. They may need updating if Douyin redesigns
//! the publish UI.

/// Hidden `<input type="file">` for the video. Sized 1×1 px; the visible
/// "上传视频" button at (908, 481) just clicks-through to this.
pub const FILE_INPUT: &str = r#"input[type="file"]"#;

/// Article title input on the post-upload form. The placeholder is
/// "填写作品标题，为作品获得更多流量". Several other inputs share the
/// `semi-input` class (售价, 付费视频标题, …), so we disambiguate by
/// the title placeholder substring.
pub const TITLE_INPUT: &str =
    r#"input.semi-input.semi-input-default[placeholder*="填写作品标题"]"#;

/// Description / caption rich-text editor. ProseMirror under the hood;
/// supports `#hashtags` and `@mentions` inline. Char limit 1000.
pub const DESCRIPTION_EDITOR: &str =
    r#"div[contenteditable="true"].editor-kit-container.editor-comp-publish"#;

/// "公开" (Public) visibility radio label.
pub const VISIBILITY_PUBLIC_LABEL: &str = "公开";
/// "好友可见" (Friends) visibility radio label.
pub const VISIBILITY_FRIENDS_LABEL: &str = "好友可见";
/// "私密" (Private) visibility radio label, if present on the account type.
pub const VISIBILITY_PRIVATE_LABEL: &str = "私密";

/// "立即发布" (Publish now) radio label.
pub const SCHEDULE_NOW_LABEL: &str = "立即发布";

/// The main `发布` submit button on the post-upload form.
pub const PUBLISH_BUTTON: &str = "button.button-dhlUZE.primary-cECiOJ.fixed-J9O8Yw";

/// Onboarding tooltip dismiss labels. We loop through these and click any
/// that appear before interacting with the form.
pub const TOOLTIP_DISMISS_LABELS: &[&str] = &["我知道了", "知道了", "关闭", "跳过"];

/// URL signatures that signal a logged-in creator center session.
/// Used by `check_auth` to verify the Chrome profile still has a valid
/// Douyin login.
pub const LOGGED_IN_URL_SIGNATURES: &[&str] =
    &["/creator-micro/home", "/creator-micro/content", "/creator-micro/data"];

/// Works-management page. `confirm` / `delete` open a fresh tab here.
pub const MANAGE_URL: &str = "https://creator.douyin.com/creator-micro/content/manage";

/// CSS class fragment on each row's outer container in the manage page.
pub const MANAGE_ROW_CLASS: &str = "card-container-creator-layout";

/// Per-row "delete" affordance label (rendered as a button-like element
/// inside the row's hover-action group).
pub const ROW_DELETE_LABEL: &str = "删除作品";

/// Status labels we can read off a row to map to `ConfirmStatus`.
/// Order matters — longer/more-specific strings before shorter ones so
/// e.g. "审核未通过" isn't mis-matched as "审核中".
pub const ROW_STATUSES: &[&str] = &["审核未通过", "未通过", "审核中", "已发布"];
