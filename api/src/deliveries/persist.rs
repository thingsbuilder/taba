//! Reads and writes around the rules: the card and the person a delivery is for, the duplicate
//! search, the transaction that records a delivery, and what the review list reads (spec 3.5.3, 4.7).

use std::collections::HashMap;

use axum::http::StatusCode;
use firestore::{FirestoreDb, FirestoreInstant, path};
use serde::{Deserialize, Serialize};

use super::domain::{Write, counts_against, same_delivery};
use super::model::{COLLECTION, DeliveryDoc, DeliveryStatus, ItemScope, Reason, Verification};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{self, AppState, CaptureDoc, Volunteer};
use crate::cards::{self, CardDoc, CardStatus};
use crate::error::ApiError;
use crate::events::{self, EventDoc, EventStatus};
use crate::households::load_household;
use crate::people::{self, PersonDoc};
use crate::shelters::ShelterDoc;
use crate::store::{self, fs_error, tx_create, tx_replace};
use crate::volunteers::{self, VolunteerDoc};

pub(super) const CAPTURES: &str = "captures";
/// Most recent deliveries of the same item and owner that the duplicate check looks at.
const MAX_PREVIOUS: u32 = 50;
/// Deliveries per page of the review list; the answer says `has_more` when the shelter has more
/// of that status than one page holds.
pub(super) const LIST_LIMIT: u32 = 100;

// ---- reads ----

/// The household of an active card and the person who takes the item, who must be a member of it.
/// A delivery made offline can reach the server after the late biometrics gave the person a new
/// id (spec 4.12): an id that is not a member but is a `former_id` of one stands for that member.
/// Returns the id the delivery is written under.
pub(super) async fn load_target(
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
        if !counts_against(w, &id, row.delivered_at) {
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

/// One transaction: re-reads the card, the household and the capture, looks for the duplicate and
/// writes the delivery. Of two deliveries of the same household the second one sees the first.
pub(super) async fn record(
    db: &FirestoreDb,
    tx: &mut firestore::FirestoreTransaction<'_>,
    w: &Write,
) -> Result<DeliveryDoc, ApiError> {
    events::ensure_shelter_open(db, &w.shelter_id).await?;
    if let Some(existing) = store::get::<DeliveryDoc>(db, COLLECTION, &w.id).await? {
        return same_delivery(existing, &w.shelter_id, &w.volunteer_id);
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

    let reasons = w.reasons(find_duplicate(db, w).await?);
    let doc = w.delivery(reasons);
    let parts = w.audit_parts();
    tx_create(db, tx, COLLECTION, &w.id, &doc)?;
    audit::add(
        db,
        tx,
        &AuditEntry::new(
            format!("delivery.{}{}{}", parts[0], parts[1], parts[2]),
            "delivery",
            &w.id,
        )
        .by(&w.volunteer_id)
        .in_shelter(&w.shelter_id)
        .basis(LegalBasis::FraudPrevention),
    )?;
    Ok(doc)
}

/// A revalidation that did not match: the capture is spent, nothing else is written but the audit.
pub(super) async fn record_failed_revalidation(
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
                &AuditEntry::new(super::ACTION_REVALIDATION_FAILED, "person", &person_id)
                    .by(&volunteer.id)
                    .in_shelter(&shelter_id)
                    .basis(LegalBasis::FraudPrevention),
            )
        })
    })
    .await
}

// ---- the review list ----

/// The fields of a delivery the list needs; the id is the document's.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Row {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub(super) id: String,
    pub(super) shelter_id: String,
    #[serde(default)]
    pub(super) person_id: Option<String>,
    #[serde(default)]
    pub(super) card_code: Option<String>,
    pub(super) item: String,
    pub(super) scope: ItemScope,
    pub(super) quantity: u32,
    pub(super) delivered_by: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub(super) delivered_at: FirestoreInstant,
    pub(super) verification: Verification,
    #[serde(default)]
    pub(super) approved_by: Option<String>,
    pub(super) status: DeliveryStatus,
    #[serde(default)]
    pub(super) reasons: Vec<Reason>,
    #[serde(default)]
    pub(super) review_note: Option<String>,
}

pub(super) async fn volunteer_name(
    db: &FirestoreDb,
    cache: &mut HashMap<String, Option<String>>,
    id: &str,
) -> Result<Option<String>, ApiError> {
    if let Some(name) = cache.get(id) {
        return Ok(name.clone());
    }
    // A pseudonymized delivery has no volunteer.
    if id.is_empty() {
        return Ok(None);
    }
    let doc: Option<VolunteerDoc> = store::get(db, volunteers::COLLECTION, id).await?;
    let name = doc.map(|d| d.name);
    cache.insert(id.to_string(), name.clone());
    Ok(name)
}

pub(super) async fn person_name(
    db: &FirestoreDb,
    cache: &mut HashMap<String, Option<String>>,
    id: &str,
) -> Result<Option<String>, ApiError> {
    if let Some(name) = cache.get(id) {
        return Ok(name.clone());
    }
    let doc: Option<PersonDoc> = store::get(db, people::COLLECTION, id).await?;
    let name = doc.and_then(|p| p.social_name.or(p.name));
    cache.insert(id.to_string(), name.clone());
    Ok(name)
}

/// Documents of the other deliveries and shelters read while building one page: many rows name the
/// same earlier delivery or shelter.
#[derive(Default)]
pub(super) struct Seen {
    pub(super) deliveries: HashMap<String, Option<DeliveryDoc>>,
    pub(super) shelters: HashMap<String, Option<ShelterDoc>>,
}

pub(super) async fn cached<T: serde::de::DeserializeOwned + Send + Clone>(
    db: &FirestoreDb,
    cache: &mut HashMap<String, Option<T>>,
    collection: &str,
    id: &str,
) -> Result<Option<T>, ApiError> {
    if let Some(doc) = cache.get(id) {
        return Ok(doc.clone());
    }
    let doc = store::get::<T>(db, collection, id).await?;
    cache.insert(id.to_string(), doc.clone());
    Ok(doc)
}
