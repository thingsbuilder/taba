//! Rescue registration (spec 3.6, 7, 8.7): `POST /v1/rescues`, `PATCH /v1/rescues/{id}` and
//! `GET /v1/shelters/nearby`. Stage 0: the data and the routes answer `501 not_implemented`.
#![allow(dead_code)] // stage 0 scaffold: the owner of this module removes this line

use axum::Json;
use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::Volunteer;
use crate::error::ApiError;

pub const COLLECTION: &str = "rescues";

/// `rescues/{rescue_id}` (UUID of the client), spec 7.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RescueDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    pub person_id: String,
    pub volunteer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landing_point: Option<String>,
    /// The phone's clock.
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub captured_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub received_at: FirestoreInstant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_shelter_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Repoints `rescues.person_id` when a late biometrics migrates a person (spec 4.12). Called by
/// `people::migrate_in` inside its transaction. Stage 0: nothing to do; `people::repoint_person_refs`
/// already moves `rescues.person_id`, owner A checks it and may move the logic here.
pub async fn repoint_person(
    _db: &firestore::FirestoreDb,
    _tx: &mut firestore::FirestoreTransaction<'_>,
    _old: &str,
    _new: &str,
) -> Result<(), ApiError> {
    Ok(())
}

/// `POST /v1/rescues` (spec 8.7), through the queue.
pub async fn post_rescue(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}

/// `PATCH /v1/rescues/{id}`: `landing_point` and `destination_shelter_id`, by the author.
pub async fn patch_rescue(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}

/// `GET /v1/shelters/nearby?lat&lon`: open shelters near a point with capacity and occupancy (spec 8.2).
pub async fn nearby_shelters(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}
