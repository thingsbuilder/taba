//! The routes (spec 3.5.2, 3.5.3, 8.6): `POST /v1/shelters/{id}/deliveries`, `PUT
//! /v1/shelters/{id}/items`, `GET /v1/shelters/{id}/deliveries?status=` and `POST
//! /v1/deliveries/{id}/review`. They parse and authorize the request, read what the rules need and
//! write the result in one transaction, so the decision (draw, liveness, admin approval, duplicate)
//! and the write cannot be separated by a second delivery.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use firestore::{FirestoreInstant, path};
use serde::Deserialize;
use serde_json::{Value, json};

use super::domain::{
    Decision, ReviewDecision, Write, decide, is_sampled, offline_claim_accepted,
    plausible_delivered_at, same_delivery, status_name, valid_items,
};
use super::model::{COLLECTION, DeliveryDoc, DeliveryStatus, Items, delivery_not_found, invalid};
use super::persist::{
    LIST_LIMIT, Row, Seen, load_target, person_name, record, record_failed_revalidation,
    volunteer_name,
};
use super::view::{DeliveryView, answer, reason_view};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{self, AppState, Purpose, Volunteer, resolve_person, usable_capture};
use crate::cards;
use crate::degraded;
use crate::error::ApiError;
use crate::events;
use crate::households::forbidden;
use crate::people::Level;
use crate::shelters::{self, ShelterDoc};
use crate::store::{self, fs_error, now, tx_replace};

const MAX_QUANTITY: u32 = 1000;
pub(super) const MAX_NOTE_CHARS: usize = 500;

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
    let (delivered_at, delivered_at_clamped) =
        plausible_delivered_at(delivered_at, received_at).ok_or_else(|| invalid("delivered_at"))?;
    let spec = shelter
        .items
        .get(&request.item)
        .cloned()
        .ok_or_else(|| invalid("item"))?;
    events::ensure_shelter_open(&state.db, &shelter_id).await?;
    let card_code = cards::crockford(&request.card_code);
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
        volunteer_id: volunteer.id,
        card_code,
        person_id,
        household_id,
        item: request.item,
        spec,
        quantity: request.quantity,
        delivered_at,
        decision,
        offline_claimed,
        delivered_at_clamped,
        received_at,
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

// ---- PUT /v1/shelters/{id}/items ----

#[derive(Debug, Deserialize)]
pub struct PutItems {
    items: Items,
}

/// `PUT /v1/shelters/{id}/items`: `{items: {<name>: {scope, period_days?, once_per_event, high_value}}}`, by an admin of the shelter; replaces `shelters.items`.
pub async fn put_items(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(id): Path<String>,
    Json(body): Json<PutItems>,
) -> Result<Json<Value>, ApiError> {
    if !volunteer.is_admin_of(&id) {
        return Err(forbidden());
    }
    events::ensure_shelter_open(&state.db, &id).await?;
    let items = valid_items(body.items)?;

    let shelter = store::transact(&state.db, |db, tx| {
        let (id, items, volunteer) = (id.clone(), items.clone(), volunteer.clone());
        Box::pin(async move {
            let mut shelter: ShelterDoc = store::get(&db, shelters::COLLECTION, &id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found"))?;
            shelter.items = items;
            tx_replace(&db, tx, shelters::COLLECTION, &id, &shelter)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new(super::ACTION_ITEMS, "shelter", &id)
                    .basis(LegalBasis::FraudPrevention)
                    .by(&volunteer.id)
                    .in_shelter(&id),
            )?;
            Ok(shelter)
        })
    })
    .await?;
    events::touch(&state, shelter.event_id.as_deref()).await;
    Ok(Json(json!({ "items": shelter.items })))
}

// ---- GET /v1/shelters/{id}/deliveries ----

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    status: Option<String>,
}

/// `GET /v1/shelters/{id}/deliveries?status=`: the shelter's deliveries of that status, newest
/// first, by an admin of it. `status` is required: the index is `(shelter_id, status, delivered_at)`.
pub async fn list_deliveries(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(id): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    if !volunteer.is_admin_of(&id) {
        return Err(forbidden());
    }
    let status: DeliveryStatus = query
        .status
        .as_deref()
        .and_then(|s| serde_json::from_value(Value::String(s.to_string())).ok())
        .ok_or_else(|| invalid("status"))?;
    let wanted = status_name(status);
    let requester: ShelterDoc = store::get(&state.db, shelters::COLLECTION, &id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found"))?;

    let mut rows: Vec<Row> = state
        .db
        .fluent()
        .select()
        .from(COLLECTION)
        .filter(|q| {
            q.for_all([
                q.field(path!(Row::shelter_id)).eq(id.clone()),
                q.field(path!(Row::status)).eq(wanted.clone()),
            ])
        })
        .order_by([(
            path!(Row::delivered_at),
            firestore::FirestoreQueryDirection::Descending,
        )])
        .limit(LIST_LIMIT + 1)
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    let has_more = rows.len() > LIST_LIMIT as usize;
    rows.truncate(LIST_LIMIT as usize);

    let (mut volunteers_seen, mut people_seen, mut seen) =
        (HashMap::new(), HashMap::new(), Seen::default());
    let mut views = Vec::with_capacity(rows.len());
    for row in rows {
        let mut reasons = Vec::with_capacity(row.reasons.len());
        for reason in &row.reasons {
            reasons.push(reason_view(&state.db, &mut seen, &requester, &id, reason).await?);
        }
        let person_name = match &row.person_id {
            Some(person_id) => person_name(&state.db, &mut people_seen, person_id).await?,
            None => None,
        };
        let approved_by_name = match &row.approved_by {
            Some(admin_id) => volunteer_name(&state.db, &mut volunteers_seen, admin_id).await?,
            None => None,
        };
        views.push(DeliveryView {
            id: row.id,
            item: row.item,
            scope: row.scope,
            quantity: row.quantity,
            status: row.status,
            verification: row.verification,
            reasons,
            delivered_at: row.delivered_at.to_string(),
            person_name,
            card_code: row.card_code,
            delivered_by_name: volunteer_name(&state.db, &mut volunteers_seen, &row.delivered_by)
                .await?,
            approved_by_name,
            review_note: row.review_note,
        });
    }
    Ok(Json(json!({ "deliveries": views, "has_more": has_more })))
}

// ---- POST /v1/deliveries/{id}/review ----

#[derive(Debug, Deserialize)]
pub struct ReviewBody {
    decision: ReviewDecision,
    #[serde(default)]
    note: String,
}

/// `POST /v1/deliveries/{id}/review`: `{decision: reviewed_ok | irregular, note}`, by an admin of the delivery's shelter.
pub async fn review_delivery(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(id): Path<String>,
    Json(body): Json<ReviewBody>,
) -> Result<Json<Value>, ApiError> {
    let note = body.note.trim();
    if note.chars().count() > MAX_NOTE_CHARS {
        return Err(invalid("note"));
    }
    let note = (!note.is_empty()).then(|| note.to_string());
    let decision = body.decision;

    let delivery: DeliveryDoc = store::get(&state.db, COLLECTION, &id)
        .await?
        .ok_or_else(delivery_not_found)?;
    if !volunteer.is_admin_of(&delivery.shelter_id) {
        return Err(forbidden());
    }
    events::ensure_shelter_open(&state.db, &delivery.shelter_id).await?;

    let reviewed = store::transact(&state.db, |db, tx| {
        let (id, note, volunteer) = (id.clone(), note.clone(), volunteer.clone());
        Box::pin(async move {
            let mut delivery: DeliveryDoc = store::get(&db, COLLECTION, &id)
                .await?
                .ok_or_else(delivery_not_found)?;
            delivery.review(decision, &volunteer.id, note)?;
            tx_replace(&db, tx, COLLECTION, &id, &delivery)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new(super::ACTION_REVIEW, "delivery", &id)
                    .by(&volunteer.id)
                    .in_shelter(&delivery.shelter_id)
                    .basis(LegalBasis::FraudPrevention),
            )?;
            Ok(delivery)
        })
    })
    .await?;
    events::touch(&state, reviewed.event_id.as_deref()).await;
    Ok(Json(
        json!({ "delivery_id": id, "status": reviewed.status }),
    ))
}
