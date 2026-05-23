//! Job orchestrator.
//!
//! Phase 0–1c: thin re-export of `multipost_core::JobState`. Real scheduling
//! + retries land in Phase 5 when this crate gets its own runtime.

#![deny(missing_docs)]

pub use multipost_core::JobState;
