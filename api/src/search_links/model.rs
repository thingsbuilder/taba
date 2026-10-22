//! The stored search link and the vocabulary around it (spec 3.7 item 11, 7).

use axum::http::StatusCode;
use firestore::FirestoreInstant;
use firestore::jiff::SignedDuration;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::ApiError;
use crate::store;

pub const COLLECTION: &str = "search_links";
pub(super) const TTL: SignedDuration = SignedDuration::from_hours(12);
/// Failed submissions a link takes before it answers `410`.
pub(super) const MAX_ATTEMPTS: u32 = 5;
/// How long one submit holds the link while Unico is being asked (Unico's own deadline is 60 s).
/// Longer than the pipeline: 5 photos, each up to 60 s at Unico, so the claim does not lapse while it runs.
pub(super) const LEASE: SignedDuration = SignedDuration::from_secs(360);
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
    /// Failed submissions, for the limit per link.
    #[serde(default)]
    pub attempts: u32,
    /// A submit is running until this instant; no other one may start.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub processing_until: Option<FirestoreInstant>,
}

impl SearchLinkDoc {
    /// Whether the family can still submit: active, not expired, attempts left.
    pub(super) fn is_valid(&self, now: FirestoreInstant) -> bool {
        self.status == LinkStatus::Active && now < self.expires_at && self.attempts < MAX_ATTEMPTS
    }
}
pub(super) fn invalid_shelter() -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_shelter");
    error.detail = json!({ "field": "shelter_id" });
    error
}

pub(super) fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

pub(super) fn busy() -> ApiError {
    let mut error = ApiError::new(StatusCode::SERVICE_UNAVAILABLE, store::CONTENTION);
    error.retry_after = Some(1);
    error
}
