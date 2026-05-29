//! WeChat MP (公众号) publisher.
//!
//! Ported from `scripts/wechat-mp/`. Implements `Publisher` for
//! `Platform::WxGzh`. Two notable quirks:
//!
//! - **No OAuth.** Credentials are `appid` + `app_secret`. We mint short-lived
//!   access tokens via `cgi-bin/stable_token` and cache them in the credentials
//!   JSON (refreshed by `refresh_credentials`).
//!
//! - **Two-stage publishing.** `publish()` creates a draft via `cgi-bin/draft/add`
//!   and attempts `cgi-bin/freepublish/submit`. Individual subscription accounts
//!   get 48001 on submit; we surface that as a clear `Rejected` error pointing
//!   the user at the MP admin web UI.
//!
//! - **IP whitelist.** WxGzh API calls fail with 40164 unless the caller's
//!   public IP is in the MP admin's whitelist. We surface this as `Transient`
//!   with a hint.

#![deny(missing_docs)]

pub mod auth;
pub mod publisher;

pub use auth::{WxGzhCredentials, check_account_info, ensure_access_token};
pub use publisher::WxGzhPublisher;

/// WeChat MP API base.
pub const API_BASE: &str = "https://api.weixin.qq.com";
