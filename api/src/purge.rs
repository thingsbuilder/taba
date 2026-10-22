//! The differentiated discard (spec 10.1.2, 6.2.6 item 3), called by the hourly task in `jobs.rs`.
//! Stage 0: it does nothing and reports nothing.
#![allow(dead_code)] // stage 0 scaffold: the owner of this module removes this line

use crate::auth::AppState;
use crate::error::ApiError;

/// What one pass did (or, with `PURGE_ENABLED` off, would have done).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PurgeReport {
    /// `true` when `PURGE_ENABLED` is off: the counts below are what it would delete or change.
    pub dry_run: bool,
    pub deleted: u32,
    pub pseudonymized: u32,
}

/// One pass of the discard: events past `purge_at` (people, households, cards, rescues, searches,
/// matches, alerts, memberships and shelters of the event are deleted, deliveries are pseudonymized,
/// the event keeps its name, dates and `stats`), pseudonymized deliveries past `delivery_purge_at`,
/// and volunteer profiles 365 days after `last_seen_at` (banned ones stay). Batches of 400 writes,
/// stopping past 15 000 deletions in the day. With `config.purge_enabled` off it only counts.
/// Owner C. Called by `jobs::run_once`.
pub async fn run(_state: &AppState) -> Result<PurgeReport, ApiError> {
    Ok(PurgeReport::default())
}
