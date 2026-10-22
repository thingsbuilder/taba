//! The routes of rescues: `POST /v1/rescues`, `PATCH /v1/rescues/{id}` and
//! `GET /v1/shelters/nearby`.

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use firestore::FirestoreInstant;
use serde::Deserialize;
use serde_json::{Value, json};

use super::domain::{forbidden, invalid, may_register, parse, rescue_not_found, text, valid_point};
use super::model::{COLLECTION, Registration, RescueDoc};
use super::persist::{event_near, identify, register_in, registered, require_shelter};
use super::view::created;
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{self, AppState, Volunteer};
use crate::error::ApiError;
use crate::events;
use crate::exits;
use crate::geo;
use crate::store::{self, tx_replace};
use crate::thumbnail;

const MAX_TEXT_CHARS: usize = 100;
const MAX_NOTE_CHARS: usize = 500;

// ---- POST /v1/rescues ----

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RescueData {
    name: Option<String>,
    wristband: Option<String>,
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RescueRequest {
    id: String,
    photo_base64: Option<String>,
    lat: f64,
    lon: f64,
    accuracy: f64,
    captured_at: String,
    #[serde(default)]
    data: Option<RescueData>,
    landing_point: Option<String>,
    destination_shelter_id: Option<String>,
    thumbnail_base64: Option<String>,
}

/// `POST /v1/rescues` (spec 8.7), through the queue.
pub async fn post_rescue(
    State(state): State<AppState>,
    volunteer: Volunteer,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if !may_register(&volunteer) {
        return Err(forbidden());
    }
    let request: RescueRequest = parse(request)?;
    uuid::Uuid::parse_str(&request.id).map_err(|_| invalid("id"))?;
    valid_point(request.lat, request.lon)?;
    if !request.accuracy.is_finite() || request.accuracy < 0.0 {
        return Err(invalid("accuracy"));
    }
    let captured_at: FirestoreInstant = request
        .captured_at
        .parse()
        .map_err(|_| invalid("captured_at"))?;
    let data = request.data.unwrap_or_default();
    let (name, wristband) = (
        text(data.name, "name", MAX_TEXT_CHARS)?,
        text(data.wristband, "wristband", MAX_TEXT_CHARS)?,
    );
    let note = text(data.note, "note", MAX_NOTE_CHARS)?;
    let landing_point = text(request.landing_point, "landing_point", MAX_TEXT_CHARS)?;
    let thumbnail = match (
        &request.thumbnail_base64,
        name.is_some() || wristband.is_some(),
    ) {
        (Some(upload), false) => {
            let jpeg = thumbnail::decode_upload(upload)?;
            Some(thumbnail::encrypt(state.thumbnail_key(), &jpeg)?)
        }
        _ => None,
    };
    if let Some(shelter_id) = &request.destination_shelter_id {
        require_shelter(&state.db, shelter_id).await?;
    }

    // A rescue sent again does not pay for its photo again.
    if let Some(rescue) = store::get::<RescueDoc>(&state.db, COLLECTION, &request.id).await? {
        let (person_id, level) = registered(&state.db, &rescue, &volunteer.id).await?;
        return Ok(created(&request.id, &person_id, level));
    }
    let (person_id, level) = identify(
        &state,
        &headers,
        &volunteer.id,
        &request.id,
        request.photo_base64.as_deref(),
    )
    .await?;
    let registration = Registration {
        rescue_id: request.id.clone(),
        person_id,
        level,
        volunteer_id: volunteer.id.clone(),
        device: auth::device_id(&headers).ok(),
        lat: request.lat,
        lon: request.lon,
        accuracy_m: request.accuracy,
        captured_at,
        name,
        wristband,
        note,
        landing_point,
        destination_shelter_id: request.destination_shelter_id,
        event_id: event_near(&state, request.lat, request.lon).await?,
        thumbnail,
    };
    let (person_id, level) = store::transact(&state.db, |db, tx| {
        let registration = registration.clone();
        Box::pin(async move { register_in(&db, tx, &registration).await })
    })
    .await?;
    events::touch(&state, registration.event_id.as_deref()).await;
    crate::audit::note_activity(
        &state,
        &volunteer.id,
        crate::audit::Activity::Rescue,
        &person_id,
        Some((registration.lat, registration.lon)),
        registration.event_id.as_deref(),
    )
    .await;
    Ok(created(&request.id, &person_id, level))
}

// ---- PATCH /v1/rescues/{id} ----

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RescuePatch {
    landing_point: Option<String>,
    destination_shelter_id: Option<String>,
}

/// `PATCH /v1/rescues/{id}`: `landing_point` and `destination_shelter_id`, by the author.
pub async fn patch_rescue(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(rescue_id): Path<String>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let patch: RescuePatch = parse(request)?;
    let landing_point = text(patch.landing_point, "landing_point", MAX_TEXT_CHARS)?;
    if let Some(shelter_id) = &patch.destination_shelter_id {
        require_shelter(&state.db, shelter_id).await?;
    }
    let entry = AuditEntry::new(super::ACTION_UPDATE, "rescue", &rescue_id)
        .by(&volunteer.id)
        .basis(LegalBasis::VitalInterest);
    let event_id = store::transact(&state.db, |db, tx| {
        let (rescue_id, landing_point, destination, entry) = (
            rescue_id.clone(),
            landing_point.clone(),
            patch.destination_shelter_id.clone(),
            entry.clone(),
        );
        let volunteer_id = volunteer.id.clone();
        Box::pin(async move {
            let mut rescue: RescueDoc = store::get(&db, COLLECTION, &rescue_id)
                .await?
                .ok_or_else(rescue_not_found)?;
            if rescue.volunteer_id != volunteer_id {
                return Err(forbidden());
            }
            events::ensure_open(&db, rescue.event_id.as_deref()).await?;
            if landing_point.is_some() {
                rescue.landing_point = landing_point;
            }
            if destination.is_some() {
                rescue.destination_shelter_id = destination;
            }
            tx_replace(&db, tx, COLLECTION, &rescue_id, &rescue)?;
            audit::add(&db, tx, &entry)?;
            Ok(rescue.event_id.clone())
        })
    })
    .await?;
    events::touch(&state, event_id.as_deref()).await;
    Ok(Json(json!({})))
}

// ---- GET /v1/shelters/nearby ----

#[derive(Debug, Deserialize)]
pub struct NearbyQuery {
    lat: f64,
    lon: f64,
}

/// `GET /v1/shelters/nearby?lat&lon`: open shelters within `EVENT_RADIUS_KM` of a point with
/// capacity and occupancy, nearest first (spec 8.2). Any volunteer may ask.
pub async fn nearby_shelters(
    State(state): State<AppState>,
    _volunteer: Volunteer,
    query: Result<Query<NearbyQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(point) = query.map_err(|_| invalid("position"))?;
    valid_point(point.lat, point.lon)?;
    let radius_m = f64::from(state.config.event_radius_km) * 1000.0;
    let mut list = Vec::new();
    for near in geo::open_shelters_within(&state.db, point.lat, point.lon, radius_m).await? {
        let Some(id) = near.shelter.id.as_deref() else {
            continue;
        };
        // The shelter of a closed event takes no one (spec 4.3 item 5).
        match events::ensure_open(&state.db, near.shelter.event_id.as_deref()).await {
            Err(err) if err.code == "event_closed" => continue,
            other => other?,
        }
        let occupancy = exits::occupancy(&state.db, id).await?;
        list.push(json!({
            "id": id,
            "name": near.shelter.name,
            "distance_m": near.distance_m.round(),
            "capacity": near.shelter.capacity,
            "occupancy": occupancy,
            "vacancies": exits::vacancies(near.shelter.capacity, occupancy),
            "confirmation": near.shelter.confirmation,
        }));
    }
    Ok(Json(json!({ "shelters": list })))
}
