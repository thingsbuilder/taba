//! The stored household and the bodies of its routes (spec 3.5.1, 7).

use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::ApiError;

pub const COLLECTION: &str = "households";

/// Companion animals are only a count (spec 3.5.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pet {
    pub species: String,
    pub count: u32,
}

/// `households/{household_id}` (UUID from the client).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HouseholdDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    #[serde(default)]
    pub member_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_id: Option<String>,
    #[serde(default)]
    pub pets: Vec<Pet>,
    /// The household's `active` card. The transaction that issues or cancels a card keeps it in
    /// step, so "one active card per household" is a check on this one document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_code: Option<String>,
    pub created_by: String,
    pub created_shelter_id: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub client_created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub updated_at: FirestoreInstant,
}

pub(super) fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_household");
    error.detail = json!({ "field": field });
    error
}

pub fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

pub(super) fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "household_not_found")
}

pub(super) fn person_not_found(person_id: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::NOT_FOUND, "person_not_found");
    error.detail = json!({ "person_id": person_id });
    error
}

pub(super) fn in_other_household(person_id: &str, household_id: &str) -> ApiError {
    let mut error = ApiError::person_in_other_household();
    error.detail = json!({ "person_id": person_id, "household_id": household_id });
    error
}

#[derive(Debug, Deserialize)]
pub struct CreateHousehold {
    pub(super) id: String,
    pub(super) member_ids: Vec<String>,
    #[serde(default)]
    pub(super) head_id: Option<String>,
    #[serde(default)]
    pub(super) pets: Vec<Pet>,
    pub(super) client_created_at: String,
}

#[derive(Debug, Clone)]
pub(super) struct NewHousehold {
    pub(super) id: String,
    pub(super) member_ids: Vec<String>,
    pub(super) head_id: Option<String>,
    pub(super) pets: Vec<Pet>,
    pub(super) client_created_at: FirestoreInstant,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PatchHousehold {
    #[serde(default)]
    pub(super) add_member_ids: Vec<String>,
    #[serde(default)]
    pub(super) remove_member_ids: Vec<String>,
    #[serde(default)]
    pub(super) head_id: Option<String>,
    #[serde(default)]
    pub(super) pets: Option<Vec<Pet>>,
}
