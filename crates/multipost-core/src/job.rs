//! Job state machine.
//!
//! Lives in `core` because both `storage` (persists it) and `orchestrator`
//! (transitions it) need it.

use serde::{Deserialize, Serialize};

/// State of a publish job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Job created, not yet picked up.
    Queued,
    /// Validating against the publisher's capabilities.
    Validating,
    /// Uploading media (if the publisher needs media pre-uploaded).
    Uploading,
    /// Calling `Publisher::publish`.
    Submitting,
    /// Polling `Publisher::confirm` for async platforms.
    Confirming,
    /// Permalink stored; job is done successfully.
    Confirmed,
    /// Job exhausted retries or hit a permanent error.
    Failed,
    /// User cancelled before submission completed.
    Cancelled,
}

impl JobState {
    /// True if the job is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobState::Confirmed | JobState::Failed | JobState::Cancelled
        )
    }
}
