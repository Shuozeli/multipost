//! YouTube Data API v3 publisher.
//!
//! Provides OAuth helpers (start URL, code exchange, refresh) plus a full
//! `Publisher` implementation for `Platform::YouTube`.
//!
//! Ported from the Python prototype in `scripts/youtube/`. All endpoint
//! paths and field shapes match what the prototype validated against a
//! real YouTube account.

#![deny(missing_docs)]

pub mod auth;
pub mod publisher;

pub use auth::{OAuthCredentials, OAuthTokens, exchange_code, refresh_token, start_oauth_url};
pub use publisher::YouTubePublisher;

/// Default OAuth scopes we request. Equivalent to `scripts/youtube/05_oauth_login.py`.
pub const SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/youtube.readonly",
    "https://www.googleapis.com/auth/youtube.upload",
    "https://www.googleapis.com/auth/youtube",
];

/// YouTube Data API v3 base URL.
pub const API_BASE: &str = "https://www.googleapis.com/youtube/v3";

/// Google OAuth 2.0 authorization endpoint.
pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";

/// Google OAuth 2.0 token endpoint.
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
