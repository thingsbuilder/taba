//! Mediation of a search (spec 4.8, 7, 8.7): the three queues, the check and the decision.
//! Stage 0: the data and the routes answer `501 not_implemented`.
#![allow(dead_code)] // stage 0 scaffold: the owner of this module removes this line

use axum::Json;
use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::Volunteer;
use crate::error::ApiError;

pub const COLLECTION: &str = "matches";

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

/// `GET /v1/matches`: the queues the volunteer attends (spec 4.8).
pub async fn list_matches(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}

/// `GET /v1/matches/{id}`: what the check needs, without the family's contact.
pub async fn get_match(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}

/// `POST /v1/matches/{id}/decision`: `accept | decline | not_the_person`; the first to decide closes it.
pub async fn post_decision(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}
