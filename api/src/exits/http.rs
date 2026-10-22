//! `POST /v1/shelters/{id}/exits` (spec 3.8): adults, children and exceptions.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde_json::{Value, json};

use super::domain::{municipality, open_stay, plan};
use super::model::{Exit, ExitRequest, Plan, forbidden, invalid, not_in_shelter};
use super::persist::{close_stay, record_failed_revalidation};
use crate::auth::{self, AppState, Purpose, Volunteer, resolve_person, usable_capture};
use crate::children::can_change_status;
use crate::error::ApiError;
use crate::events;
use crate::members;
use crate::people::{self, PersonDoc};
use crate::shelters::{self, ShelterDoc};
use crate::store;

pub async fn post_exit(
    State(state): State<AppState>,
    headers: HeaderMap,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
    Json(request): Json<ExitRequest>,
) -> Result<Json<Value>, ApiError> {
    let member = volunteer.role_in(&shelter_id).is_some();
    let privileged = can_change_status(&volunteer);
    if !member && !privileged {
        return Err(forbidden());
    }
    let shelter = store::get::<ShelterDoc>(&state.db, shelters::COLLECTION, &shelter_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found"))?;
    events::ensure_open(&state.db, shelter.event_id.as_deref()).await?;
    let destination = request
        .exit_destination
        .ok_or_else(|| invalid("exit_destination_required"))?;
    let municipality = municipality(&request.destination_municipality)?;
    let person: PersonDoc = store::get(&state.db, people::COLLECTION, &request.person_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "person_not_found"))?;
    open_stay(&person, &shelter_id).ok_or_else(not_in_shelter)?;
    let decided = plan(member, privileged, &request, &person)?;

    let mut exit = Exit {
        shelter_id: shelter_id.clone(),
        volunteer: volunteer.clone(),
        member,
        privileged,
        request,
        destination,
        municipality,
        revalidated: None,
    };
    if let Plan::Revalidate {
        guardian_id,
        capture_id,
    } = &decided
    {
        let device = auth::device_id(&headers)?;
        let mut capture =
            usable_capture(&state, &device, capture_id, Purpose::Revalidation).await?;
        if capture.volunteer_id.as_deref() != Some(volunteer.id.as_str())
            || capture.shelter_id.as_deref() != Some(shelter_id.as_str())
            || capture.expected_person_id.as_deref() != Some(guardian_id.as_str())
        {
            return Err(invalid("wrong_capture_target"));
        }
        let resolved = resolve_person(&state, capture_id, &mut capture).await?;
        if &resolved != guardian_id {
            record_failed_revalidation(&state, &exit, capture_id, guardian_id, &resolved).await?;
            return Err(ApiError::revalidation_failed());
        }
        exit.revalidated = Some(resolved);
    }

    let left_at = store::transact(&state.db, |db, tx| {
        let exit = exit.clone();
        Box::pin(async move { close_stay(&db, tx, &exit).await })
    })
    .await?;
    members::touch_shelter_event(&state, &shelter_id).await;
    Ok(Json(json!({
        "person_id": exit.request.person_id,
        "left_at": left_at.to_string(),
        "exit_destination": destination,
    })))
}
