//! The routes (spec 2.1, 8.4): only a validator or the Civil Defense may grant or revoke.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::domain::grantable;
use super::persist::change;
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;

#[derive(Debug, Deserialize)]
pub struct RoleRequest {
    role: String,
}

fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

/// `POST /v1/volunteers/{id}/roles`: `{role: mediator|protection}`; records the grant in `role_grants`.
pub async fn post_role(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(target): Path<String>,
    Json(request): Json<RoleRequest>,
) -> Result<Json<Value>, ApiError> {
    if !volunteer.is_validator_or_civil_defense() {
        return Err(forbidden());
    }
    let role = grantable(&request.role)?;
    change(&state, &volunteer, &target, role, true).await?;
    Ok(Json(json!({})))
}

/// `DELETE /v1/volunteers/{id}/roles/{role}`.
pub async fn delete_role(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path((target, role)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    if !volunteer.is_validator_or_civil_defense() {
        return Err(forbidden());
    }
    let role = grantable(&role)?;
    change(&state, &volunteer, &target, role, false).await?;
    Ok(StatusCode::NO_CONTENT)
}
