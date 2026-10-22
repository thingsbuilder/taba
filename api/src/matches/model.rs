//! The stored match and the vocabulary around it (spec 4.8, 7).

use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::ApiError;

pub const COLLECTION: &str = "matches";
/// The most matches one queue lists per status.
// ponytail: past 100 in one queue and status the listing keeps the newest 100 and answers
// `has_more: true`; there is no cursor to read the rest. Add one if a queue passes that.
pub(super) const LIST_LIMIT: u32 = 100;
pub(super) const MAX_NOTE_CHARS: usize = 500;

/// Who attends the match (spec 4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediatorKind {
    Shelter,
    Central,
    Protection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchStatus {
    Open,
    Accepted,
    Declined,
    NotThePerson,
}

impl MatchStatus {
    pub(super) const ALL: [MatchStatus; 4] = [
        MatchStatus::Open,
        MatchStatus::Accepted,
        MatchStatus::Declined,
        MatchStatus::NotThePerson,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecidedBy {
    Person,
    Operator,
}

/// `matches/{sha256(search_id|person_id)}`, spec 7.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    pub search_id: String,
    pub person_id: String,
    pub is_child: bool,
    pub mediator_kind: MediatorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mediator_shelter_id: Option<String>,
    pub status: MatchStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<DecidedBy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by_volunteer: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub decided_at: Option<FirestoreInstant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
}

pub(super) fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

pub(super) fn match_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "match_not_found")
}

pub(super) fn invalid_request(message: String) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
    error.detail = json!({ "message": message });
    error
}
