//! The rules of a delivery, with no database and no HTTP: the types stored in `deliveries`, what a
//! delivery needs (draw, liveness, admin approval), what makes two deliveries a duplicate, what is
//! told of the other one, and the two transitions of a delivery's life (recorded, reviewed).
//! `mod.rs` reads and writes around them; `deliveries_admin.rs` calls the review.

use std::collections::BTreeMap;

use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::ApiError;
use crate::shelters::{Confirmation, ShelterDoc};

pub const COLLECTION: &str = "deliveries";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemScope {
    PerPerson,
    PerHousehold,
}

/// One item a shelter gives (spec 3.5.2); `shelters.items` maps the item name to this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemSpec {
    pub scope: ItemScope,
    /// Days between two deliveries of the item; absent means the default of 7. Ignored when `once_per_event`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_days: Option<u32>,
    #[serde(default)]
    pub once_per_event: bool,
    #[serde(default)]
    pub high_value: bool,
}

/// `shelters.items` (spec 7): item name to its rule.
pub type Items = BTreeMap<String, ItemSpec>;

/// How the person who took the item was checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    Card,
    Liveness,
    AdminApproval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Ok,
    InReview,
    ReviewedOk,
    Irregular,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonKind {
    DuplicateInPeriod,
    LivenessUnavailable,
}

/// Why a delivery waits for review (spec 4.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reason {
    pub kind: ReasonKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_delivery_id: Option<String>,
}

/// `deliveries/{delivery_id}` (UUID of the client), spec 7. `person_id` is a token id or, for a
/// `no_biometrics` person, a client UUID; `late biometrics` repoints it (`people::repoint_person_refs`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryDoc {
    pub shelter_id: String,
    pub shelter_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub household_id: Option<String>,
    /// Phase 6 fills it when the delivery is pseudonymized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub household_pseudonym: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_code: Option<String>,
    pub item: String,
    pub scope: ItemScope,
    pub quantity: u32,
    /// Empty once the delivery is pseudonymized.
    #[serde(default)]
    pub delivered_by: String,
    /// The phone's clock.
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub delivered_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub received_at: FirestoreInstant,
    pub verification: Verification,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    pub status: DeliveryStatus,
    #[serde(default)]
    pub reasons: Vec<Reason>,
    /// The app said the delivery was made offline and the server believed it (`offline_claim`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub offline_claimed: bool,
    /// The phone's `delivered_at` was ahead of the server's clock and was cut to `received_at`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub delivered_at_clamped: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_note: Option<String>,
    #[serde(default)]
    pub pseudonymized: bool,
}

const DEFAULT_PERIOD_DAYS: i64 = 7;
pub(crate) const DAY_MICROS: i64 = 86_400_000_000;
/// A delivery the app calls offline reaches the server later: one that arrives within this of its
/// own `delivered_at` was not queued, and the claim is ignored (the server treats it as online).
const OFFLINE_MIN_DELAY_MICROS: i64 = 30_000_000;
/// `delivered_at` is the phone's clock: refused when older than this; a time ahead of the server's
/// is not refused (a phone with the clock ahead would never get a delivery through) but cut to the
/// moment of receipt.
const MAX_AGE_MICROS: i64 = 30 * DAY_MICROS;

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

    /// What the audit says besides the delivery: the offline claim the server believed and a clock cut.
    pub fn audit_action(&self) -> String {
        format!(
            "delivery.{}{}{}",
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
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shelters::{Address, ShelterStatus};
    use crate::store;
    use crate::test_support::new_id;

    const DAYS: i64 = DAY_MICROS;

    fn spec(scope: ItemScope, period: Option<u32>, once: bool, high: bool) -> ItemSpec {
        ItemSpec {
            scope,
            period_days: period,
            once_per_event: once,
            high_value: high,
        }
    }

    fn shelter(confirmation: Confirmation) -> ShelterDoc {
        let now = store::now();
        ShelterDoc {
            id: None,
            name: "Abrigo".into(),
            lat: 0.0,
            lon: 0.0,
            accuracy_m: 5.0,
            geohash: String::new(),
            address: Address::default(),
            event_id: Some("e1".into()),
            capacity: None,
            created_by: "v".into(),
            client_created_at: now,
            created_at: now,
            status: ShelterStatus::Open,
            confirmation,
            confirmation_type: None,
            can_endorse: false,
            endorsements: vec![],
            revocations: vec![],
            flagged: false,
            other_site_reason: None,
            conflicts_with: None,
            items: Items::new(),
        }
    }

    fn write(decision: Decision) -> Write {
        let now = store::now();
        Write {
            id: "d2".into(),
            shelter_id: "s1".into(),
            shelter: shelter(Confirmation::Unconfirmed),
            volunteer_id: "vol".into(),
            card_code: "ABCD2345".into(),
            person_id: "p1".into(),
            household_id: "h1".into(),
            item: "cesta".into(),
            spec: spec(ItemScope::PerHousehold, None, false, false),
            quantity: 1,
            delivered_at: now,
            decision,
            offline_claimed: false,
            delivered_at_clamped: false,
            received_at: now,
            capture_id: None,
        }
    }

    /// A fixed instant plus `micros`, so differences between two of them are exact.
    fn at(micros: i64) -> FirestoreInstant {
        FirestoreInstant::from_microsecond(1_800_000_000_000_000 + micros).unwrap()
    }

    // ---- the draw and the decision ----

    #[test]
    fn the_draw_is_deterministic_and_close_to_the_rate() {
        let id = new_id();
        assert_eq!(is_sampled(&id, 0.05), is_sampled(&id, 0.05));
        let ids: Vec<String> = (0..20_000).map(|_| new_id()).collect();
        let drawn = ids.iter().filter(|id| is_sampled(id, 0.05)).count();
        assert!((700..=1300).contains(&drawn), "{drawn} of 20000");
        assert!(ids.iter().all(|id| !is_sampled(id, 0.0)));
        assert!(ids.iter().all(|id| is_sampled(id, 1.0)));
        // growing the rate never undraws an id
        assert!(
            ids.iter()
                .all(|id| !is_sampled(id, 0.05) || is_sampled(id, 0.5))
        );
    }

    #[test]
    fn the_decision_table() {
        use Decision::*;
        // (high_value, sampled, unavailable, admin_approval, has_capture)
        let cases = [
            ((false, false, false, false, false), Card),
            ((false, true, false, false, false), Ask),
            ((true, false, false, false, false), Ask),
            ((true, false, false, false, true), Revalidate),
            ((false, false, false, false, true), Revalidate),
            ((true, true, false, true, true), AdminApproval),
            ((false, false, false, true, false), AdminApproval),
            ((true, false, true, false, false), Unavailable),
            ((true, false, true, true, false), AdminApproval),
            ((false, true, true, false, false), Card),
            ((false, false, true, false, true), Card),
        ];
        for ((high, sampled, unavailable, admin, capture), expected) in cases {
            assert_eq!(
                decide(high, sampled, unavailable, admin, capture),
                expected,
                "{high} {sampled} {unavailable} {admin} {capture}"
            );
        }
    }

    #[test]
    fn the_period_is_strictly_shorter_than_period_days_and_ignored_for_once_per_event() {
        let base = store::now();
        let week = spec(ItemScope::PerPerson, None, false, false);
        let at_ = |micros: i64| {
            FirestoreInstant::from_microsecond(base.as_microsecond() + micros).unwrap()
        };
        assert!(within_period(&week, base, at_(7 * DAYS - 1)));
        assert!(!within_period(&week, base, at_(7 * DAYS)));
        assert!(
            within_period(&week, at_(7 * DAYS - 1), base),
            "either order"
        );
        let three = spec(ItemScope::PerPerson, Some(3), false, false);
        assert!(!within_period(&three, base, at_(3 * DAYS)));
        let once = spec(ItemScope::PerPerson, Some(3), true, false);
        assert!(within_period(&once, base, at_(400 * DAYS)));
    }

    #[test]
    fn the_offline_claim_needs_a_real_wait_and_a_future_clock_is_cut_at_receipt() {
        let received = at(0);
        assert!(offline_claim_accepted(
            true,
            at(-OFFLINE_MIN_DELAY_MICROS),
            received
        ));
        assert!(!offline_claim_accepted(
            true,
            at(-OFFLINE_MIN_DELAY_MICROS + 1),
            received
        ));
        assert!(!offline_claim_accepted(false, at(-60_000_000), received));

        assert_eq!(
            plausible_delivered_at(at(1), received),
            Some((received, true))
        );
        assert_eq!(
            plausible_delivered_at(at(-MAX_AGE_MICROS), received),
            Some((at(-MAX_AGE_MICROS), false))
        );
        assert_eq!(
            plausible_delivered_at(at(-MAX_AGE_MICROS - 1), received),
            None
        );
    }

    #[test]
    fn what_is_told_of_the_other_delivery_follows_who_asks() {
        let unconfirmed = shelter(Confirmation::Unconfirmed);
        let confirmed = shelter(Confirmation::Confirmed);
        // -> (date, name)
        assert_eq!(
            previous_visibility(&unconfirmed, "a", "a", None),
            (true, true)
        );
        assert_eq!(
            previous_visibility(&unconfirmed, "a", "b", Some(&confirmed)),
            (false, false)
        );
        assert_eq!(
            previous_visibility(&confirmed, "a", "b", Some(&unconfirmed)),
            (true, false)
        );
        assert_eq!(
            previous_visibility(&confirmed, "a", "b", Some(&confirmed)),
            (true, true)
        );
        assert_eq!(
            previous_visibility(&confirmed, "a", "b", None),
            (true, false)
        );
    }

    // ---- invariants (matrix of Q03: what each operation may do to them) ----

    /// A repeated take is flagged, never refused: whatever the decision, a duplicate is a reason
    /// and the delivery is recorded in review.
    #[test]
    fn a_repeated_take_is_flagged_never_refused() {
        for decision in [
            Decision::Card,
            Decision::Revalidate,
            Decision::AdminApproval,
            Decision::Unavailable,
        ] {
            let w = write(decision);
            let doc = w.delivery(w.reasons(Some("d1".into())));
            assert_eq!(doc.status, DeliveryStatus::InReview, "{decision:?}");
            assert!(
                doc.reasons
                    .iter()
                    .any(|r| r.kind == ReasonKind::DuplicateInPeriod
                        && r.previous_delivery_id.as_deref() == Some("d1")),
                "{decision:?}"
            );
        }
        let w = write(Decision::Card);
        assert_eq!(w.delivery(w.reasons(None)).status, DeliveryStatus::Ok);
    }

    #[test]
    fn a_duplicate_and_a_liveness_that_could_not_be_done_carry_both_reasons() {
        let w = write(Decision::Unavailable);
        let kinds: Vec<_> = w
            .reasons(Some("d1".into()))
            .iter()
            .map(|r| r.kind)
            .collect();
        assert_eq!(
            kinds,
            [
                ReasonKind::DuplicateInPeriod,
                ReasonKind::LivenessUnavailable
            ]
        );
    }

    /// A repeated item does not repeat its effect: a delivery is never its own duplicate, so the
    /// same request sent again is not flagged against itself.
    #[test]
    fn a_delivery_is_never_its_own_duplicate() {
        let mut w = write(Decision::Card);
        w.delivered_at = at(0);
        assert!(!counts_against(&w, &w.id, at(0)));
        assert!(counts_against(&w, "d1", at(0)));
        assert!(counts_against(&w, "d1", at(-7 * DAYS + 1)));
        assert!(!counts_against(&w, "d1", at(-7 * DAYS)));
    }

    /// A replay does not widen access: the stored delivery answers the same shelter and volunteer
    /// again; anyone else using the id gets a conflict, never the stored data.
    #[test]
    fn a_replay_answers_only_the_volunteer_and_shelter_that_made_it() {
        let doc = write(Decision::Card).delivery(vec![]);
        assert!(same_delivery(doc.clone(), "s1", "vol").is_ok());
        for (shelter_id, volunteer_id) in [("s1", "other"), ("s2", "vol"), ("s2", "other")] {
            let error = same_delivery(doc.clone(), shelter_id, volunteer_id).unwrap_err();
            assert_eq!(error.code, "delivery_id_taken");
        }
    }

    /// A repeated review does not repeat its effect: the second neither changes the decision nor
    /// replaces the reviewer and the note.
    #[test]
    fn a_review_decides_once() {
        let w = write(Decision::Card);
        let mut doc = w.delivery(w.reasons(Some("d1".into())));
        doc.review(ReviewDecision::Irregular, "admin1", Some("first".into()))
            .unwrap();
        assert_eq!(doc.status, DeliveryStatus::Irregular);
        for decision in [ReviewDecision::ReviewedOk, ReviewDecision::Irregular] {
            let error = doc
                .review(decision, "admin2", Some("second".into()))
                .unwrap_err();
            assert_eq!(error.code, "delivery_not_in_review");
        }
        assert_eq!(doc.status, DeliveryStatus::Irregular);
        assert_eq!(doc.reviewed_by.as_deref(), Some("admin1"));
        assert_eq!(doc.review_note.as_deref(), Some("first"));
    }

    #[test]
    fn only_a_delivery_in_review_can_be_reviewed() {
        let mut ok = write(Decision::Card).delivery(vec![]);
        assert!(
            ok.review(ReviewDecision::ReviewedOk, "admin", None)
                .is_err()
        );
        assert_eq!(ok.status, DeliveryStatus::Ok);
        assert!(ok.reviewed_by.is_none());
    }

    #[test]
    fn the_audit_action_says_how_the_delivery_was_vouched_for() {
        let mut w = write(Decision::Card);
        assert_eq!(w.audit_action(), "delivery.create");
        w.offline_claimed = true;
        w.delivered_at_clamped = true;
        assert_eq!(w.audit_action(), "delivery.create_offline_clamped");
        let admin = write(Decision::AdminApproval);
        assert_eq!(admin.audit_action(), "delivery.admin_approval");
        assert_eq!(admin.delivery(vec![]).approved_by.as_deref(), Some("vol"));
    }
}
