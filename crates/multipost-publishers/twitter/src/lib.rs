//! Twitter / X publisher.
//!
//! Drives `x.com` via Chrome DevTools Protocol. Like Douyin and
//! Toutiao, each Twitter account corresponds to a Chrome profile that's
//! pre-logged-in. Credentials just point at the CDP endpoint and the
//! account handle (needed for the per-tweet delete flow).
//!
//! Selectors mapped from `scripts/twitter/03_compose_post.py`.

#![deny(missing_docs)]

pub mod cdp;
pub mod credentials;
pub mod publisher;
pub mod selectors;

pub use credentials::TwitterCredentials;
pub use publisher::TwitterPublisher;
