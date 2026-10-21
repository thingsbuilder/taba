//! Households (spec 3.5.1, 7, 8.5). Owner B.
#![allow(dead_code)] // stage 0 scaffold of phase 3: the owner removes this once the module is wired

use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};

use crate::auth::Volunteer;
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
    pub created_by: String,
    pub created_shelter_id: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub client_created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub updated_at: FirestoreInstant,
}

// ---- routes (owner B) ----

pub async fn post_household(_volunteer: Volunteer) -> Result<(), ApiError> {
    Err(ApiError::not_implemented())
}

pub async fn get_household(_volunteer: Volunteer) -> Result<(), ApiError> {
    Err(ApiError::not_implemented())
}

pub async fn patch_household(_volunteer: Volunteer) -> Result<(), ApiError> {
    Err(ApiError::not_implemented())
}
