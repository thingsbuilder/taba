//! The stored endorsement code and the bodies of the endorsement routes (spec 3.9, 8.3).

use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

pub const COLLECTION: &str = "endorsement_codes";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndorsementCodeDoc {
    pub shelter_id: String,
    pub created_by: String,
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: f64,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub expires_at: FirestoreInstant,
    pub used: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Position {
    pub(super) lat: f64,
    pub(super) lon: f64,
    pub(super) accuracy: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Endorse {
    pub(super) code: String,
    pub(super) endorser_shelter_id: String,
    #[serde(flatten)]
    pub(super) position: Position,
}

pub(super) fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

pub(super) fn already_confirmed() -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "already_confirmed")
}
