//! The hourly task of `serve` (spec 6.2.6 items 1, 2, 4): closes events, raises `search_expiring`,
//! deletes thumbnails, expires captures, invites and endorsement codes. Every step is idempotent
//! (it reads what is still to do inside its own transaction), so a run that repeats or overlaps
//! another one changes nothing twice. The discard at `purge_at` is `purge.rs`.
//!
//! Each step runs with a deadline and its own outcome: one that fails or runs out of time does not
//! stop the others, and every failure is logged with the step's name. The scans that have no
//! natural bound read the collection as a stream (memory does not grow with the collection) and
//! stop after `WORK_BUDGET` writes; what is left is found again by the next run, since each step
//! recomputes its pending work from the database.
//!
//! Layers, one file each: `model` (the rows the scans read), `domain` (the work budget and the
//! pure date and expiry rules), `persist` (the steps: their queries and transactions) and `runner`
//! (the loop that ticks every hour and runs the steps with a deadline). The task has no routes and
//! shows nothing, so there is no `http` or `view`.

mod domain;
mod model;
mod persist;
mod runner;

#[cfg(test)]
mod tests;

// `Jobs`, `StepFailure` and `run_once` are reached by name from the tests only.
#[allow(unused_imports)]
pub use runner::{Jobs, StepFailure, run_once, spawn};
