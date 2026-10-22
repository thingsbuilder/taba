//! The limits of a pass, the plan of what to delete and the rules that need no database.

use std::collections::BTreeMap;

use firestore::FirestoreInstant;
use firestore::jiff::tz::TimeZone;

use crate::events::{EventDoc, EventStatus};
use crate::volunteers::{self};
use crate::{cards, households, people};

/// Writes of one transaction (Firestore takes 500).
const BATCH_WRITES: usize = 400;
/// Deletes per transaction: the batch minus the daily counter and the audit entry.
pub(super) const DELETES_PER_TX: usize = BATCH_WRITES - 2;
/// A delivery write plus at most one new pseudonym, and the audit entry.
pub(super) const PSEUDONYMIZE_PER_TX: usize = (BATCH_WRITES - 1) / 2;
/// A household takes its cards with it, so fewer per transaction.
const HOUSEHOLDS_PER_TX: usize = 50;
/// The free tier allows 20 000 deletions a day (spec 6.3.5).
pub(super) const DAILY_DELETIONS: u32 = 15_000;
pub(super) const VOLUNTEER_IDLE_DAYS: i64 = 365;
pub(super) const DAY_MICROS: i64 = 86_400_000_000;
/// A pass that dies leaves the lease behind; the next one may take it after this.
pub(super) const LEASE_MICROS: i64 = 20 * 60 * 1_000_000;

// A transaction stays within `BATCH_WRITES` writes.
const _: () = assert!(DELETES_PER_TX + 2 <= BATCH_WRITES);
const _: () = assert!(PSEUDONYMIZE_PER_TX * 2 < BATCH_WRITES);
const _: () = assert!(HOUSEHOLDS_PER_TX * 3 + 2 <= BATCH_WRITES);

pub(super) fn micros_ahead(at: FirestoreInstant, micros: i64) -> FirestoreInstant {
    FirestoreInstant::from_microsecond(at.as_microsecond() + micros).expect("a date within range")
}

pub(super) fn day_of(at: FirestoreInstant, tz: &str) -> String {
    let zone = TimeZone::get(tz).unwrap_or(TimeZone::UTC);
    at.to_zoned(zone).date().to_string()
}

pub(super) type Counts = BTreeMap<String, u32>;

pub(super) fn bump(counts: &mut Counts, label: &str, n: u32) {
    *counts.entry(label.into()).or_default() += n;
}

/// The target id of the `purge.dry_run` entry for data no event owns.
pub(super) const NO_EVENT: &str = "no_event";

pub(super) fn due_for_purge(event: &EventDoc, now: FirestoreInstant) -> bool {
    event.status == EventStatus::Closed && event.purge_at.is_some_and(|at| at <= now)
}

/// One thing to delete. `Person`, `Household`, `Card` and `Volunteer` are re-checked inside the
/// transaction that deletes them.
#[derive(Clone)]
pub(super) enum Op {
    Plain(&'static str, String),
    Person(String),
    Household(String),
    Card(String),
    Volunteer(String),
}

impl Op {
    pub(super) fn label(&self) -> &'static str {
        match self {
            Op::Plain(collection, _) => collection,
            Op::Person(_) => people::COLLECTION,
            Op::Household(_) => households::COLLECTION,
            Op::Card(_) => cards::COLLECTION,
            Op::Volunteer(_) => volunteers::COLLECTION,
        }
    }

    pub(super) fn per_tx(&self) -> usize {
        match self {
            Op::Household(_) => HOUSEHOLDS_PER_TX,
            Op::Volunteer(_) => 1,
            _ => DELETES_PER_TX,
        }
    }
}

pub(super) struct Target<'a> {
    pub(super) action: &'a str,
    pub(super) kind: &'a str,
    pub(super) id: &'a str,
}
