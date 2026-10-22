//! What Unico answers, as this module names it (spec 5.2 to 5.5): a journey, where it stands
//! and the ways a call can fail.

use serde_json::Value;

#[derive(Debug, PartialEq, Eq)]
pub enum UnicoError {
    /// `429` saying the monthly credits are over. Never retried.
    Limit,
    /// `429` that persisted through every retry.
    Busy,
    /// `5xx`, timeout or connection failure after the retry.
    Unavailable,
    /// `401`/`403`, or no access key configured.
    Misconfigured,
    /// A process state or response shape we do not handle.
    UnexpectedState(String),
    /// Any other `4xx`; carries the response body for the caller to interpret.
    Rejected(Value),
    /// API-only flow: finished without a token, or failed.
    Inconclusive,
    NoFace,
    /// Any other refusal of the image by Unico: it will refuse it again, so it is not repeated.
    PhotoRejected,
    InvalidImage,
    ImageTooLarge,
    /// The degraded mode is on (spec 5.7): no call was made.
    Degraded,
}

/// A journey created for a capture (spec 5.2, step 1).
#[derive(Debug, PartialEq, Eq)]
pub struct Journey {
    pub process_id: String,
    pub link: String,
}

/// Where a journey stands (spec 5.2, step 3).
#[derive(Debug, PartialEq, Eq)]
pub enum ProcessResult {
    Pending,
    Token(String),
    Inconclusive,
    Failed,
    Canceled,
}
