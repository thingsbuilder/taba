//! Global roles granted from the app (spec 2.1, 8.4): `mediator` and `protection`, by a
//! validator or the Civil Defense. Stage 0: the routes answer `501 not_implemented`.
#![allow(dead_code)] // stage 0 scaffold: the owner of this module removes this line

use axum::Json;
use serde_json::Value;

use crate::auth::Volunteer;
use crate::error::ApiError;

/// `POST /v1/volunteers/{id}/roles`: `{role: mediator|protection}`; records the grant in `role_grants`.
pub async fn post_role(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}

/// `DELETE /v1/volunteers/{id}/roles/{role}`.
pub async fn delete_role(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}
