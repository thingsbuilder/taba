//! The routes (spec 8.4): list, promote, remove, leave and ban.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::model::{ShelterRole, forbidden};
use super::persist::{self, active_in_shelter, touch_shelter_event};
use super::view::member_row;
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::store;
use crate::volunteers::{COLLECTION as VOLUNTEERS, GlobalRole, VolunteerDoc};

const MAX_REASON_CHARS: usize = 500;

// ---- GET /v1/shelters/{id}/members ----

pub async fn list_members(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if volunteer.role_in(&shelter_id).is_none() && !volunteer.has_global(GlobalRole::CivilDefense) {
        return Err(forbidden());
    }
    let mut rows = Vec::new();
    for doc in active_in_shelter(&state.db, &shelter_id).await? {
        if let Some(profile) =
            store::get::<VolunteerDoc>(&state.db, VOLUNTEERS, &doc.volunteer_id).await?
        {
            rows.push((doc.joined_at, member_row(&doc, &profile)));
        }
    }
    rows.sort_by_key(|(joined_at, _)| *joined_at);
    let members: Vec<Value> = rows.into_iter().map(|(_, row)| row).collect();
    Ok(Json(json!({ "members": members })))
}

// ---- PATCH /v1/shelters/{id}/members/{volunteer_id} ----

#[derive(Debug, Deserialize)]
pub struct Promote {
    role: ShelterRole,
}

pub async fn promote_member(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path((shelter_id, target)): Path<(String, String)>,
    Json(request): Json<Promote>,
) -> Result<Json<Value>, ApiError> {
    if !volunteer.is_admin_of(&shelter_id) {
        return Err(forbidden());
    }
    crate::events::ensure_shelter_open(&state.db, &shelter_id).await?;
    if request.role != ShelterRole::Admin {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_role",
        ));
    }
    let (doc, profile) = persist::promote(&state, &shelter_id, &target, &volunteer.id).await?;
    touch_shelter_event(&state, &shelter_id).await;
    Ok(Json(json!({ "member": member_row(&doc, &profile) })))
}

// ---- DELETE /v1/shelters/{id}/members/{volunteer_id} ----

pub async fn remove_member(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path((shelter_id, target)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let civil_defense = volunteer.has_global(GlobalRole::CivilDefense);
    if !volunteer.is_admin_of(&shelter_id) && !civil_defense {
        return Err(forbidden());
    }
    crate::events::ensure_shelter_open(&state.db, &shelter_id).await?;
    persist::remove(&state, &shelter_id, &target, &volunteer.id, civil_defense).await?;
    touch_shelter_event(&state, &shelter_id).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- POST /v1/shelters/{id}/leave ----

pub async fn leave_shelter(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if volunteer.role_in(&shelter_id).is_none() {
        return Err(forbidden());
    }
    persist::leave(&state, &shelter_id, &volunteer.id).await?;
    touch_shelter_event(&state, &shelter_id).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- POST /v1/volunteers/{id}/ban ----

#[derive(Debug, Deserialize)]
pub struct Ban {
    reason: String,
}

/// Bans a volunteer from the network (spec 4.4): marked banned, removed from every shelter,
/// sessions deleted. Sessions that outlive a failed delete are refused anyway (`Volunteer`).
pub async fn ban_volunteer(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(target): Path<String>,
    Json(request): Json<Ban>,
) -> Result<Json<Value>, ApiError> {
    if !volunteer.has_global(GlobalRole::CivilDefense) {
        return Err(forbidden());
    }
    let reason = request.reason.trim().to_string();
    if reason.is_empty() || reason.chars().count() > MAX_REASON_CHARS {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_reason",
        ));
    }
    persist::ban(&state, &target, &volunteer.id, &reason).await?;
    Ok(Json(json!({ "volunteer_id": target, "banned": true })))
}
