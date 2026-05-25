//! Generated tonic + prost code for multipost's gRPC API.

#![allow(clippy::derive_partial_eq_without_eq)]

/// Shared message types referenced by other services.
pub mod common {
    tonic::include_proto!("common");
}

/// Account management service.
pub mod accounts {
    tonic::include_proto!("accounts");
}

/// Post submission + job control service.
pub mod posts {
    tonic::include_proto!("posts");
}

/// Media upload service.
pub mod media {
    tonic::include_proto!("media");
}

/// Content-discovery (crawler) service.
pub mod crawl {
    tonic::include_proto!("crawl");
}
