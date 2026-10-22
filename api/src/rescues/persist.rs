//! What a rescue reads and writes: who the photo names, the event and the shelter it points
//! at, the transaction that registers it and the visibility of a rescued person.

use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use firestore::{FirestoreDb, FirestoreTransaction};

use super::domain::{forbidden, fresh_person, shelter_not_found, unsheltered};
use super::model::{COLLECTION, Registration, RescueDoc};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::events::{self, EventDoc, EventStatus};
use crate::geo;
use crate::idempotency;
use crate::people::{self, Level, PersonDoc};
use crate::shelters::{self, ShelterDoc};
use crate::store::{self, tx_create, tx_replace};
use crate::volunteers::GlobalRole;

/// The route a photo of a rescue is paid under (see `idempotency::photo_person_id`).
pub(super) const ROUTE: &str = "POST /v1/rescues";
/// What Unico says about a photo that cannot name a person; the rescue is then registered
/// without biometrics and the photo waits for the late biometrics (spec 3.6 item 6, 4.12).
/// A photo over 800 KB is not in the list: it is `413 image_too_large` before Unico is asked, because
/// the app should have reduced it, and nothing is written. A photo that is not base64 is a
/// malformed request (`422 invalid_image`); one that Unico cannot read is a rescue without biometrics.
const NO_TOKEN_FOR_PHOTO: [&str; 6] = [
    "invalid_image",
    "no_face",
    "capture_inconclusive",
    "photo_rejected",
    "unico_degraded",
    "unico_limit",
];

/// The person of a photo: the HMAC of its token, or, when Unico cannot name one (no usable
/// face, degraded mode) or there is no photo, the client's UUID as a `no_biometrics` person.
pub(super) async fn identify(
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
pub(super) async fn event_near(
    state: &AppState,
    lat: f64,
    lon: f64,
) -> Result<Option<String>, ApiError> {
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

pub(super) async fn require_shelter(db: &FirestoreDb, id: &str) -> Result<(), ApiError> {
    store::get::<ShelterDoc>(db, shelters::COLLECTION, id)
        .await?
        .map(|_| ())
        .ok_or_else(shelter_not_found)
}

/// The answer of a rescue already registered: the person it points at now.
pub(super) async fn registered(
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
pub(super) async fn register_in(
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
    // A person already registered keeps the name and the wristband they have: a rescue only fills
    // what is empty, so a second landing point cannot overwrite what a shelter wrote.
    if person.name.is_none() {
        person.name = r.name.clone();
    }
    if person.wristband.is_none() {
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
    let mut entry = AuditEntry::new(super::ACTION_CREATE, "rescue", &r.rescue_id)
        .by(&r.volunteer_id)
        .basis(LegalBasis::VitalInterest);
    if let Some(device) = &r.device {
        entry = entry.on_device(device);
    }
    audit::add(db, tx, &entry)?;
    Ok((r.person_id.clone(), person.level))
}

/// Whether `volunteer` registered a rescue of `person`, who is still in no shelter.
pub async fn registered_rescue_of(
    db: &FirestoreDb,
    volunteer: &Volunteer,
    person: &PersonDoc,
) -> Result<bool, ApiError> {
    if !unsheltered(person) {
        return Ok(false);
    }
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
pub async fn can_see_thumbnail(
    db: &FirestoreDb,
    volunteer: &Volunteer,
    person: &PersonDoc,
) -> Result<bool, ApiError> {
    if !unsheltered(person) || person.rescue_ids.is_empty() {
        return Ok(false);
    }
    if volunteer.has_global(GlobalRole::CivilDefense) {
        return Ok(true);
    }
    registered_rescue_of(db, volunteer, person).await
}
