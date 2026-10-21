//! Search for missing people (spec 3.7, 7, 8.7): `POST /v1/searches` and the photo pipeline that
//! the family link shares. Stage 0: the data, the shared signature and the route (`501`).
#![allow(dead_code)] // stage 0 scaffold: the owner of this module removes this line

use axum::Json;
use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;

pub const COLLECTION: &str = "searches";

/// Who made the search: the volunteer typing it, or the family through a link the volunteer made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchVia {
    Volunteer,
    FamilyLink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchStatus {
    Active,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissingPerson {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Family {
    pub name: String,
    pub phone: String,
    pub relationship: String,
    /// `pt-BR`, `en` or `es`: the language of the WhatsApp message (spec 11.1).
    pub language: String,
}

/// `searches/{search_id}` (UUID of the client), spec 7. Only the `person_id` derived from each
/// photo is kept, never the photo or the token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    /// The volunteer in whose name the search is: who typed it, or who made the family link.
    pub created_by: String,
    pub via: SearchVia,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_shelter_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default)]
    pub person_ids: Vec<String>,
    pub photos_sent: u32,
    pub usable_photos: u32,
    pub missing_person: MissingPerson,
    pub family: Family,
    pub status: SearchStatus,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub expiry_notified_at: Option<FirestoreInstant>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
}

/// Who a registered search belongs to, for `register_photos`.
#[derive(Debug, Clone)]
pub struct SearchOwner {
    pub volunteer_id: String,
    pub device_id: Option<String>,
    pub origin_shelter_id: Option<String>,
    pub event_id: Option<String>,
    pub via: SearchVia,
    /// What keeps the paid answer of Unico per photo for a retry: the `Idempotency-Key` of
    /// `POST /v1/searches`, or the link code of the family link. The photos are never paid twice.
    pub key: Option<String>,
}

/// What the search needs, already validated by the route: 1 to 5 photos, each decoded and
/// at most 800 KB.
#[derive(Debug, Clone)]
pub struct SearchInput {
    pub id: String,
    pub photos: Vec<Vec<u8>>,
    pub missing_person: MissingPerson,
    pub family: Family,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisterOutcome {
    pub usable_photos: u32,
}

/// The pipeline both `POST /v1/searches` and the family link's submit run (spec 3.7 items 5 and
/// 11): one token per photo, keep only the derived `person_id`s, write the search, try the match
/// now (`matching`), and answer the same whether or not there was one. `503 unico_degraded` when
/// Unico cannot be used, without writing anything. Owner B (`searches.rs`); called by owner E
/// (`search_links.rs`). Stage 0: not written.
pub async fn register_photos(
    _state: &AppState,
    _owner: SearchOwner,
    _input: SearchInput,
) -> Result<RegisterOutcome, ApiError> {
    Err(ApiError::not_implemented())
}

/// `POST /v1/searches`: `202 {message: "search_registered", usable_photos}`; `429 search_limit`.
pub async fn post_search(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}
