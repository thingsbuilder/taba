//! The rules of a delivery, with no database and no HTTP: what a delivery needs (draw, liveness,
//! admin approval), what makes two deliveries a duplicate, what is told of the other one, the two
//! transitions of a delivery's life (recorded, reviewed) and the validation of a shelter's items
//! (spec 3.5.2). `persist.rs` reads and writes around them.

use std::collections::BTreeSet;

use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::model::{
    DeliveryDoc, DeliveryStatus, ItemSpec, Items, Reason, ReasonKind, Verification, invalid,
};
use crate::error::ApiError;
use crate::shelters::{Confirmation, ShelterDoc};

pub(super) const DEFAULT_PERIOD_DAYS: i64 = 7;
pub(crate) const DAY_MICROS: i64 = 86_400_000_000;
/// A delivery the app calls offline reaches the server later: one that arrives within this of its
/// own `delivered_at` was not queued, and the claim is ignored (the server treats it as online).
pub(super) const OFFLINE_MIN_DELAY_MICROS: i64 = 30_000_000;
/// `delivered_at` is the phone's clock: refused when older than this; a time ahead of the server's
/// is not refused (a phone with the clock ahead would never get a delivery through) but cut to the
/// moment of receipt.
pub(super) const MAX_AGE_MICROS: i64 = 30 * DAY_MICROS;

// ---- the decision ----

/// Whether `sha256(delivery_id)`, read as a number in [0, 1), is below `rate` (spec 3.5.3). The
/// same id always gives the same answer, so a retry is drawn the same way.
pub fn is_sampled(delivery_id: &str, rate: f64) -> bool {
    let digest = Sha256::digest(delivery_id.as_bytes());
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    // 2^64 as a float: the largest value maps to just below 1.
    (u64::from_be_bytes(head) as f64 / 18_446_744_073_709_551_616.0) < rate
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Decision {
    /// Delivered on the card alone.
    Card,
    /// The capture sent with the request must be the person.
    Revalidate,
    /// An admin of the shelter vouches for the person.
    AdminApproval,
    /// Liveness was due but cannot be done now: the delivery waits for review.
    Unavailable,
    /// `428`: ask for a selfie (or the admin's approval).
    Ask,
}

/// What a delivery needs (spec 3.5.3 items 3, 4 and 7). `unavailable`: offline or Unico degraded,
/// where there is no draw and a `high_value` item without an admin goes to review.
pub(super) fn decide(
    high_value: bool,
    sampled: bool,
    unavailable: bool,
    admin_approval: bool,
    has_capture: bool,
) -> Decision {
    if admin_approval {
        return Decision::AdminApproval;
    }
    if unavailable {
        return if high_value {
            Decision::Unavailable
        } else {
            Decision::Card
        };
    }
    if has_capture {
        return Decision::Revalidate;
    }
    if high_value || sampled {
        Decision::Ask
    } else {
        Decision::Card
    }
}

/// Whether two deliveries of the item are close enough to be a duplicate: less than `period_days`
/// apart (either order, since offline items can sync out of order), or any time for `once_per_event`.
pub(super) fn within_period(spec: &ItemSpec, a: FirestoreInstant, b: FirestoreInstant) -> bool {
    if spec.once_per_event {
        return true;
    }
    let days = spec.period_days.map_or(DEFAULT_PERIOD_DAYS, i64::from);
    (a.as_microsecond() - b.as_microsecond()).abs() < days * DAY_MICROS
}

/// Whether the app's `offline` claim is believed: only for a delivery that waited at least
/// `OFFLINE_MIN_DELAY_MICROS` between `delivered_at` and now.
pub(super) fn offline_claim_accepted(
    claimed: bool,
    delivered_at: FirestoreInstant,
    received_at: FirestoreInstant,
) -> bool {
    claimed
        && received_at.as_microsecond() - delivered_at.as_microsecond() >= OFFLINE_MIN_DELAY_MICROS
}

/// The phone's `delivered_at` against the server's clock: a time in the future is cut to
/// `received_at` (and says so); one older than 30 days is not plausible (`None`).
pub(super) fn plausible_delivered_at(
    delivered_at: FirestoreInstant,
    received_at: FirestoreInstant,
) -> Option<(FirestoreInstant, bool)> {
    if delivered_at > received_at {
        return Some((received_at, true));
    }
    (received_at.as_microsecond() - delivered_at.as_microsecond() <= MAX_AGE_MICROS)
        .then_some((delivered_at, false))
}

/// What the requesting shelter may be told about the delivery that makes its own a duplicate:
/// its own shelter's, always; another shelter's only to a confirmed requester (the date), and the
/// name only when that other shelter is confirmed too (spec 3.5.3 item 6, 4.7).
pub fn previous_visibility(
    requester: &ShelterDoc,
    requester_id: &str,
    previous_shelter_id: &str,
    other: Option<&ShelterDoc>,
) -> (bool, bool) {
    let own = previous_shelter_id == requester_id;
    let confirmed = requester.confirmation == Confirmation::Confirmed;
    let date = own || confirmed;
    let name =
        own || (confirmed && other.is_some_and(|o| o.confirmation == Confirmation::Confirmed));
    (date, name)
}

// ---- recording ----

/// Everything the transaction that records a delivery needs.
#[derive(Clone)]
pub(super) struct Write {
    pub id: String,
    pub shelter_id: String,
    pub shelter: ShelterDoc,
    pub volunteer_id: String,
    pub card_code: String,
    pub person_id: String,
    pub household_id: String,
    pub item: String,
    pub spec: ItemSpec,
    pub quantity: u32,
    pub delivered_at: FirestoreInstant,
    pub decision: Decision,
    pub offline_claimed: bool,
    pub delivered_at_clamped: bool,
    pub received_at: FirestoreInstant,
    /// The revalidation capture, already resolved to the person (`Decision::Revalidate`).
    pub capture_id: Option<String>,
}

impl Write {
    /// Why the delivery waits for review: a duplicate is flagged, never refused (spec 3.5.3 item 6).
    pub fn reasons(&self, duplicate_of: Option<String>) -> Vec<Reason> {
        let mut reasons = Vec::new();
        if let Some(previous) = duplicate_of {
            reasons.push(Reason {
                kind: ReasonKind::DuplicateInPeriod,
                previous_delivery_id: Some(previous),
            });
        }
        if self.decision == Decision::Unavailable {
            reasons.push(Reason {
                kind: ReasonKind::LivenessUnavailable,
                previous_delivery_id: None,
            });
        }
        reasons
    }

    /// The document that records this delivery; any reason puts it in review.
    pub fn delivery(&self, reasons: Vec<Reason>) -> DeliveryDoc {
        DeliveryDoc {
            shelter_id: self.shelter_id.clone(),
            shelter_name: self.shelter.name.clone(),
            event_id: self.shelter.event_id.clone(),
            person_id: Some(self.person_id.clone()),
            household_id: Some(self.household_id.clone()),
            household_pseudonym: None,
            card_code: Some(self.card_code.clone()),
            item: self.item.clone(),
            scope: self.spec.scope,
            quantity: self.quantity,
            delivered_by: self.volunteer_id.clone(),
            delivered_at: self.delivered_at,
            received_at: self.received_at,
            verification: match self.decision {
                Decision::Revalidate => Verification::Liveness,
                Decision::AdminApproval => Verification::AdminApproval,
                _ => Verification::Card,
            },
            approved_by: (self.decision == Decision::AdminApproval)
                .then(|| self.volunteer_id.clone()),
            status: if reasons.is_empty() {
                DeliveryStatus::Ok
            } else {
                DeliveryStatus::InReview
            },
            reasons,
            offline_claimed: self.offline_claimed,
            delivered_at_clamped: self.delivered_at_clamped,
            reviewed_by: None,
            review_note: None,
            pseudonymized: false,
        }
    }

    /// What the audit says besides the delivery, as the three parts of `delivery.{}{}{}`: how it
    /// was vouched for, the offline claim the server believed and a clock cut.
    pub fn audit_parts(&self) -> [&'static str; 3] {
        [
            if self.decision == Decision::AdminApproval {
                "admin_approval"
            } else {
                "create"
            },
            if self.offline_claimed { "_offline" } else { "" },
            if self.delivered_at_clamped {
                "_clamped"
            } else {
                ""
            },
        ]
    }
}

/// Whether the delivery `previous` (a row of the duplicate query) makes `w` a duplicate: not `w`
/// itself, and close enough in time.
pub(super) fn counts_against(
    w: &Write,
    previous_id: &str,
    previous_delivered_at: FirestoreInstant,
) -> bool {
    previous_id != w.id && within_period(&w.spec, previous_delivered_at, w.delivered_at)
}

/// A delivery that already exists answers the same request again (same shelter, same volunteer);
/// the same id from anyone else is a conflict.
pub(super) fn same_delivery(
    existing: DeliveryDoc,
    shelter_id: &str,
    volunteer_id: &str,
) -> Result<DeliveryDoc, ApiError> {
    if existing.shelter_id == shelter_id && existing.delivered_by == volunteer_id {
        Ok(existing)
    } else {
        Err(ApiError::new(StatusCode::CONFLICT, "delivery_id_taken"))
    }
}

// ---- review ----

/// What an admin decides about a delivery that waits for review.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    ReviewedOk,
    Irregular,
}

pub fn not_in_review() -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "delivery_not_in_review")
}

impl DeliveryDoc {
    /// The only way a delivery leaves `in_review`. A delivery that was already decided is a
    /// conflict, so a repeated review never replaces the first reviewer or note.
    pub fn review(
        &mut self,
        decision: ReviewDecision,
        reviewer_id: &str,
        note: Option<String>,
    ) -> Result<(), ApiError> {
        if self.status != DeliveryStatus::InReview {
            return Err(not_in_review());
        }
        self.status = match decision {
            ReviewDecision::ReviewedOk => DeliveryStatus::ReviewedOk,
            ReviewDecision::Irregular => DeliveryStatus::Irregular,
        };
        self.reviewed_by = Some(reviewer_id.to_string());
        self.review_note = note;
        Ok(())
    }
}

pub(super) const MAX_ITEMS: usize = 50;
pub(super) const MAX_ITEM_NAME_CHARS: usize = 60;
pub(super) const MAX_PERIOD_DAYS: u32 = 365;

/// Trims the names, refuses the ones that are empty, long, with control characters or that repeat
/// (ignoring case), and drops `period_days` where `once_per_event` makes it meaningless.
pub(super) fn valid_items(items: Items) -> Result<Items, ApiError> {
    if items.len() > MAX_ITEMS {
        return Err(invalid("items"));
    }
    let mut seen = BTreeSet::new();
    let mut valid = Items::new();
    for (name, mut spec) in items {
        let name = name.trim().to_string();
        if name.is_empty()
            || name.chars().count() > MAX_ITEM_NAME_CHARS
            || name.chars().any(char::is_control)
            || !seen.insert(name.to_lowercase())
        {
            return Err(invalid("item"));
        }
        if spec.once_per_event {
            spec.period_days = None;
        } else if spec
            .period_days
            .is_some_and(|days| days == 0 || days > MAX_PERIOD_DAYS)
        {
            return Err(invalid("period_days"));
        }
        valid.insert(name, spec);
    }
    Ok(valid)
}

pub(super) fn status_name(status: DeliveryStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}
