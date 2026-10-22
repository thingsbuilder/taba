//! Degraded mode (spec 5.5, 5.7): `system/status`, the monthly limit, three failures in a row,
//! the `503 unico_degraded` gate and `taba unico resume`.
//!
//! The state lives in one document so that `taba unico resume`, a separate process, takes effect
//! on the next request. `unico.rs` reads it before every call and calls `enter_limit` and
//! `enter_unavailable` from the outcome of the calls.
//!
//! Layers, one file each: `model` (the stored document), `domain` (the rules over it) and
//! `persist` (the reads and transactions). There are no routes of its own (`status.rs` serves
//! `GET /v1/status`), so no `http` or `view`.

// The audit action stays here: the conformance tests look for it in this file.
const ACTION_RESUME: &str = "unico.resume";

mod domain;
mod model;
mod persist;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub use model::*;
#[cfg(test)]
pub use persist::read;
pub use persist::{blocking_reason, enter_limit, enter_unavailable, resume};
