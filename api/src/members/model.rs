//! The stored membership and the vocabulary around it (spec 2.3, 3.3, 4.4).

use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Grant,
    Revoke,
}

pub const COLLECTION: &str = "memberships";

pub fn membership_id(shelter_id: &str, volunteer_id: &str) -> String {
    format!("{shelter_id}_{volunteer_id}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShelterRole {
    Admin,
    Volunteer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipStatus {
    Active,
    Removed,
    Left,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    Creator,
    Qr,
    Link,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MembershipDoc {
    pub shelter_id: String,
    pub volunteer_id: String,
    pub role: ShelterRole,
    pub status: MembershipStatus,
    pub via: Via,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invited_by: Option<String>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub joined_at: FirestoreInstant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_by: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub removed_at: Option<FirestoreInstant>,
}

pub(super) fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

pub(super) fn member_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "member_not_found")
}

pub(super) fn last_admin() -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "last_admin")
}
