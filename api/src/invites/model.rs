//! The stored invite and the vocabulary around it (spec 3.3, 7).

use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::members::ShelterRole;

pub const COLLECTION: &str = "invites";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InviteKind {
    Qr,
    Link,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InviteStatus {
    Active,
    Used,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteDoc {
    pub shelter_id: String,
    pub kind: InviteKind,
    pub role: ShelterRole,
    pub created_by: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub expires_at: FirestoreInstant,
    pub status: InviteStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_by: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub used_at: Option<FirestoreInstant>,
}

pub(super) fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

pub(super) fn invite_invalid() -> ApiError {
    ApiError::new(StatusCode::GONE, "invite_invalid")
}
