//! The route `PATCH /v1/events/{id}` (spec 8.2): who may rename the event and who may set the
//! reference contacts, and the transaction that writes it.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::domain::{EventPatch, apply};
use super::model::{COLLECTION, EventDoc};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::store::{self, tx_replace};
use crate::volunteers::GlobalRole;

fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

pub async fn patch_event(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(event_id): Path<String>,
    Json(patch): Json<EventPatch>,
) -> Result<Json<Value>, ApiError> {
    let civil_defense = volunteer.has_global(GlobalRole::CivilDefense);
    // Contacts: validator or Civil Defense. Name: an admin of a shelter of the event, or Civil Defense.
    if patch.reference_contacts.is_some() && !volunteer.is_validator_or_civil_defense() {
        return Err(forbidden());
    }
    let event = store::transact(&state.db, |db, tx| {
        let (event_id, volunteer) = (event_id.clone(), volunteer.clone());
        let patch = patch.clone();
        Box::pin(async move {
            let mut event: EventDoc = store::get(&db, COLLECTION, &event_id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "event_not_found"))?;
            if patch.name.is_some()
                && !civil_defense
                && !volunteer.memberships.iter().any(|m| {
                    m.event_id.as_deref() == Some(event_id.as_str())
                        && m.role == crate::members::ShelterRole::Admin
                })
            {
                return Err(forbidden());
            }
            apply(&mut event, &patch)?;
            tx_replace(&db, tx, COLLECTION, &event_id, &event)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new(super::ACTION_UPDATE, "event", &event_id)
                    .basis(LegalBasis::PublicPolicy)
                    .by(&volunteer.id),
            )?;
            Ok(event)
        })
    })
    .await?;
    Ok(Json(json!({ "event": event.view(&event_id) })))
}
