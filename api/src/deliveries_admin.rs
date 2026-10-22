//! The admin side of the deliveries (spec 3.5.2, 3.5.3 item 8, 8.6): `PUT /v1/shelters/{id}/items`,
//! `GET /v1/shelters/{id}/deliveries?status=` and `POST /v1/deliveries/{id}/review`. Stage 0: the
//! routes answer `501 not_implemented`; the types are in `deliveries.rs`.
#![allow(dead_code)] // stage 0 scaffold: the owner of this module removes this line

use axum::Json;
use serde_json::Value;

use crate::auth::Volunteer;
use crate::error::ApiError;

/// `PUT /v1/shelters/{id}/items`: `{items: {<name>: {scope, period_days?, once_per_event, high_value}}}`, by an admin of the shelter; replaces `shelters.items`.
pub async fn put_items(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}

/// `GET /v1/shelters/{id}/deliveries?status=`: the shelter's deliveries, by an admin of it.
pub async fn list_deliveries(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}

/// `POST /v1/deliveries/{id}/review`: `{decision: reviewed_ok | irregular, note}`, by an admin of the delivery's shelter.
pub async fn review_delivery(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}
