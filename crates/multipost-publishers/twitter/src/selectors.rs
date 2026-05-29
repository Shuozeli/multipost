//! DOM selectors / text matchers for x.com.
//!
//! Mapped from `scripts/twitter/03_compose_post.py`. Twitter's composer
//! is a Lexical/Draft-style contenteditable that listens for InputEvent.
//! The publisher fills it with per-keystroke CDP `Input.insertText` (a
//! trusted beforeinput/input path Draft honors); the older untrusted
//! `execCommand('insertText')` bulk fill was an automation tell and is no
//! longer used.

/// Compose landing page. We use the inline composer on /home rather
/// than /compose/post — the latter opens a MODAL on top of the
/// timeline that duplicates `tweetTextarea_0` in the DOM and the
/// queries pick the wrong one.
pub const COMPOSE_URL: &str = "https://x.com/home";

/// The contenteditable body of the inline composer.
pub const TEXTAREA: &str = r#"[data-testid="tweetTextarea_0"]"#;

/// The Post button on the inline composer. Distinct test-id from the
/// modal composer's `tweetButton` so the two don't collide.
pub const POST_BUTTON: &str = r#"[data-testid="tweetButtonInline"]"#;

/// URL signatures that indicate a logged-in session.
pub const LOGGED_IN_URL_SIGNATURES: &[&str] = &["x.com/home", "x.com/i/"];

/// URL signatures indicating Twitter punted us to login / re-auth.
pub const LOGIN_URL_SIGNATURES: &[&str] = &["/i/flow/login", "/login", "/i/flow/signup"];

/// Per-tweet "more" menu trigger on a timeline article. Opens the menu
/// with Delete + a few other items. Each tweet has its own caret.
pub const TWEET_CARET: &str = r#"[data-testid="caret"]"#;

/// The actual delete menu item in the dropdown that opens after
/// clicking caret. Twitter labels this "Delete".
pub const DELETE_MENU_LABEL: &str = "Delete";

/// Confirmation dialog button after clicking Delete.
pub const DELETE_CONFIRM_LABEL: &str = "Delete";

/// A timeline tweet container — `article` element with role.
pub const TWEET_ARTICLE: &str = r#"article[role="article"]"#;

/// Twitter's hard cap on tweet body length for non-Premium accounts.
/// Premium / X users get 25,000, but we conservatively reject above
/// 280 since most accounts are on the base tier.
pub const MAX_TWEET_CHARS: usize = 280;

/// The composer's hidden `<input type="file">` for image/video uploads.
/// We stream image bytes into it over CDP (the Chrome is remote).
pub const FILE_INPUT: &str = r#"[data-testid="fileInput"]"#;

/// Twitter accepts at most 4 images per tweet.
pub const MAX_TWEET_IMAGES: usize = 4;

/// Container that holds the media previews once images are attached.
/// Used to detect upload completion (previews rendered) and post success
/// (container cleared after submit).
pub const ATTACHMENTS: &str = r#"[data-testid="attachments"]"#;
