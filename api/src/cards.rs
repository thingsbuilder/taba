//! Cards of a household (spec 3.5.1, 7, 8.5). Owner B.
#![allow(dead_code)] // stage 0 scaffold of phase 3: the owner removes this once the module is wired

use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};

use crate::auth::Volunteer;
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

// ---- routes (owner B) ----

pub async fn post_card(_volunteer: Volunteer) -> Result<(), ApiError> {
    Err(ApiError::not_implemented())
}

pub async fn get_card(_volunteer: Volunteer) -> Result<(), ApiError> {
    Err(ApiError::not_implemented())
}

pub async fn cancel_card(_volunteer: Volunteer) -> Result<(), ApiError> {
    Err(ApiError::not_implemented())
}
