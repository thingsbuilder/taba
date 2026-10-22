//! The export of a shelter (spec 8.8, Q22): `GET /v1/shelters/{id}/export.csv`, for an admin of a
//! `confirmed` shelter: its people, households and deliveries, no thumbnail and no `person_id`,
//! audited. Also the `stats` an event keeps when it is discarded. Stage 0: the route answers `501`.
#![allow(dead_code)] // stage 0 scaffold: the owner of this module removes this line

use axum::Json;
use serde_json::Value;

use crate::auth::Volunteer;
use crate::error::ApiError;

/// `GET /v1/shelters/{id}/export.csv`: `403` for anyone but an admin of a `confirmed` shelter.
pub async fn export_csv(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}
