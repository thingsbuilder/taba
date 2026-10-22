//! The stored card and the bodies of its routes (spec 3.5.1, 7).

use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::ApiError;

pub const COLLECTION: &str = "cards";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardStatus {
    Active,
    Canceled,
}

/// `cards/{code}`: 8 characters of Crockford base32, made on the phone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub code: Option<String>,
    pub household_id: String,
    pub shelter_id: String,
    pub status: CardStatus,
    pub issued_by: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub client_issued_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub canceled_at: Option<FirestoreInstant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_by: Option<String>,
}

pub(super) fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_card");
    error.detail = json!({ "field": field });
    error
}

pub(super) fn card_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "card_not_found")
}

#[derive(Debug, Deserialize)]
pub struct IssueCard {
    pub(super) code: String,
    pub(super) client_issued_at: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct CancelCard {
    #[serde(default)]
    pub(super) replaced_by: Option<String>,
}
