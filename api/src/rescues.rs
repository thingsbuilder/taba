//! Rescue registration (spec 3.6, 7, 8.7, 8.2): `POST /v1/rescues`, `PATCH /v1/rescues/{id}` and
//! `GET /v1/shelters/nearby`.

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use firestore::{FirestoreDb, FirestoreInstant, FirestoreTransaction};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{self, AppState, Volunteer};
use crate::error::ApiError;
use crate::events::{self, EventDoc, EventStatus};
use crate::exits;
use crate::geo;
use crate::idempotency;
use crate::people::{self, Level, Origin, PersonDoc};
use crate::shelters::{self, ShelterDoc};
use crate::store::{self, tx_create, tx_replace};
use crate::thumbnail;
use crate::volunteers::GlobalRole;

pub const COLLECTION: &str = "rescues";
/// The route a photo of a rescue is paid under (see `idempotency::photo_person_id`).
const ROUTE: &str = "POST /v1/rescues";
const MAX_TEXT_CHARS: usize = 100;
const MAX_NOTE_CHARS: usize = 500;
/// What Unico says about a photo that cannot name a person; the rescue is then registered
/// without biometrics and the photo waits for the late biometrics (spec 3.6 item 6, 4.12).
const NO_TOKEN_FOR_PHOTO: [&str; 5] = [
    "no_face",
    "capture_inconclusive",
    "photo_rejected",
    "unico_degraded",
    "unico_limit",
];

/// `rescues/{rescue_id}` (UUID of the client), spec 7.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RescueDoc {
    pub person_id: String,
    pub volunteer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landing_point: Option<String>,
    /// The phone's clock.
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub captured_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub received_at: FirestoreInstant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_shelter_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Called by `people::migrate_in` inside its transaction (spec 4.12). Nothing is left to do here:
/// `people::repoint_person_refs` already points every `rescues.person_id` at the new id, and the
/// merge of the two people carries `rescue_ids` over.
pub async fn repoint_person(
    _db: &FirestoreDb,
    _tx: &mut FirestoreTransaction<'_>,
    _old: &str,
    _new: &str,
) -> Result<(), ApiError> {
    Ok(())
}

fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
    error.detail = json!({ "field": field });
    error
}

fn shelter_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found")
}

fn rescue_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "rescue_not_found")
}

/// Trimmed; blank counts as absent.
fn text(value: Option<String>, field: &str, max: usize) -> Result<Option<String>, ApiError> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim();
    if value.chars().count() > max {
        return Err(invalid(field));
    }
    Ok((!value.is_empty()).then(|| value.to_string()))
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ApiError> {
    serde_json::from_value(value).map_err(|err| {
        let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
        error.detail = json!({ "message": err.to_string() });
        error
    })
}

fn valid_point(lat: f64, lon: f64) -> Result<(), ApiError> {
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(invalid("position"));
    }
    Ok(())
}

/// Registering a rescue: a volunteer or admin of a shelter, or the Civil Defense (spec 2.3).
fn may_register(volunteer: &Volunteer) -> bool {
    !volunteer.memberships.is_empty() || volunteer.has_global(GlobalRole::CivilDefense)
}

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

/// The request after validation, with what the transaction needs.
#[derive(Clone)]
struct Registration {
    rescue_id: String,
    person_id: String,
    level: Level,
    volunteer_id: String,
    device: Option<String>,
    lat: f64,
    lon: f64,
    accuracy_m: f64,
    captured_at: FirestoreInstant,
    name: Option<String>,
    wristband: Option<String>,
    note: Option<String>,
    landing_point: Option<String>,
    destination_shelter_id: Option<String>,
    event_id: Option<String>,
    /// Encrypted JPEG; only when the rescued person has neither name nor wristband (spec 4.6).
    thumbnail: Option<Vec<u8>>,
}

/// The person of a photo: the HMAC of its token, or, when Unico cannot name one (no usable
/// face, degraded mode) or there is no photo, the client's UUID as a `no_biometrics` person.
async fn identify(
    state: &AppState,
    headers: &HeaderMap,
    volunteer_id: &str,
    rescue_id: &str,
    photo: Option<&str>,
) -> Result<(String, Level), ApiError> {
    let Some(photo) = photo else {
        return Ok((rescue_id.to_string(), Level::NoBiometrics));
    };
    let image = STANDARD
        .decode(photo.trim())
        .map_err(|_| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_image"))?;
    let asked = idempotency::photo_person_id(state, headers, volunteer_id, ROUTE, async {
        let token = state.unico.token_from_image(&image).await?;
        Ok(state.person_id_for_token(&token))
    })
    .await;
    match asked {
        Ok(person_id) => Ok((person_id, Level::Assisted)),
        Err(err) if NO_TOKEN_FOR_PHOTO.contains(&err.code) => {
            Ok((rescue_id.to_string(), Level::NoBiometrics))
        }
        Err(err) => Err(err),
    }
}

/// The event of the nearest open shelter of an active event within `EVENT_RADIUS_KM` (spec 3.6 item 6).
async fn event_near(state: &AppState, lat: f64, lon: f64) -> Result<Option<String>, ApiError> {
    let radius_m = f64::from(state.config.event_radius_km) * 1000.0;
    for near in geo::open_shelters_within(&state.db, lat, lon, radius_m).await? {
        let Some(event_id) = near.shelter.event_id else {
            continue;
        };
        let event: Option<EventDoc> = store::get(&state.db, events::COLLECTION, &event_id).await?;
        if event.is_some_and(|e| e.status == EventStatus::Active) {
            return Ok(Some(event_id));
        }
    }
    Ok(None)
}

async fn require_shelter(db: &FirestoreDb, id: &str) -> Result<(), ApiError> {
    store::get::<ShelterDoc>(db, shelters::COLLECTION, id)
        .await?
        .map(|_| ())
        .ok_or_else(shelter_not_found)
}

fn fresh_person(id: &str, level: Level, now: FirestoreInstant) -> PersonDoc {
    PersonDoc {
        id: Some(id.into()),
        level,
        biometrics_declined: false,
        former_ids: vec![],
        name: None,
        social_name: None,
        wristband: None,
        birth_year: None,
        is_child: false,
        child_status: None,
        guardians: vec![],
        guardian_ids: vec![],
        needs: Default::default(),
        vulnerabilities: Default::default(),
        household_id: None,
        current_shelter_id: None,
        stays: vec![],
        event_ids: vec![],
        rescue_ids: vec![],
        thumbnail_encrypted: None,
        origin: Origin::Rescue,
        legal_basis: LegalBasis::VitalInterest,
        created_at: now,
        updated_at: now,
        client_edited_at: None,
    }
}

/// The answer of a rescue already registered: the person it points at now.
async fn registered(
    db: &FirestoreDb,
    rescue: &RescueDoc,
    volunteer_id: &str,
) -> Result<(String, Level), ApiError> {
    if rescue.volunteer_id != volunteer_id {
        return Err(forbidden());
    }
    let level = store::get::<PersonDoc>(db, people::COLLECTION, &rescue.person_id)
        .await?
        .map_or(Level::NoBiometrics, |p| p.level);
    Ok((rescue.person_id.clone(), level))
}

/// One transaction: the rescue is created (an id already used by the same volunteer answers
/// again with the first result), and the person is created or gets the rescue in their history.
async fn register_in(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    r: &Registration,
) -> Result<(String, Level), ApiError> {
    if let Some(rescue) = store::get::<RescueDoc>(db, COLLECTION, &r.rescue_id).await? {
        return registered(db, &rescue, &r.volunteer_id).await;
    }
    events::ensure_open(db, r.event_id.as_deref()).await?;
    let existing: Option<PersonDoc> = store::get(db, people::COLLECTION, &r.person_id).await?;
    let now = store::now();
    let mut person = existing.unwrap_or_else(|| fresh_person(&r.person_id, r.level, now));
    person.level = person.level.raised_to(r.level);
    if r.name.is_some() {
        person.name = r.name.clone();
    }
    if r.wristband.is_some() {
        person.wristband = r.wristband.clone();
    }
    if let Some(sealed) = &r.thumbnail {
        person.thumbnail_encrypted = Some(sealed.clone());
    }
    people::drop_thumbnail_if_identified(&mut person);
    if let Some(event_id) = &r.event_id {
        if !person.event_ids.contains(event_id) {
            person.event_ids.push(event_id.clone());
        }
    }
    person.rescue_ids.push(r.rescue_id.clone());
    person.updated_at = now;
    let rescue = RescueDoc {
        person_id: r.person_id.clone(),
        volunteer_id: r.volunteer_id.clone(),
        device_id: r.device.clone(),
        lat: r.lat,
        lon: r.lon,
        accuracy_m: r.accuracy_m,
        landing_point: r.landing_point.clone(),
        captured_at: r.captured_at,
        received_at: now,
        event_id: r.event_id.clone(),
        destination_shelter_id: r.destination_shelter_id.clone(),
        note: r.note.clone(),
    };
    tx_create(db, tx, COLLECTION, &r.rescue_id, &rescue)?;
    tx_replace(db, tx, people::COLLECTION, &r.person_id, &person)?;
    // A person without biometrics has no token id and matches nothing (spec 3.6 item 8).
    if person.level != Level::NoBiometrics {
        let shelter: Option<ShelterDoc> = match &person.current_shelter_id {
            Some(id) => store::get(db, shelters::COLLECTION, id).await?,
            None => None,
        };
        crate::matching::on_person_registered(db, tx, &person, &r.person_id, shelter.as_ref())
            .await?;
    }
    let mut entry = AuditEntry::new("rescue.create", "rescue", &r.rescue_id)
        .by(&r.volunteer_id)
        .basis(LegalBasis::VitalInterest);
    if let Some(device) = &r.device {
        entry = entry.on_device(device);
    }
    audit::add(db, tx, &entry)?;
    Ok((r.person_id.clone(), person.level))
}

fn created(rescue_id: &str, person_id: &str, level: Level) -> (StatusCode, Json<Value>) {
    (
        StatusCode::CREATED,
        Json(json!({ "rescue_id": rescue_id, "person_id": person_id, "level": level })),
    )
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
    let entry = AuditEntry::new("rescue.update", "rescue", &rescue_id)
        .by(&volunteer.id)
        .basis(LegalBasis::VitalInterest);
    store::transact(&state.db, |db, tx| {
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
            audit::add(&db, tx, &entry)
        })
    })
    .await?;
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

// ---- who may see a rescued person (spec 4.6, 4.12) ----

/// Whether `volunteer` registered a rescue of `person`.
// Called by `people::visible_person` once the orchestrator wires it (see the hand-off notes).
#[allow(dead_code)]
pub async fn registered_rescue_of(
    db: &FirestoreDb,
    volunteer: &Volunteer,
    person: &PersonDoc,
) -> Result<bool, ApiError> {
    for rescue_id in &person.rescue_ids {
        let rescue: Option<RescueDoc> = store::get(db, COLLECTION, rescue_id).await?;
        if rescue.is_some_and(|r| r.volunteer_id == volunteer.id) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The thumbnail of a rescued person who has no shelter yet (spec 4.6, Q33): the volunteer who
/// registered the rescue and the Civil Defense. Once the person has a stay, the shelter rules apply.
// Called by `thumbnail::get_thumbnail` once the orchestrator wires it (see the hand-off notes).
#[allow(dead_code)]
pub async fn can_see_thumbnail(
    db: &FirestoreDb,
    volunteer: &Volunteer,
    person: &PersonDoc,
) -> Result<bool, ApiError> {
    if !person.stays.is_empty() || person.rescue_ids.is_empty() {
        return Ok(false);
    }
    if volunteer.has_global(GlobalRole::CivilDefense) {
        return Ok(true);
    }
    registered_rescue_of(db, volunteer, person).await
}
