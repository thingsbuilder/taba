//! The differentiated discard (spec 10.1.2, 6.2.6 item 3), called by the hourly task in `jobs.rs`.
//!
//! Layers of safety, outermost first:
//! 1. `PURGE_ENABLED` off (the default): dry run. The pass computes what it would delete and
//!    only logs the counts and writes `purge.dry_run` (once a day per event).
//! 2. A lease document keeps two passes from running at once.
//! 3. Only `closed` events with `purge_at <= now` are discarded; a person is deleted only when
//!    every event of theirs is due (re-checked inside the deleting transaction).
//! 4. Deletes go in transactions of at most `BATCH_WRITES` writes, each carrying the daily
//!    counter and a `purge.event` audit entry, and the pass stops at `DAILY_DELETIONS` deletions
//!    in the day (in `TZ_PILOT`); the next hour resumes.
//!
//! Every step recomputes what is left from the database, so a pass that stopped halfway
//! (crash, daily cap) is finished by the next one. Deliveries are pseudonymized first, with the
//! pseudonym of each household kept in `purge_pseudonyms` until the event is done, so a resumed
//! pass gives the same household the same pseudonym. `audit_log` is never touched here.
//!
//! Layers, one file each: `model` (the documents the pass keeps and reads), `domain` (the limits,
//! the plan of what to delete and the pure rules) and `persist` (the pass itself, which reads,
//! deletes and audits). The discard has no routes and shows nothing, so there is no `http` or
//! `view`.

mod domain;
mod model;
mod persist;

#[cfg(test)]
mod tests;

pub use persist::run;
