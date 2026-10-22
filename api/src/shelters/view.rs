//! The JSON shown of a shelter: the full view for members, validators and the Civil Defense, the
//! public one for everybody else (spec 8.2).

use firestore::FirestoreDb;
use serde_json::{Value, json};

use super::model::ShelterDoc;
use super::persist::event_of;
use crate::auth::Volunteer;
use crate::error::ApiError;
use crate::events::EventDoc;
use crate::exits;
use crate::members::ShelterRole;

/// The shelter as members, validators and the Civil Defense see it.
pub fn full_view(
    id: &str,
    shelter: &ShelterDoc,
    event: Option<(&str, &EventDoc)>,
    role: Option<ShelterRole>,
    occupancy: u32,
) -> Value {
    json!({
        "id": id,
        "name": shelter.name,
        "lat": shelter.lat,
        "lon": shelter.lon,
        "accuracy_m": shelter.accuracy_m,
        "address": shelter.address,
        "event_id": shelter.event_id,
        "event": event.map(|(event_id, event)| event.view(event_id)),
        "capacity": shelter.capacity,
        "occupancy": occupancy,
        "vacancies": exits::vacancies(shelter.capacity, occupancy),
        "status": shelter.status,
        "confirmation": shelter.confirmation,
        "confirmation_type": shelter.confirmation_type,
        "can_endorse": shelter.can_endorse,
        "flagged": shelter.flagged,
        "created_at": shelter.created_at.to_string(),
        "role": role,
        "items": shelter.items,
    })
}

/// What any other volunteer sees: name, confirmation and vacancies (spec 8.2).
fn public_view(id: &str, shelter: &ShelterDoc, occupancy: u32) -> Value {
    json!({
        "id": id,
        "name": shelter.name,
        "confirmation": shelter.confirmation,
        "capacity": shelter.capacity,
        "occupancy": occupancy,
        "vacancies": exits::vacancies(shelter.capacity, occupancy),
    })
}

/// The view that fits the caller (spec 8.2): full for members, validators and Civil Defense.
pub async fn view_for(
    db: &FirestoreDb,
    volunteer: &Volunteer,
    id: &str,
    shelter: &ShelterDoc,
) -> Result<Value, ApiError> {
    let role = volunteer.role_in(id);
    let occupancy = exits::occupancy(db, id).await?;
    if role.is_none() && !volunteer.is_validator_or_civil_defense() {
        return Ok(public_view(id, shelter, occupancy));
    }
    let event = event_of(db, shelter).await?;
    Ok(full_view(
        id,
        shelter,
        event
            .as_ref()
            .map(|(event_id, event)| (event_id.as_str(), event)),
        role,
        occupancy,
    ))
}

/// The full view for a caller who is allowed to change the shelter.
pub(super) async fn respond(
    db: &FirestoreDb,
    id: &str,
    shelter: &ShelterDoc,
    event: Option<&(String, EventDoc)>,
    volunteer: &Volunteer,
) -> Result<Value, ApiError> {
    Ok(full_view(
        id,
        shelter,
        event.map(|(event_id, event)| (event_id.as_str(), event)),
        volunteer.role_in(id),
        exits::occupancy(db, id).await?,
    ))
}
