//! `POST /v1/people/{person_id}/guardians` (spec 3.4.1, 4.9).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::domain::{can_change_status, guardian_entry};
use super::model::GuardianRequest;
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::people::{self, Level, PersonDoc};
use crate::store::{self, tx_replace};

pub async fn post_guardian(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(person_id): Path<String>,
    Json(request): Json<GuardianRequest>,
) -> Result<Json<Value>, ApiError> {
    let guardians = store::transact(&state.db, |db, tx| {
        let (person_id, request, volunteer) =
            (person_id.clone(), request.clone(), volunteer.clone());
        Box::pin(async move {
            let mut child: PersonDoc = store::get(&db, people::COLLECTION, &person_id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "person_not_found"))?;
            let in_shelter = child
                .current_shelter_id
                .as_deref()
                .is_some_and(|shelter| volunteer.role_in(shelter).is_some());
            if !in_shelter && !can_change_status(&volunteer) {
                return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden"));
            }
            if let Some(shelter_id) = child.current_shelter_id.as_deref() {
                crate::events::ensure_shelter_open(&db, shelter_id).await?;
            }
            if !child.is_child {
                return Err(ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "not_a_child",
                ));
            }
            if request.guardian_id == person_id {
                return Err(ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_guardian",
                ));
            }
            let guardian: Option<PersonDoc> =
                store::get(&db, people::COLLECTION, &request.guardian_id).await?;
            if guardian.is_none_or(|g| g.level != Level::Verified) {
                return Err(ApiError::guardian_not_verified());
            }
            let entry = guardian_entry(&request, &volunteer);
            match child
                .guardians
                .iter_mut()
                .find(|g| g.person_id == entry.person_id)
            {
                Some(existing) => *existing = entry,
                None => child.guardians.push(entry),
            }
            child.sync_guardian_ids();
            child.updated_at = store::now();
            tx_replace(&db, tx, people::COLLECTION, &person_id, &child)?;
            let mut audit_entry = AuditEntry::new("person.guardian", "person", &person_id)
                .by(&volunteer.id)
                .basis(LegalBasis::ChildProtection);
            if let Some(shelter) = &child.current_shelter_id {
                audit_entry = audit_entry.in_shelter(shelter);
            }
            audit::add(&db, tx, &audit_entry)?;
            Ok(child.guardians)
        })
    })
    .await?;
    let view: Vec<Value> = guardians
        .iter()
        .map(|g| {
            json!({
                "person_id": g.person_id,
                "link": g.link,
                "checked_by": g.checked_by,
                "checked_at": g.checked_at.map(|t| t.to_string()),
            })
        })
        .collect();
    if let Ok(Some(child)) =
        store::get::<PersonDoc>(&state.db, people::COLLECTION, &person_id).await
    {
        if let Some(shelter_id) = child.current_shelter_id.as_deref() {
            crate::members::touch_shelter_event(&state, shelter_id).await;
        }
    }
    Ok(Json(json!({ "person_id": person_id, "guardians": view })))
}
