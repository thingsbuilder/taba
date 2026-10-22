//! The rules on the status (spec 5.5, 5.7): plain functions over `StatusDoc`, nothing `async`.

use firestore::FirestoreInstant;

use super::model::StatusDoc;
use crate::status::{Reason, Unico};

impl StatusDoc {
    pub(super) fn degraded(
        reason: Reason,
        now: FirestoreInstant,
        until: Option<FirestoreInstant>,
    ) -> Self {
        Self {
            unico: Unico::Degraded,
            reason: Some(reason),
            since: Some(now),
            until,
        }
    }

    /// Why Unico is off limits at `now`, if it is. An `unavailable` period whose `until` has
    /// passed no longer counts: the mode ends by itself (spec 5.7).
    pub fn active_reason(&self, now: FirestoreInstant) -> Option<Reason> {
        match (self.unico, self.reason) {
            (Unico::Degraded, Some(Reason::Limit)) => Some(Reason::Limit),
            (Unico::Degraded, Some(Reason::Unavailable)) if self.until.is_some_and(|u| now < u) => {
                Some(Reason::Unavailable)
            }
            _ => None,
        }
    }
}
