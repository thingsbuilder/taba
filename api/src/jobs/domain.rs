//! The work budget of a step and the date and expiry rules. Nothing here is `async` or
//! reads the database.

use firestore::FirestoreInstant;

/// Writes of one transaction (Firestore takes 500).
pub(super) const BATCH_WRITES: usize = 400;
/// Documents a step changes in one run before it stops and leaves the rest to the next run.
pub(super) const WORK_BUDGET: usize = 5_000;
/// Events listed at once. Events are disasters, not volumes: there are a handful of open ones.
/// A listing that fills this is logged, and the events past it wait until some close.
pub(super) const MAX_EVENTS: usize = 1_000;
pub(super) const DAY_MICROS: i64 = 86_400_000_000;

/// How much a step may still change in this run.
pub(super) struct Budget {
    pub(super) step: &'static str,
    pub(super) left: usize,
}

impl Budget {
    pub(super) fn new(step: &'static str) -> Self {
        Self {
            step,
            left: WORK_BUDGET,
        }
    }

    pub(super) fn spend(&mut self, n: usize) {
        self.left = self.left.saturating_sub(n);
    }

    /// `true` once the budget is gone; logs it, as the backlog the next run takes up.
    pub(super) fn spent(&self) -> bool {
        if self.left == 0 {
            tracing::warn!(
                step = self.step,
                budget = WORK_BUDGET,
                "work budget reached; the rest waits for the next run"
            );
        }
        self.left == 0
    }
}

pub(super) fn plus_days(at: FirestoreInstant, days: u32) -> FirestoreInstant {
    FirestoreInstant::from_microsecond(at.as_microsecond() + i64::from(days) * DAY_MICROS)
        .expect("a date within range")
}

pub(super) fn minus_days(at: FirestoreInstant, days: u32) -> FirestoreInstant {
    FirestoreInstant::from_microsecond(at.as_microsecond() - i64::from(days) * DAY_MICROS)
        .expect("a date within range")
}

/// A person with no event and no shelter (a rescue the landing point never sent anywhere) has no
/// event whose end would delete the thumbnail: it goes `PURGE_AFTER_DAYS` after the last change
/// to the person (spec 4.6), which is as long as an event keeps it after it closes.
pub(super) fn orphan_expired(
    event_ids: &[String],
    current_shelter_id: Option<&str>,
    updated_at: FirestoreInstant,
    cutoff: FirestoreInstant,
) -> bool {
    event_ids.is_empty() && current_shelter_id.is_none() && updated_at < cutoff
}
