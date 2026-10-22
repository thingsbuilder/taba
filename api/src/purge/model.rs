//! The documents the discard keeps (lease, daily counter, pseudonyms) and the rows it reads.

use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};

use crate::volunteers::GlobalRole;

pub(super) const STATE: &str = "purge_state";
pub(super) const LOCK_DOC: &str = "lock";
pub(super) const DAILY_DOC: &str = "daily";
pub(super) const PSEUDONYMS: &str = "purge_pseudonyms";

/// What one pass did (or, with `PURGE_ENABLED` off, would have done).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PurgeReport {
    /// `true` when `PURGE_ENABLED` is off: the counts below are what it would delete or change.
    pub dry_run: bool,
    pub deleted: u32,
    pub pseudonymized: u32,
}

#[derive(Serialize, Deserialize)]
pub(super) struct LockDoc {
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub(super) locked_until: FirestoreInstant,
}

/// Deletions made on `day` (a date in `TZ_PILOT`), the events whose dry run was already audited
/// that day and whether the data without an event was counted.
#[derive(Serialize, Deserialize, Default)]
pub(super) struct DailyDoc {
    pub(super) day: String,
    pub(super) deleted: u32,
    #[serde(default)]
    pub(super) dry_run_logged: Vec<String>,
    /// The data without an event was already counted today (it reads every old document).
    #[serde(default)]
    pub(super) without_event_counted: bool,
}

/// The event fields of a search or rescue (`event_id`) or a person (`event_ids`).
#[derive(Deserialize)]
pub(super) struct WithoutEvent {
    #[serde(default)]
    pub(super) event_id: Option<String>,
    #[serde(default)]
    pub(super) event_ids: Vec<String>,
}

impl WithoutEvent {
    pub(super) fn has_no_event(&self) -> bool {
        self.event_id.is_none() && self.event_ids.is_empty()
    }
}

#[derive(Deserialize)]
pub(super) struct Id {
    #[serde(alias = "_firestore_id")]
    pub(super) id: String,
}

#[derive(Deserialize)]
pub(super) struct Row<T> {
    #[serde(alias = "_firestore_id")]
    pub(super) id: String,
    #[serde(flatten)]
    pub(super) doc: T,
}

#[derive(Deserialize)]
pub(super) struct PersonRow {
    #[serde(default)]
    pub(super) event_ids: Vec<String>,
    #[serde(default)]
    pub(super) household_id: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct HouseholdRow {
    #[serde(default)]
    pub(super) member_ids: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct CardRow {
    pub(super) household_id: String,
}

#[derive(Deserialize)]
pub(super) struct VolunteerRow {
    #[serde(default)]
    pub(super) banned: bool,
    #[serde(default)]
    pub(super) global_roles: Vec<GlobalRole>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub(super) last_seen_at: FirestoreInstant,
}

#[derive(Serialize, Deserialize)]
pub(super) struct PseudonymDoc {
    pub(super) event_id: String,
    pub(super) pseudonym: String,
}
