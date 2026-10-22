//! The routes (spec 3.4, 4.1, 4.11, 4.12, 8.5): they parse and authorize the request, call
//! `persist` for the transaction and answer with the views.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use firestore::FirestoreInstant;
use serde::Deserialize;
use serde_json::{Value, json};

use super::domain::{Checkin, PersonData};
use super::model::{Level, PersonDoc, forbidden, invalid};
use super::persist::{self, LIST_LIMIT, Migration};
use super::view::{history_for, list_view, person_view, viewer_shelter};
use crate::audit::{AuditEntry, LegalBasis};
use crate::auth::{self, AppState, Purpose, Volunteer};
use crate::children;
use crate::error::ApiError;
use crate::idempotency;
use crate::store;
use crate::thumbnail;

// ---- request plumbing ----

fn body<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ApiError> {
    serde_json::from_value(value).map_err(|err| {
        let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
        error.detail = json!({ "message": err.to_string() });
        error
    })
}

fn require_member(volunteer: &Volunteer, shelter_id: &str) -> Result<(), ApiError> {
    volunteer
        .role_in(shelter_id)
        .map(|_| ())
        .ok_or_else(forbidden)
}

/// `X-Taken-Over-From`: the original author of a queue item an admin took over (spec 9.2).
fn taken_over_from(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-taken-over-from")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.len() <= 64)
        .map(str::to_string)
}

fn photo_bytes(base64: &str) -> Result<Vec<u8>, ApiError> {
    STANDARD
        .decode(base64.trim())
        .map_err(|_| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_image"))
}

/// A capture made for another shelter or another person.
fn wrong_capture_target() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "capture_wrong_target")
}

// ---- POST /v1/shelters/{id}/checkins ----

#[derive(Debug, Deserialize)]
struct CheckinRequest {
    id: String,
    capture_id: Option<String>,
    photo_base64: Option<String>,
    #[serde(default)]
    no_biometrics: bool,
    #[serde(default)]
    biometrics_declined: bool,
    #[serde(default)]
    data: Value,
    thumbnail_base64: Option<String>,
    legal_basis: LegalBasis,
    client_created_at: String,
}

pub async fn post_checkin(
    State(state): State<AppState>,
    volunteer: Volunteer,
    headers: HeaderMap,
    Path(shelter_id): Path<String>,
    Json(request): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let request: CheckinRequest = body(request)?;
    require_member(&volunteer, &shelter_id)?;
    let shelter = persist::load_shelter(&state.db, &shelter_id).await?;
    crate::events::ensure_open(&state.db, shelter.event_id.as_deref()).await?;
    let data = PersonData::parse(request.data)?;
    let entered_at: FirestoreInstant = request
        .client_created_at
        .parse()
        .map_err(|_| invalid("client_created_at"))?;
    let thumbnail = match (&request.thumbnail_base64, data.identifies()) {
        (Some(upload), false) => {
            let jpeg = thumbnail::decode_upload(upload)?;
            Some(thumbnail::encrypt(state.thumbnail_key(), &jpeg)?)
        }
        _ => None,
    };

    let device = auth::device_id(&headers);
    let (person_id, level, capture_id) = match (
        &request.capture_id,
        &request.photo_base64,
        request.no_biometrics,
    ) {
        (Some(capture_id), None, false) => {
            let device = device.clone()?;
            let mut capture =
                auth::usable_capture(&state, &device, capture_id, Purpose::Checkin).await?;
            // The capture was asked for one shelter and cannot check anyone in elsewhere.
            // Nor can another volunteer, even on the same phone, use someone else's capture.
            if capture.shelter_id.as_deref() != Some(shelter_id.as_str())
                || capture.volunteer_id.as_deref() != Some(volunteer.id.as_str())
            {
                return Err(wrong_capture_target());
            }
            let person_id = auth::resolve_person(&state, capture_id, &mut capture).await?;
            (person_id, Level::Verified, Some(capture_id.clone()))
        }
        (None, Some(photo), false) => {
            let route = format!("POST /v1/shelters/{shelter_id}/checkins");
            let id = idempotency::photo_person_id(&state, &headers, &volunteer.id, &route, async {
                let token = state.unico.token_from_image(&photo_bytes(photo)?).await?;
                Ok(state.person_id_for_token(&token))
            })
            .await?;
            (id, Level::Assisted, None)
        }
        (None, None, true) => {
            uuid::Uuid::parse_str(&request.id).map_err(|_| invalid("id"))?;
            (request.id.clone(), Level::NoBiometrics, None)
        }
        _ => return Err(invalid("biometrics")),
    };
    let checkin = Checkin {
        person_id: person_id.clone(),
        level,
        declined: request.biometrics_declined,
        data,
        thumbnail,
        capture_id,
        shelter_id: shelter_id.clone(),
        event_id: shelter.event_id.clone(),
        shelter: shelter.clone(),
        legal_basis: request.legal_basis,
        entered_at,
        volunteer_id: volunteer.id.clone(),
        may_alter_child: children::can_change_status(&volunteer),
        device: device.ok(),
        taken_over_from: taken_over_from(&headers),
    };
    let (person, is_new) = store::transact(&state.db, |db, tx| {
        let checkin = checkin.clone();
        Box::pin(async move { persist::checkin(&db, tx, &checkin).await })
    })
    .await?;
    crate::events::touch(&state, shelter.event_id.as_deref()).await;
    crate::audit::note_activity(
        &state,
        &volunteer.id,
        crate::audit::Activity::Checkin,
        &person_id,
        Some((shelter.lat, shelter.lon)),
        shelter.event_id.as_deref(),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "person_id": person_id,
            "level": person.level,
            "is_new": is_new,
            "history": history_for(&person, &volunteer, &shelter_id),
        })),
    ))
}

// ---- GET /v1/shelters/{id}/people[/needs-selfie] ----

pub async fn list_people(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_member(&volunteer, &shelter_id)?;
    let (people, has_more) = persist::people_of(&state.db, &shelter_id, false, LIST_LIMIT).await?;
    Ok(Json(list_view(&people, &volunteer, &shelter_id, has_more)?))
}

/// `assisted` and `no_biometrics` who did not refuse the selfie (spec 8.5).
pub async fn needs_selfie(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_member(&volunteer, &shelter_id)?;
    let (people, has_more) = persist::people_of(&state.db, &shelter_id, true, LIST_LIMIT).await?;
    Ok(Json(list_view(&people, &volunteer, &shelter_id, has_more)?))
}

// ---- GET, PATCH /v1/people/{id} ----

pub async fn get_person(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(person_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (person, shelter_id) =
        persist::visible_person(&state, &volunteer, &person_id, true).await?;
    let history = history_for(&person, &volunteer, &shelter_id);
    Ok(Json(
        json!({ "person": person_view(&person_id, &person, history) }),
    ))
}

#[derive(Debug, Deserialize)]
struct PatchRequest {
    #[serde(default)]
    data: Value,
    client_edited_at: String,
}

pub async fn patch_person(
    State(state): State<AppState>,
    volunteer: Volunteer,
    headers: HeaderMap,
    Path(person_id): Path<String>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let request: PatchRequest = body(request)?;
    let data = PersonData::parse(request.data)?;
    let edited_at: FirestoreInstant = request
        .client_edited_at
        .parse()
        .map_err(|_| invalid("client_edited_at"))?;
    let (current, shelter_id) =
        persist::visible_person(&state, &volunteer, &person_id, true).await?;
    if !shelter_id.is_empty() {
        crate::events::ensure_shelter_open(&state.db, &shelter_id).await?;
    }
    // Someone who sees the child only through the alert changes the status and nothing else.
    if viewer_shelter(&current, &volunteer).is_none() && !data.only_child_status() {
        return Err(forbidden());
    }
    let entry = AuditEntry::new(super::ACTION_UPDATE, "person", &person_id)
        .by(&volunteer.id)
        .taken_over_from(taken_over_from(&headers).as_deref())
        .basis(current.legal_basis);
    let may_alter_child = children::can_change_status(&volunteer);
    let person = store::transact(&state.db, |db, tx| {
        let (person_id, data, entry) = (person_id.clone(), data.clone(), entry.clone());
        Box::pin(async move {
            persist::patch(
                &db,
                tx,
                &person_id,
                &data,
                edited_at,
                may_alter_child,
                &entry,
            )
            .await
        })
    })
    .await?;
    if !shelter_id.is_empty() {
        crate::members::touch_shelter_event(&state, &shelter_id).await;
    }
    let history = history_for(&person, &volunteer, &shelter_id);
    Ok(Json(
        json!({ "person": person_view(&person_id, &person, history) }),
    ))
}

// ---- PUT /v1/people/{id}/biometrics (spec 4.12) ----

#[derive(Debug, Deserialize)]
struct BiometricsRequest {
    capture_id: Option<String>,
    photo_base64: Option<String>,
}

pub async fn put_biometrics(
    State(state): State<AppState>,
    volunteer: Volunteer,
    headers: HeaderMap,
    Path(person_id): Path<String>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let request: BiometricsRequest = body(request)?;
    let (person, shelter_id) =
        persist::visible_person(&state, &volunteer, &person_id, false).await?;
    if !shelter_id.is_empty() {
        crate::events::ensure_shelter_open(&state.db, &shelter_id).await?;
    }
    let device = auth::device_id(&headers);
    let (new_id, level, capture_id) = match (&request.capture_id, &request.photo_base64) {
        (Some(capture_id), None) => {
            let device = device.clone()?;
            let mut capture =
                auth::usable_capture(&state, &device, capture_id, Purpose::Biometrics).await?;
            // ... and a biometrics capture was asked for one person.
            if capture.expected_person_id.as_deref() != Some(person_id.as_str())
                || capture.volunteer_id.as_deref() != Some(volunteer.id.as_str())
            {
                return Err(wrong_capture_target());
            }
            let new_id = auth::resolve_person(&state, capture_id, &mut capture).await?;
            (new_id, Level::Verified, Some(capture_id.clone()))
        }
        (None, Some(photo)) => {
            let route = format!("PUT /v1/people/{person_id}/biometrics");
            let id = idempotency::photo_person_id(&state, &headers, &volunteer.id, &route, async {
                let token = state.unico.token_from_image(&photo_bytes(photo)?).await?;
                Ok(state.person_id_for_token(&token))
            })
            .await?;
            (id, Level::Assisted, None)
        }
        _ => return Err(invalid("biometrics")),
    };
    // A record that already has a token can only be raised by its own token.
    if person.level != Level::NoBiometrics && new_id != person_id {
        return Err(ApiError::new(StatusCode::CONFLICT, "person_mismatch"));
    }
    let migration_shelter = (!shelter_id.is_empty()).then_some(shelter_id);
    let migration = Migration {
        old_id: person_id,
        new_id: new_id.clone(),
        level,
        capture_id,
        volunteer_id: volunteer.id.clone(),
        device: device.ok(),
        shelter_id: migration_shelter.clone(),
        taken_over_from: taken_over_from(&headers),
    };
    let person: PersonDoc = store::transact(&state.db, |db, tx| {
        let m = migration.clone();
        Box::pin(async move {
            if m.old_id == m.new_id {
                persist::raise(&db, tx, &m).await
            } else {
                persist::migrate(&db, tx, &m).await
            }
        })
    })
    .await?;
    if let Some(shelter_id) = &migration_shelter {
        crate::members::touch_shelter_event(&state, shelter_id).await;
    }
    Ok(Json(json!({ "person_id": new_id, "level": person.level })))
}
