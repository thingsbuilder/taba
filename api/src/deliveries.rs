//! Deliveries (spec 3.5.3, 4.7, 7, 8.6): `POST /v1/shelters/{id}/deliveries`. The decision (draw,
//! liveness, admin approval, duplicate) and the write happen in one transaction; the types are
//! shared with `deliveries_admin.rs`.

use std::collections::{BTreeMap, HashMap};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use firestore::{FirestoreDb, FirestoreInstant, path};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{self, AppState, CaptureDoc, Purpose, Volunteer, resolve_person, usable_capture};
use crate::cards::{self, CardDoc, CardStatus};
use crate::degraded;
use crate::error::ApiError;
use crate::events::{self, EventDoc, EventStatus};
use crate::households::{forbidden, load_household};
use crate::people::{self, Level, PersonDoc};
use crate::shelters::{self, Confirmation, ShelterDoc};
use crate::store::{self, fs_error, now, tx_create, tx_replace};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_note: Option<String>,
    #[serde(default)]
    pub pseudonymized: bool,
}

const CAPTURES: &str = "captures";
const MAX_QUANTITY: u32 = 1000;
const DEFAULT_PERIOD_DAYS: i64 = 7;
const DAY_MICROS: i64 = 86_400_000_000;
/// A delivery the app calls offline reaches the server later: one that arrives within this of its
/// own `delivered_at` was not queued, and the claim is ignored (the server treats it as online).
const OFFLINE_MIN_DELAY_MICROS: i64 = 30_000_000;
/// `delivered_at` is the phone's clock: accepted from this long before the server's to a little ahead.
const MAX_AGE_MICROS: i64 = 30 * DAY_MICROS;
const MAX_AHEAD_MICROS: i64 = 5 * 60_000_000;
/// Most recent deliveries of the same item and owner that the duplicate check looks at.
const MAX_PREVIOUS: u32 = 50;

fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
    error.detail = json!({ "field": field });
    error
}

/// `POST /v1/shelters/{id}/deliveries` (spec 8.6). `offline` is set by the app for an item that
/// was made without a connection: the person is gone by the time it syncs, so no liveness.
#[derive(Debug, Clone, Deserialize)]
pub struct DeliveryRequest {
    id: String,
    card_code: String,
    person_id: String,
    item: String,
    quantity: u32,
    delivered_at: String,
    #[serde(default)]
    capture_id: Option<String>,
    #[serde(default)]
    admin_approval: bool,
    #[serde(default)]
    offline: bool,
}

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
enum Decision {
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
fn decide(
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
fn within_period(spec: &ItemSpec, a: FirestoreInstant, b: FirestoreInstant) -> bool {
    if spec.once_per_event {
        return true;
    }
    let days = spec.period_days.map_or(DEFAULT_PERIOD_DAYS, i64::from);
    (a.as_microsecond() - b.as_microsecond()).abs() < days * DAY_MICROS
}

/// Whether the app's `offline` claim is believed: only for a delivery that waited at least
/// `OFFLINE_MIN_DELAY_MICROS` between `delivered_at` and now.
fn offline_claim_accepted(
    claimed: bool,
    delivered_at: FirestoreInstant,
    received_at: FirestoreInstant,
) -> bool {
    claimed
        && received_at.as_microsecond() - delivered_at.as_microsecond() >= OFFLINE_MIN_DELAY_MICROS
}

/// Whether the phone's `delivered_at` is plausible against the server's clock.
fn delivered_at_in_range(delivered_at: FirestoreInstant, received_at: FirestoreInstant) -> bool {
    let age = received_at.as_microsecond() - delivered_at.as_microsecond();
    (-MAX_AHEAD_MICROS..=MAX_AGE_MICROS).contains(&age)
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

/// What someone typed or scanned, as Crockford reads it (same rule as `cards.rs`).
fn normalize_card(input: &str) -> String {
    input
        .trim()
        .chars()
        .map(|c| match c.to_ascii_uppercase() {
            'I' | 'L' => '1',
            'O' => '0',
            other => other,
        })
        .collect()
}

// ---- reads ----

/// The household of an active card and the person who takes the item, who must be a member of it.
/// A delivery made offline can reach the server after the late biometrics gave the person a new
/// id (spec 4.12): an id that is not a member but is a `former_id` of one stands for that member.
/// Returns the id the delivery is written under.
async fn load_target(
    db: &FirestoreDb,
    card_code: &str,
    person_id: &str,
) -> Result<(String, PersonDoc, String), ApiError> {
    let card: CardDoc = store::get(db, cards::COLLECTION, card_code)
        .await?
        .filter(|c: &CardDoc| c.status == CardStatus::Active)
        .ok_or_else(ApiError::card_not_found)?;
    let household = load_household(db, &card.household_id).await?;
    let mut member = household
        .member_ids
        .iter()
        .find(|m| *m == person_id)
        .cloned();
    if member.is_none() {
        for id in &household.member_ids {
            let former = store::get::<PersonDoc>(db, people::COLLECTION, id)
                .await?
                .is_some_and(|p| p.former_ids.iter().any(|f| f == person_id));
            if former {
                member = Some(id.clone());
                break;
            }
        }
    }
    let member = member.ok_or_else(ApiError::person_not_in_household)?;
    let person = store::get(db, people::COLLECTION, &member)
        .await?
        .ok_or_else(ApiError::person_not_in_household)?;
    Ok((card.household_id, person, member))
}

#[derive(Deserialize)]
struct Previous {
    #[serde(alias = "_firestore_id", default)]
    id: Option<String>,
    #[serde(default)]
    event_id: Option<String>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    delivered_at: FirestoreInstant,
}

/// The most recent delivery that makes `w` a duplicate: same item to the same person
/// (`per_person`) or household (`per_household`), within the period, in an active event.
async fn find_duplicate(db: &FirestoreDb, w: &Write) -> Result<Option<String>, ApiError> {
    let (field, owner) = match w.spec.scope {
        ItemScope::PerPerson => (path!(DeliveryDoc::person_id), &w.person_id),
        ItemScope::PerHousehold => (path!(DeliveryDoc::household_id), &w.household_id),
    };
    let rows: Vec<Previous> = db
        .fluent()
        .select()
        .from(COLLECTION)
        .filter(|q| {
            q.for_all([
                q.field(field.as_str()).eq(owner.as_str()),
                q.field(path!(DeliveryDoc::item)).eq(w.item.as_str()),
            ])
        })
        .order_by([(
            path!(DeliveryDoc::delivered_at),
            firestore::FirestoreQueryDirection::Descending,
        )])
        .limit(MAX_PREVIOUS)
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    let mut active: HashMap<String, bool> = HashMap::new();
    for row in rows {
        let Some(id) = row.id else { continue };
        if id == w.id || !within_period(&w.spec, row.delivered_at, w.delivered_at) {
            continue;
        }
        let counts = match &row.event_id {
            None => true,
            Some(event_id) => match active.get(event_id) {
                Some(known) => *known,
                None => {
                    let event: Option<EventDoc> =
                        store::get(db, events::COLLECTION, event_id).await?;
                    let is_active = event.is_some_and(|e| e.status == EventStatus::Active);
                    active.insert(event_id.clone(), is_active);
                    is_active
                }
            },
        };
        if counts {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

// ---- the write ----

/// Everything the transaction that records the delivery needs.
#[derive(Clone)]
struct Write {
    id: String,
    shelter_id: String,
    shelter: ShelterDoc,
    volunteer: Volunteer,
    card_code: String,
    person_id: String,
    household_id: String,
    item: String,
    spec: ItemSpec,
    quantity: u32,
    delivered_at: FirestoreInstant,
    decision: Decision,
    offline_claimed: bool,
    /// The revalidation capture, already resolved to the person (`Decision::Revalidate`).
    capture_id: Option<String>,
}

/// One transaction: re-reads the card, the household and the capture, looks for the duplicate and
/// writes the delivery. Of two deliveries of the same household the second one sees the first.
async fn record(
    db: &FirestoreDb,
    tx: &mut firestore::FirestoreTransaction<'_>,
    w: &Write,
) -> Result<DeliveryDoc, ApiError> {
    events::ensure_shelter_open(db, &w.shelter_id).await?;
    if let Some(existing) = store::get::<DeliveryDoc>(db, COLLECTION, &w.id).await? {
        return same_delivery(existing, &w.shelter_id, &w.volunteer.id);
    }
    let (household_id, _, _) = load_target(db, &w.card_code, &w.person_id).await?;
    if household_id != w.household_id {
        return Err(ApiError::card_not_found());
    }
    if let Some(capture_id) = &w.capture_id {
        let capture: CaptureDoc = store::get(db, CAPTURES, capture_id)
            .await?
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "capture_not_found"))?;
        if capture.consumed {
            return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
        }
        tx_replace(
            db,
            tx,
            CAPTURES,
            capture_id,
            &auth::consumed(capture, &w.person_id),
        )?;
    }

    let mut reasons = Vec::new();
    if let Some(previous) = find_duplicate(db, w).await? {
        reasons.push(Reason {
            kind: ReasonKind::DuplicateInPeriod,
            previous_delivery_id: Some(previous),
        });
    }
    if w.decision == Decision::Unavailable {
        reasons.push(Reason {
            kind: ReasonKind::LivenessUnavailable,
            previous_delivery_id: None,
        });
    }
    let approved = w.decision == Decision::AdminApproval;
    let doc = DeliveryDoc {
        shelter_id: w.shelter_id.clone(),
        shelter_name: w.shelter.name.clone(),
        event_id: w.shelter.event_id.clone(),
        person_id: Some(w.person_id.clone()),
        household_id: Some(w.household_id.clone()),
        household_pseudonym: None,
        card_code: Some(w.card_code.clone()),
        item: w.item.clone(),
        scope: w.spec.scope,
        quantity: w.quantity,
        delivered_by: w.volunteer.id.clone(),
        delivered_at: w.delivered_at,
        received_at: now(),
        verification: match w.decision {
            Decision::Revalidate => Verification::Liveness,
            Decision::AdminApproval => Verification::AdminApproval,
            _ => Verification::Card,
        },
        approved_by: approved.then(|| w.volunteer.id.clone()),
        status: if reasons.is_empty() {
            DeliveryStatus::Ok
        } else {
            DeliveryStatus::InReview
        },
        reasons,
        offline_claimed: w.offline_claimed,
        reviewed_by: None,
        review_note: None,
        pseudonymized: false,
    };
    tx_create(db, tx, COLLECTION, &w.id, &doc)?;
    let action = match (approved, w.offline_claimed) {
        (true, true) => "delivery.admin_approval_offline",
        (true, false) => "delivery.admin_approval",
        (false, true) => "delivery.create_offline",
        (false, false) => "delivery.create",
    };
    audit::add(
        db,
        tx,
        &AuditEntry::new(action, "delivery", &w.id)
            .by(&w.volunteer.id)
            .in_shelter(&w.shelter_id)
            .basis(LegalBasis::FraudPrevention),
    )?;
    Ok(doc)
}

/// A delivery that already exists answers the same request again (same shelter, same volunteer);
/// the same id from anyone else is a conflict.
fn same_delivery(
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

/// A revalidation that did not match: the capture is spent, nothing else is written but the audit.
async fn record_failed_revalidation(
    state: &AppState,
    volunteer: &Volunteer,
    shelter_id: &str,
    capture_id: &str,
    person_id: &str,
    resolved: &str,
) -> Result<(), ApiError> {
    store::transact(&state.db, |db, tx| {
        let (volunteer, shelter_id, capture_id, person_id, resolved) = (
            volunteer.clone(),
            shelter_id.to_string(),
            capture_id.to_string(),
            person_id.to_string(),
            resolved.to_string(),
        );
        Box::pin(async move {
            let capture: CaptureDoc = store::get(&db, CAPTURES, &capture_id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "capture_not_found"))?;
            if capture.consumed {
                return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
            }
            tx_replace(
                &db,
                tx,
                CAPTURES,
                &capture_id,
                &auth::consumed(capture, &resolved),
            )?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new("delivery.revalidation_failed", "person", &person_id)
                    .by(&volunteer.id)
                    .in_shelter(&shelter_id)
                    .basis(LegalBasis::FraudPrevention),
            )
        })
    })
    .await
}

// ---- the answer ----

/// `{delivery_id, status, reasons}` (spec 8.6). What is told of the other delivery follows
/// `previous_visibility`.
async fn answer(
    db: &FirestoreDb,
    id: &str,
    doc: &DeliveryDoc,
    requester: &ShelterDoc,
    requester_id: &str,
) -> Result<Value, ApiError> {
    let mut reasons = Vec::new();
    for reason in &doc.reasons {
        let mut view = json!({ "kind": reason.kind });
        if let Some(previous_id) = &reason.previous_delivery_id {
            view["previous_delivery_id"] = json!(previous_id);
            if let Some(previous) = store::get::<DeliveryDoc>(db, COLLECTION, previous_id).await? {
                let other: Option<ShelterDoc> =
                    store::get(db, shelters::COLLECTION, &previous.shelter_id).await?;
                let (date, name) = previous_visibility(
                    requester,
                    requester_id,
                    &previous.shelter_id,
                    other.as_ref(),
                );
                if date {
                    view["previous_delivered_at"] = json!(previous.delivered_at.to_string());
                }
                if name {
                    view["previous_shelter_name"] = json!(previous.shelter_name);
                }
            }
        }
        reasons.push(view);
    }
    Ok(json!({ "delivery_id": id, "status": doc.status, "reasons": reasons }))
}

// ---- POST /v1/shelters/{id}/deliveries ----

pub async fn post_delivery(
    State(state): State<AppState>,
    headers: HeaderMap,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
    Json(request): Json<DeliveryRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if volunteer.role_in(&shelter_id).is_none() {
        return Err(forbidden());
    }
    let shelter: ShelterDoc = store::get(&state.db, shelters::COLLECTION, &shelter_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found"))?;
    if uuid::Uuid::parse_str(&request.id).is_err() {
        return Err(invalid("id"));
    }
    // The same request again gets the same answer, before anything else is asked of it.
    if let Some(existing) = store::get::<DeliveryDoc>(&state.db, COLLECTION, &request.id).await? {
        let existing = same_delivery(existing, &shelter_id, &volunteer.id)?;
        return Ok((
            StatusCode::CREATED,
            Json(answer(&state.db, &request.id, &existing, &shelter, &shelter_id).await?),
        ));
    }
    if request.quantity == 0 || request.quantity > MAX_QUANTITY {
        return Err(invalid("quantity"));
    }
    let delivered_at: FirestoreInstant = request
        .delivered_at
        .parse()
        .map_err(|_| invalid("delivered_at"))?;
    let received_at = now();
    if !delivered_at_in_range(delivered_at, received_at) {
        return Err(invalid("delivered_at"));
    }
    let spec = shelter
        .items
        .get(&request.item)
        .cloned()
        .ok_or_else(|| invalid("item"))?;
    events::ensure_shelter_open(&state.db, &shelter_id).await?;
    let card_code = normalize_card(&request.card_code);
    let (household_id, person, person_id) =
        load_target(&state.db, &card_code, &request.person_id).await?;

    // `offline` is the app's word: it is believed only for a delivery that really waited.
    let offline_claimed = offline_claim_accepted(request.offline, delivered_at, received_at);
    let unavailable = offline_claimed || degraded::blocking_reason(&state.db).await?.is_some();
    let sampled = !unavailable && is_sampled(&request.id, state.config.delivery_sample_rate);
    // The admin's approval stands in for a selfie, so it only counts where a selfie would be asked.
    let approval = request.admin_approval && (spec.high_value || sampled);
    if approval && !volunteer.is_admin_of(&shelter_id) {
        return Err(ApiError::admin_approval_required());
    }
    let decision = decide(
        spec.high_value,
        sampled,
        unavailable,
        approval,
        request.capture_id.is_some(),
    );
    let mut capture_id = None;
    match (decision, &request.capture_id) {
        (Decision::Ask, _) => {
            let mut error = ApiError::liveness_required();
            error.detail = json!({
                "reason": if spec.high_value { "high_value" } else { "sample" },
                "admin_approval_only": person.level == Level::NoBiometrics,
            });
            return Err(error);
        }
        (Decision::Revalidate, Some(id)) => {
            let device = auth::device_id(&headers)?;
            let mut capture = usable_capture(&state, &device, id, Purpose::Revalidation).await?;
            if capture.volunteer_id.as_deref() != Some(volunteer.id.as_str())
                || capture.shelter_id.as_deref() != Some(shelter_id.as_str())
                || ![Some(request.person_id.as_str()), Some(person_id.as_str())]
                    .contains(&capture.expected_person_id.as_deref())
            {
                return Err(invalid("capture_id"));
            }
            let resolved = resolve_person(&state, id, &mut capture).await?;
            if resolved != person_id {
                record_failed_revalidation(
                    &state,
                    &volunteer,
                    &shelter_id,
                    id,
                    &person_id,
                    &resolved,
                )
                .await?;
                return Err(ApiError::revalidation_failed());
            }
            capture_id = Some(id.clone());
        }
        _ => {}
    }

    let write = Write {
        id: request.id.clone(),
        shelter_id: shelter_id.clone(),
        shelter: shelter.clone(),
        volunteer,
        card_code,
        person_id,
        household_id,
        item: request.item,
        spec,
        quantity: request.quantity,
        delivered_at,
        decision,
        offline_claimed,
        capture_id,
    };
    let doc = store::transact(&state.db, |db, tx| {
        let write = write.clone();
        Box::pin(async move { record(&db, tx, &write).await })
    })
    .await?;
    events::touch(&state, shelter.event_id.as_deref()).await;
    Ok((
        StatusCode::CREATED,
        Json(answer(&state.db, &request.id, &doc, &shelter, &shelter_id).await?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Purpose as P;
    use crate::exits::fixtures;
    use crate::households::tests::{World, household, person, world};
    use crate::test_support::{Actor, TestApp, all_docs, audit_actions, new_id};

    const CARD: &str = "ABCD2345";
    const DAYS: i64 = DAY_MICROS;

    fn spec(scope: ItemScope, period: Option<u32>, once: bool, high: bool) -> ItemSpec {
        ItemSpec {
            scope,
            period_days: period,
            once_per_event: once,
            high_value: high,
        }
    }

    /// An id the 5% draw leaves out, so ordinary tests are deterministic.
    fn unsampled() -> String {
        loop {
            let id = new_id();
            if !is_sampled(&id, 0.05) {
                return id;
            }
        }
    }

    fn at_instant(offset_micros: i64) -> FirestoreInstant {
        FirestoreInstant::from_microsecond(store::now().as_microsecond() + offset_micros).unwrap()
    }

    fn at(base: FirestoreInstant, offset_micros: i64) -> String {
        FirestoreInstant::from_microsecond(base.as_microsecond() + offset_micros)
            .unwrap()
            .to_string()
    }

    fn req(id: &str, person: &str, item: &str) -> Value {
        json!({
            "id": id, "card_code": CARD, "person_id": person, "item": item,
            "quantity": 1, "delivered_at": store::now().to_string(),
        })
    }

    /// A delivery the app queued offline: it says so and carries a `delivered_at` from minutes ago.
    fn offline(body: Value) -> Value {
        with(
            with(body, "offline", json!(true)),
            "delivered_at",
            json!(at(store::now(), -10 * 60_000_000)),
        )
    }

    fn with(mut body: Value, key: &str, value: Value) -> Value {
        body[key] = value;
        body
    }

    async fn deliver(
        app: &TestApp,
        who: &Actor,
        shelter: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let auth = format!("Bearer {}", who.secret);
        let (status, _, json) = app
            .request(
                "POST",
                &format!("/v1/shelters/{shelter}/deliveries"),
                &[
                    ("authorization", auth.as_str()),
                    ("x-device-id", who.device.as_str()),
                ],
                Some(body),
            )
            .await;
        (status, json)
    }

    async fn set_items(app: &TestApp, shelter: &str, items: &[(&str, ItemSpec)]) {
        let mut doc: ShelterDoc = store::get(&app.state.db, shelters::COLLECTION, shelter)
            .await
            .unwrap()
            .unwrap();
        doc.items = items
            .iter()
            .map(|(name, spec)| (name.to_string(), spec.clone()))
            .collect();
        store::replace(&app.state.db, shelters::COLLECTION, shelter, &doc)
            .await
            .unwrap();
    }

    async fn stored(app: &TestApp, id: &str) -> DeliveryDoc {
        store::get(&app.state.db, COLLECTION, id)
            .await
            .unwrap()
            .unwrap()
    }

    async fn issue_card(w: &World, household: &str, code: &str) {
        let (status, body) = w
            .app
            .send(
                "POST",
                &format!("/v1/households/{household}/cards"),
                &w.vol_a.secret,
                Some(json!({ "code": code, "client_issued_at": "2026-10-21T10:00:00Z" })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    /// A person of `level` in shelter A, stored under the id Unico's token `token` gives.
    async fn known_person(w: &World, token: &str, level: crate::people::Level) -> String {
        let id = w.app.state.person_id_for_token(token);
        fixtures::put(&w.app, &id, &fixtures::person(level, Some(&w.a))).await;
        id
    }

    struct S {
        w: World,
        p1: String,
        p2: String,
        household: String,
    }

    /// Shelter A (unconfirmed) with a household of two people without biometrics and its card.
    async fn setup() -> S {
        let w = world().await;
        let p1 = person(&w.app, "Ana", Some(&w.a)).await;
        let p2 = person(&w.app, "Bia", Some(&w.a)).await;
        let household = household(&w.app, &w.vol_a, &w.a, &[&p1, &p2]).await;
        issue_card(&w, &household, CARD).await;
        set_items(
            &w.app,
            &w.a,
            &[
                ("cesta", spec(ItemScope::PerHousehold, None, false, false)),
                ("kit", spec(ItemScope::PerPerson, Some(7), false, false)),
                ("colchao", spec(ItemScope::PerPerson, None, true, true)),
                ("tv", spec(ItemScope::PerHousehold, Some(30), false, true)),
            ],
        )
        .await;
        S {
            w,
            p1,
            p2,
            household,
        }
    }

    async fn revalidation_capture(
        app: &TestApp,
        who: &Actor,
        shelter: &str,
        person: &str,
        token: &str,
    ) -> String {
        let auth = format!("Bearer {}", who.secret);
        let (status, _, body) = app
            .request(
                "POST",
                "/v1/captures",
                &[
                    ("authorization", auth.as_str()),
                    ("x-device-id", who.device.as_str()),
                ],
                Some(json!({
                    "purpose": "revalidation", "shelter_id": shelter, "expected_person_id": person,
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let id = body["capture_id"].as_str().unwrap().to_string();
        let doc: CaptureDoc = store::get(&app.state.db, CAPTURES, &id)
            .await
            .unwrap()
            .unwrap();
        app.unico.finish_with_token(&doc.process_id, token);
        id
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
    fn card_codes_are_read_as_crockford() {
        assert_eq!(normalize_card(" abcd2o4l "), "ABCD2041");
    }

    // ---- criteria 25, 27, 28: ordinary deliveries ----

    #[tokio::test]
    async fn an_ordinary_delivery_is_recorded_on_the_card_without_asking_unico() {
        let s = setup().await;
        let (posts, gets) = (s.w.app.unico.post_count(), s.w.app.unico.get_count());
        let id = unsampled();
        let (status, body) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&id, &s.p1, "kit")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(
            body,
            json!({ "delivery_id": id, "status": "ok", "reasons": [] })
        );
        let doc = stored(&s.w.app, &id).await;
        assert_eq!(doc.verification, Verification::Card);
        assert_eq!(doc.status, DeliveryStatus::Ok);
        assert_eq!(doc.approved_by, None);
        assert_eq!(doc.person_id.as_deref(), Some(s.p1.as_str()));
        assert_eq!(doc.household_id.as_deref(), Some(s.household.as_str()));
        assert_eq!(doc.delivered_by, s.w.vol_a.id);
        assert_eq!(doc.shelter_name, "Abrigo A");
        assert_eq!(
            (s.w.app.unico.post_count(), s.w.app.unico.get_count()),
            (posts, gets)
        );
        let audit = all_docs(&s.w.app.state.db, "audit_log").await;
        let entry = audit
            .iter()
            .find(|e| e["action"] == "delivery.create")
            .unwrap();
        assert_eq!(entry["legal_basis"], "fraud_prevention");
        assert_eq!(entry["target_id"], id);
    }

    #[tokio::test]
    async fn a_person_who_declined_the_selfie_gets_an_ordinary_delivery() {
        // criterion 25: `person` writes people who declined (no_biometrics, biometrics_declined)
        let s = setup().await;
        let declined = stored_person(&s.w.app, &s.p1).await;
        assert!(declined.biometrics_declined);
        let (status, body) = deliver(
            &s.w.app,
            &s.w.vol_a,
            &s.w.a,
            req(&unsampled(), &s.p1, "kit"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["status"], "ok");
    }

    async fn stored_person(app: &TestApp, id: &str) -> crate::people::PersonDoc {
        crate::households::tests::stored_person(app, id).await
    }

    #[tokio::test]
    async fn an_assisted_person_receives_like_anyone_else_without_428() {
        let s = setup().await;
        let ana = known_person(&s.w, "tok-assisted", crate::people::Level::Assisted).await;
        let h = household(&s.w.app, &s.w.vol_a, &s.w.a, &[&ana]).await;
        issue_card(&s.w, &h, "EFGH6789").await;
        let body = with(
            req(&unsampled(), &ana, "kit"),
            "card_code",
            json!("EFGH6789"),
        );
        let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(
            stored(&s.w.app, answer["delivery_id"].as_str().unwrap())
                .await
                .verification,
            Verification::Card
        );
    }

    // ---- criterion 30: the draw and retries ----

    #[tokio::test]
    async fn a_drawn_delivery_asks_for_liveness_every_time_it_is_sent() {
        let mut s = setup().await;
        let mut config = (*s.w.app.state.config).clone();
        config.delivery_sample_rate = 1.0;
        s.w.app.state.config = std::sync::Arc::new(config);
        let id = new_id();
        for _ in 0..2 {
            let (status, body) =
                deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&id, &s.p1, "kit")).await;
            assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}");
            assert_eq!(body["error"], "liveness_required");
            assert_eq!(body["detail"]["reason"], "sample");
            assert_eq!(body["detail"]["admin_approval_only"], true);
        }
        assert!(all_docs(&s.w.app.state.db, COLLECTION).await.is_empty());
    }

    #[tokio::test]
    async fn the_same_delivery_sent_again_gets_the_same_answer_and_one_document() {
        let s = setup().await;
        let id = unsampled();
        let body = req(&id, &s.p1, "cesta");
        let first = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body.clone()).await;
        let second = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body.clone()).await;
        assert_eq!(first, second);
        assert_eq!(all_docs(&s.w.app.state.db, COLLECTION).await.len(), 1);
        // another volunteer cannot take the id
        let (status, answer) = deliver(&s.w.app, &s.w.admin_a, &s.w.a, body).await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::CONFLICT, Some("delivery_id_taken"))
        );
    }

    #[tokio::test]
    async fn the_idempotency_key_replays_the_answer_of_a_duplicate_too() {
        let s = setup().await;
        let first_id = unsampled();
        deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&first_id, &s.p1, "cesta")).await;
        let id = unsampled();
        let path = format!("/v1/shelters/{}/deliveries", s.w.a);
        let body = req(&id, &s.p2, "cesta");
        let a =
            s.w.app
                .send_keyed("POST", &path, &s.w.vol_a.secret, "k1", Some(body.clone()))
                .await;
        let b =
            s.w.app
                .send_keyed("POST", &path, &s.w.vol_a.secret, "k1", Some(body))
                .await;
        assert_eq!(a.0, StatusCode::CREATED);
        assert_eq!(a, b);
        assert_eq!(a.1["status"], "in_review");
        assert_eq!(all_docs(&s.w.app.state.db, COLLECTION).await.len(), 2);
    }

    // ---- criterion 29: high value ----

    #[tokio::test]
    async fn high_value_without_capture_asks_for_liveness() {
        let s = setup().await;
        let ana = known_person(&s.w, "tok-v", crate::people::Level::Verified).await;
        let h = household(&s.w.app, &s.w.vol_a, &s.w.a, &[&ana]).await;
        issue_card(&s.w, &h, "EFGH6789").await;
        let body = with(
            req(&new_id(), &ana, "colchao"),
            "card_code",
            json!("EFGH6789"),
        );
        let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
        assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{answer}");
        assert_eq!(answer["detail"]["reason"], "high_value");
        assert_eq!(answer["detail"]["admin_approval_only"], false);
    }

    #[tokio::test]
    async fn a_capture_of_the_right_person_records_a_liveness_delivery_and_is_consumed() {
        let s = setup().await;
        let ana = known_person(&s.w, "tok-v", crate::people::Level::Verified).await;
        let h = household(&s.w.app, &s.w.vol_a, &s.w.a, &[&ana]).await;
        issue_card(&s.w, &h, "EFGH6789").await;
        let capture = revalidation_capture(&s.w.app, &s.w.vol_a, &s.w.a, &ana, "tok-v").await;
        let id = new_id();
        let body = with(
            with(req(&id, &ana, "colchao"), "card_code", json!("EFGH6789")),
            "capture_id",
            json!(capture),
        );
        let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(answer["status"], "ok");
        assert_eq!(
            stored(&s.w.app, &id).await.verification,
            Verification::Liveness
        );
        let doc: CaptureDoc = store::get(&s.w.app.state.db, CAPTURES, &capture)
            .await
            .unwrap()
            .unwrap();
        assert!(doc.consumed);
        // sent again by the same volunteer: same answer, Unico not asked again
        let gets = s.w.app.unico.get_count();
        let again = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
        assert_eq!(again, (StatusCode::CREATED, answer));
        assert_eq!(s.w.app.unico.get_count(), gets);
    }

    #[tokio::test]
    async fn a_capture_of_someone_else_is_refused_and_nothing_is_recorded() {
        let s = setup().await;
        let ana = known_person(&s.w, "tok-v", crate::people::Level::Verified).await;
        let h = household(&s.w.app, &s.w.vol_a, &s.w.a, &[&ana]).await;
        issue_card(&s.w, &h, "EFGH6789").await;
        let capture =
            revalidation_capture(&s.w.app, &s.w.vol_a, &s.w.a, &ana, "tok-someone-else").await;
        let body = with(
            with(
                req(&new_id(), &ana, "colchao"),
                "card_code",
                json!("EFGH6789"),
            ),
            "capture_id",
            json!(capture),
        );
        let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body.clone()).await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::CONFLICT, Some("revalidation_failed"))
        );
        assert!(all_docs(&s.w.app.state.db, COLLECTION).await.is_empty());
        let doc: CaptureDoc = store::get(&s.w.app.state.db, CAPTURES, &capture)
            .await
            .unwrap()
            .unwrap();
        assert!(doc.consumed);
        assert!(
            audit_actions(&s.w.app.state.db)
                .await
                .contains(&"delivery.revalidation_failed".to_string())
        );
        // the spent capture cannot be used again
        let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::CONFLICT, Some("capture_consumed"))
        );
    }

    #[tokio::test]
    async fn a_capture_asked_for_another_person_or_volunteer_is_refused() {
        let s = setup().await;
        let ana = known_person(&s.w, "tok-v", crate::people::Level::Verified).await;
        let h = household(&s.w.app, &s.w.vol_a, &s.w.a, &[&ana]).await;
        issue_card(&s.w, &h, "EFGH6789").await;
        // asked for someone else of the household
        let capture = revalidation_capture(&s.w.app, &s.w.vol_a, &s.w.a, &s.p1, "tok-v").await;
        let body = with(
            with(
                req(&new_id(), &ana, "colchao"),
                "card_code",
                json!("EFGH6789"),
            ),
            "capture_id",
            json!(capture),
        );
        let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
        assert!(all_docs(&s.w.app.state.db, COLLECTION).await.is_empty());
    }

    #[tokio::test]
    async fn admin_approval_by_an_admin_is_ok_audited_and_not_reviewed() {
        let s = setup().await;
        let id = new_id();
        let body = with(req(&id, &s.p1, "colchao"), "admin_approval", json!(true));
        let (status, answer) = deliver(&s.w.app, &s.w.admin_a, &s.w.a, body).await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(answer["status"], "ok");
        let doc = stored(&s.w.app, &id).await;
        assert_eq!(doc.verification, Verification::AdminApproval);
        assert_eq!(doc.approved_by.as_deref(), Some(s.w.admin_a.id.as_str()));
        assert_eq!(doc.delivered_by, s.w.admin_a.id);
        let audit = all_docs(&s.w.app.state.db, "audit_log").await;
        let entry = audit
            .iter()
            .find(|e| e["action"] == "delivery.admin_approval")
            .unwrap();
        assert_eq!(entry["volunteer_id"], s.w.admin_a.id);
        assert_eq!(entry["legal_basis"], "fraud_prevention");
    }

    #[tokio::test]
    async fn admin_approval_by_a_volunteer_is_refused() {
        let s = setup().await;
        let body = with(
            req(&new_id(), &s.p1, "colchao"),
            "admin_approval",
            json!(true),
        );
        let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::FORBIDDEN, Some("admin_approval_required"))
        );
        assert!(all_docs(&s.w.app.state.db, COLLECTION).await.is_empty());
    }

    // ---- criteria 4 and 33: offline and degraded ----

    #[tokio::test]
    async fn offline_high_value_without_an_admin_goes_to_review_and_with_one_is_ok() {
        let s = setup().await;
        let id = new_id();
        let body = offline(req(&id, &s.p1, "colchao"));
        let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(answer["status"], "in_review");
        assert_eq!(
            answer["reasons"],
            json!([{ "kind": "liveness_unavailable" }])
        );
        assert_eq!(stored(&s.w.app, &id).await.verification, Verification::Card);

        let approved = with(
            offline(req(&new_id(), &s.p2, "colchao")),
            "admin_approval",
            json!(true),
        );
        let (status, answer) = deliver(&s.w.app, &s.w.admin_a, &s.w.a, approved).await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(answer["status"], "ok");
    }

    #[tokio::test]
    async fn offline_has_no_draw_and_never_asks_unico() {
        let mut s = setup().await;
        let mut config = (*s.w.app.state.config).clone();
        config.delivery_sample_rate = 1.0;
        s.w.app.state.config = std::sync::Arc::new(config);
        let (posts, gets) = (s.w.app.unico.post_count(), s.w.app.unico.get_count());
        let body = offline(req(&new_id(), &s.p1, "kit"));
        let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(answer["status"], "ok");
        assert_eq!(
            (s.w.app.unico.post_count(), s.w.app.unico.get_count()),
            (posts, gets)
        );
    }

    #[tokio::test]
    async fn degraded_mode_works_like_offline() {
        let mut s = setup().await;
        let mut config = (*s.w.app.state.config).clone();
        config.delivery_sample_rate = 1.0;
        s.w.app.state.config = std::sync::Arc::new(config);
        degraded::enter_limit(&s.w.app.state.db).await.unwrap();
        // ordinary item: no draw, recorded on the card
        let (status, answer) =
            deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&new_id(), &s.p1, "kit")).await;
        assert_eq!(
            (status, answer["status"].as_str()),
            (StatusCode::CREATED, Some("ok"))
        );
        // high value without an admin: review
        let (status, answer) = deliver(
            &s.w.app,
            &s.w.vol_a,
            &s.w.a,
            req(&new_id(), &s.p1, "colchao"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(answer["reasons"][0]["kind"], "liveness_unavailable");
        // high value with an admin's approval: ok
        let body = with(
            req(&new_id(), &s.p2, "colchao"),
            "admin_approval",
            json!(true),
        );
        let (status, answer) = deliver(&s.w.app, &s.w.admin_a, &s.w.a, body).await;
        assert_eq!(
            (status, answer["status"].as_str()),
            (StatusCode::CREATED, Some("ok"))
        );
    }

    #[tokio::test]
    async fn offline_high_value_that_is_also_a_duplicate_carries_both_reasons() {
        let s = setup().await;
        let first = req(&new_id(), &s.p1, "colchao");
        let first = with(first, "admin_approval", json!(true));
        deliver(&s.w.app, &s.w.admin_a, &s.w.a, first).await;
        let body = offline(req(&new_id(), &s.p1, "colchao"));
        let (_, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
        let kinds: Vec<&str> = answer["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["duplicate_in_period", "liveness_unavailable"]);
    }

    // ---- criteria 31 and 32: duplicates ----

    #[tokio::test]
    async fn per_household_flags_the_second_member_and_per_person_does_not() {
        let s = setup().await;
        let first = unsampled();
        let (_, a) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&first, &s.p1, "cesta")).await;
        assert_eq!(a["status"], "ok");
        let (status, b) = deliver(
            &s.w.app,
            &s.w.vol_a,
            &s.w.a,
            req(&unsampled(), &s.p2, "cesta"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{b}");
        assert_eq!(b["status"], "in_review");
        assert_eq!(b["reasons"][0]["kind"], "duplicate_in_period");
        assert_eq!(b["reasons"][0]["previous_delivery_id"], first);
        assert!(b["reasons"][0]["previous_delivered_at"].is_string());

        deliver(
            &s.w.app,
            &s.w.vol_a,
            &s.w.a,
            req(&unsampled(), &s.p1, "kit"),
        )
        .await;
        let (_, c) = deliver(
            &s.w.app,
            &s.w.vol_a,
            &s.w.a,
            req(&unsampled(), &s.p2, "kit"),
        )
        .await;
        assert_eq!(c["status"], "ok");
        // the same person again is flagged, and still recorded
        let (status, d) = deliver(
            &s.w.app,
            &s.w.vol_a,
            &s.w.a,
            req(&unsampled(), &s.p1, "kit"),
        )
        .await;
        assert_eq!(
            (status, d["status"].as_str()),
            (StatusCode::CREATED, Some("in_review"))
        );
    }

    #[tokio::test]
    async fn the_period_limits_and_the_default_of_seven_days() {
        let s = setup().await;
        let base = at_instant(-29 * DAYS);
        let send = |offset: i64| {
            let body = with(
                req(&unsampled(), &s.p1, "kit"),
                "delivered_at",
                json!(at(base, offset)),
            );
            deliver(&s.w.app, &s.w.vol_a, &s.w.a, body)
        };
        assert_eq!(send(0).await.1["status"], "ok");
        assert_eq!(send(7 * DAYS).await.1["status"], "ok");
        assert_eq!(send(7 * DAYS + DAYS / 2).await.1["status"], "in_review");
        assert_eq!(send(20 * DAYS).await.1["status"], "ok");
        // an item without period_days uses 7
        let body = |offset: i64| {
            with(
                req(&unsampled(), &s.p1, "cesta"),
                "delivered_at",
                json!(at(base, offset)),
            )
        };
        assert_eq!(
            deliver(&s.w.app, &s.w.vol_a, &s.w.a, body(0)).await.1["status"],
            "ok"
        );
        assert_eq!(
            deliver(&s.w.app, &s.w.vol_a, &s.w.a, body(7 * DAYS - 1))
                .await
                .1["status"],
            "in_review"
        );
    }

    #[tokio::test]
    async fn once_per_event_is_flagged_even_after_the_period() {
        let s = setup().await;
        let base = store::now();
        let old = with(
            with(
                req(&new_id(), &s.p1, "colchao"),
                "delivered_at",
                json!(at(base, -25 * DAYS)),
            ),
            "admin_approval",
            json!(true),
        );
        let (_, a) = deliver(&s.w.app, &s.w.admin_a, &s.w.a, old).await;
        assert_eq!(a["status"], "ok");
        let again = with(
            with(
                req(&new_id(), &s.p1, "colchao"),
                "admin_approval",
                json!(true),
            ),
            "delivered_at",
            json!(at(base, 0)),
        );
        let (status, b) = deliver(&s.w.app, &s.w.admin_a, &s.w.a, again).await;
        assert_eq!(status, StatusCode::CREATED, "{b}");
        assert_eq!(b["status"], "in_review");
        assert_eq!(b["reasons"][0]["kind"], "duplicate_in_period");
        // another person is not affected (per_person)
        let other = with(
            req(&new_id(), &s.p2, "colchao"),
            "admin_approval",
            json!(true),
        );
        assert_eq!(
            deliver(&s.w.app, &s.w.admin_a, &s.w.a, other).await.1["status"],
            "ok"
        );
    }

    #[tokio::test]
    async fn duplicates_are_looked_for_in_every_active_event_and_only_there() {
        let s = setup().await;
        set_items(
            &s.w.app,
            &s.w.b,
            &[("cesta", spec(ItemScope::PerHousehold, None, false, false))],
        )
        .await;
        // the card works in any shelter: first at B (confirmed, another event)
        let first = unsampled();
        let (status, a) = deliver(&s.w.app, &s.w.vol_b, &s.w.b, req(&first, &s.p1, "cesta")).await;
        assert_eq!(status, StatusCode::CREATED, "{a}");
        let (_, b) = deliver(
            &s.w.app,
            &s.w.vol_a,
            &s.w.a,
            req(&unsampled(), &s.p2, "cesta"),
        )
        .await;
        assert_eq!(b["status"], "in_review");
        assert_eq!(b["reasons"][0]["previous_delivery_id"], first);
        // the requester, shelter A, is not confirmed: it is told nothing of the other shelter's delivery
        assert!(
            b["reasons"][0].get("previous_shelter_name").is_none(),
            "{b}"
        );
        assert!(
            b["reasons"][0].get("previous_delivered_at").is_none(),
            "{b}"
        );

        // once the event of B is closed its deliveries no longer count
        let shelter_b: ShelterDoc = store::get(&s.w.app.state.db, shelters::COLLECTION, &s.w.b)
            .await
            .unwrap()
            .unwrap();
        let event_id = shelter_b.event_id.unwrap();
        let mut event: EventDoc = store::get(&s.w.app.state.db, events::COLLECTION, &event_id)
            .await
            .unwrap()
            .unwrap();
        event.status = EventStatus::Closed;
        store::replace(&s.w.app.state.db, events::COLLECTION, &event_id, &event)
            .await
            .unwrap();
        let (_, c) = deliver(
            &s.w.app,
            &s.w.vol_a,
            &s.w.a,
            req(&unsampled(), &s.p1, "kit"),
        )
        .await;
        assert_eq!(c["status"], "ok");
        let (_, d) = deliver(
            &s.w.app,
            &s.w.vol_a,
            &s.w.a,
            req(&unsampled(), &s.p2, "cesta"),
        )
        .await;
        // the duplicate of A's own first cesta (the in-review one) still counts, B's does not
        assert_eq!(d["reasons"][0]["previous_delivery_id"], b["delivery_id"]);
    }

    #[tokio::test]
    async fn the_shelter_of_the_other_delivery_is_not_shown_when_it_is_unconfirmed() {
        let s = setup().await;
        set_items(
            &s.w.app,
            &s.w.b,
            &[("cesta", spec(ItemScope::PerHousehold, None, false, false))],
        )
        .await;
        let first = unsampled();
        deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&first, &s.p1, "cesta")).await;
        let (status, b) = deliver(
            &s.w.app,
            &s.w.vol_b,
            &s.w.b,
            req(&unsampled(), &s.p2, "cesta"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{b}");
        assert_eq!(b["reasons"][0]["previous_delivery_id"], first);
        assert!(
            b["reasons"][0].get("previous_shelter_name").is_none(),
            "{b}"
        );
        // and the replay of that answer does not leak it either
        let again = deliver(
            &s.w.app,
            &s.w.vol_b,
            &s.w.b,
            req(b["delivery_id"].as_str().unwrap(), &s.p2, "cesta"),
        )
        .await;
        assert!(again.1["reasons"][0].get("previous_shelter_name").is_none());
    }

    // ---- two deliveries at once ----

    #[tokio::test]
    async fn two_simultaneous_deliveries_to_one_household_flag_exactly_one() {
        let s = setup().await;
        let names: Vec<String> = (0..4).map(|n| format!("pacote{n}")).collect();
        let items: Vec<(&str, ItemSpec)> = names
            .iter()
            .map(|n| {
                (
                    n.as_str(),
                    spec(ItemScope::PerHousehold, None, false, false),
                )
            })
            .collect();
        set_items(&s.w.app, &s.w.a, &items).await;
        for name in &names {
            let (a, b) = tokio::join!(
                deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&unsampled(), &s.p1, name)),
                deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&unsampled(), &s.p2, name)),
            );
            assert_eq!(
                (a.0, b.0),
                (StatusCode::CREATED, StatusCode::CREATED),
                "{a:?} {b:?}"
            );
            let mut statuses = [
                a.1["status"].as_str().unwrap().to_string(),
                b.1["status"].as_str().unwrap().to_string(),
            ];
            statuses.sort();
            assert_eq!(statuses, ["in_review", "ok"], "{name}");
        }
    }

    // ---- refusals ----

    #[tokio::test]
    async fn who_may_deliver() {
        let s = setup().await;
        let path = format!("/v1/shelters/{}/deliveries", s.w.a);
        let (status, _, answer) =
            s.w.app
                .request("POST", &path, &[], Some(req(&unsampled(), &s.p1, "kit")))
                .await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::UNAUTHORIZED, Some("unauthorized"))
        );
        for who in [
            &s.w.outsider,
            &s.w.vol_b,
            &s.w.validator,
            &s.w.civil_defense,
        ] {
            let (status, answer) =
                deliver(&s.w.app, who, &s.w.a, req(&unsampled(), &s.p1, "kit")).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
        }
        // volunteers and admins of an unconfirmed shelter deliver (matrix 2.3)
        for who in [&s.w.vol_a, &s.w.admin_a] {
            let (status, answer) =
                deliver(&s.w.app, who, &s.w.a, req(&unsampled(), &s.p1, "kit")).await;
            assert_eq!(status, StatusCode::CREATED, "{answer}");
        }
        assert!(all_docs(&s.w.app.state.db, COLLECTION).await.len() == 2);
    }

    #[tokio::test]
    async fn the_card_the_person_and_the_item_must_exist() {
        let s = setup().await;
        let other = person(&s.w.app, "Carla", Some(&s.w.a)).await;
        let cases = [
            (
                with(
                    req(&unsampled(), &s.p1, "kit"),
                    "card_code",
                    json!("ZZZZZZZZ"),
                ),
                StatusCode::NOT_FOUND,
                "card_not_found",
            ),
            (
                req(&unsampled(), &other, "kit"),
                StatusCode::NOT_FOUND,
                "person_not_in_household",
            ),
            (
                req(&unsampled(), &s.p1, "unknown"),
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                with(req(&unsampled(), &s.p1, "kit"), "quantity", json!(0)),
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                with(
                    req(&unsampled(), &s.p1, "kit"),
                    "delivered_at",
                    json!("yesterday"),
                ),
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                with(req(&unsampled(), &s.p1, "kit"), "id", json!("not-a-uuid")),
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
        ];
        for (body, expected, code) in cases {
            let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body.clone()).await;
            assert_eq!(
                (status, answer["error"].as_str()),
                (expected, Some(code)),
                "{body}"
            );
        }
        let (_, answer) = deliver(
            &s.w.app,
            &s.w.vol_a,
            &s.w.a,
            req(&unsampled(), &s.p1, "unknown"),
        )
        .await;
        assert_eq!(answer["detail"]["field"], "item");
        // a canceled card no longer works
        let (status, _) =
            s.w.app
                .send(
                    "POST",
                    &format!("/v1/cards/{CARD}/cancel"),
                    &s.w.vol_a.secret,
                    Some(json!({})),
                )
                .await;
        assert_eq!(status, StatusCode::OK);
        let (status, answer) = deliver(
            &s.w.app,
            &s.w.vol_a,
            &s.w.a,
            req(&unsampled(), &s.p1, "kit"),
        )
        .await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::NOT_FOUND, Some("card_not_found"))
        );
        assert!(all_docs(&s.w.app.state.db, COLLECTION).await.is_empty());
    }

    #[tokio::test]
    async fn a_closed_event_takes_no_deliveries() {
        let s = setup().await;
        let shelter: ShelterDoc = store::get(&s.w.app.state.db, shelters::COLLECTION, &s.w.a)
            .await
            .unwrap()
            .unwrap();
        let event_id = shelter.event_id.unwrap();
        let mut event: EventDoc = store::get(&s.w.app.state.db, events::COLLECTION, &event_id)
            .await
            .unwrap()
            .unwrap();
        event.status = EventStatus::Closed;
        store::replace(&s.w.app.state.db, events::COLLECTION, &event_id, &event)
            .await
            .unwrap();
        let (status, answer) = deliver(
            &s.w.app,
            &s.w.vol_a,
            &s.w.a,
            req(&unsampled(), &s.p1, "kit"),
        )
        .await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::CONFLICT, Some("event_closed"))
        );
    }

    // ---- late biometrics (spec 4.12) ----

    #[tokio::test]
    async fn late_biometrics_repoints_deliveries_and_the_duplicate_follows_the_new_id() {
        let s = setup().await;
        let first = unsampled();
        deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&first, &s.p1, "kit")).await;
        let auth = format!("Bearer {}", s.w.vol_a.secret);
        let (status, _, body) =
            s.w.app
                .request(
                    "POST",
                    "/v1/captures",
                    &[
                        ("authorization", auth.as_str()),
                        ("x-device-id", s.w.vol_a.device.as_str()),
                    ],
                    Some(json!({ "purpose": "biometrics", "expected_person_id": s.p1 })),
                )
                .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let capture = body["capture_id"].as_str().unwrap().to_string();
        let doc: CaptureDoc = store::get(&s.w.app.state.db, CAPTURES, &capture)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(doc.purpose, P::Biometrics);
        s.w.app
            .unico
            .finish_with_token(&doc.process_id, "tok-ana-late");
        let (status, _, answer) =
            s.w.app
                .request(
                    "PUT",
                    &format!("/v1/people/{}/biometrics", s.p1),
                    &[
                        ("authorization", auth.as_str()),
                        ("x-device-id", s.w.vol_a.device.as_str()),
                    ],
                    Some(json!({ "capture_id": capture })),
                )
                .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let new = s.w.app.state.person_id_for_token("tok-ana-late");
        assert_eq!(
            stored(&s.w.app, &first).await.person_id.as_deref(),
            Some(new.as_str())
        );
        // the household now lists the new id, and a second kit is a duplicate of the first
        let (status, again) =
            deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&unsampled(), &new, "kit")).await;
        assert_eq!(status, StatusCode::CREATED, "{again}");
        assert_eq!(again["status"], "in_review");
        assert_eq!(again["reasons"][0]["previous_delivery_id"], first);
        // an offline delivery that still carries the old id is the same person (`former_ids`)
        let (status, late) = deliver(
            &s.w.app,
            &s.w.vol_a,
            &s.w.a,
            req(&unsampled(), &s.p1, "kit"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{late}");
        assert_eq!(late["status"], "in_review");
        assert_eq!(
            stored(&s.w.app, late["delivery_id"].as_str().unwrap())
                .await
                .person_id
                .as_deref(),
            Some(new.as_str())
        );
        // an id that was never theirs still leads nowhere
        let (status, _) = deliver(
            &s.w.app,
            &s.w.vol_a,
            &s.w.a,
            req(&unsampled(), &new_id(), "kit"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
