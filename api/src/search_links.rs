//! The family's search link (spec 3.7 item 11, 7, 10.4): a one-use link of 12 hours that the
//! volunteer shares; the family fills the search in a public page. Stage 0: the data and the
//! four routes answer `501 not_implemented` (the last two need no session).
#![allow(dead_code)] // stage 0 scaffold: the owner of this module removes this line

use axum::Json;
use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::Volunteer;
use crate::error::ApiError;

pub const COLLECTION: &str = "search_links";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkStatus {
    Active,
    Used,
    Canceled,
}

/// `search_links/{code}` (128 bits, base64url); TTL on `expires_at`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchLinkDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    pub created_by: String,
    pub shelter_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    pub status: LinkStatus,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    /// `created_at` + 12 hours.
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub expires_at: FirestoreInstant,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub used_at: Option<FirestoreInstant>,
    /// The search the submission made, once the link is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_id: Option<String>,
    /// Failed or repeated submissions, for the limit per link.
    #[serde(default)]
    pub attempts: u32,
}

/// `POST /v1/search-links`: `201 {code, url, expires_at}`; counts in `SEARCHES_PER_DAY` (`429 search_limit`).
pub async fn post_search_link(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}

/// `DELETE /v1/search-links/{code}`: cancels it.
pub async fn delete_search_link(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}

/// `GET /v1/search-links/{code}`, public: only whether the link is valid.
pub async fn get_search_link() -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}

/// `POST /v1/search-links/{code}/submit`, public: the same pipeline as `POST /v1/searches`
/// (`searches::register_photos`); `410 search_link_invalid`, `503 unico_degraded` keeps the link.
pub async fn submit_search_link() -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}
