//! The rows the hourly steps read.

use firestore::FirestoreInstant;
use serde::Deserialize;

/// A document with its id, for queries over types that do not carry one.
#[derive(Deserialize)]
pub(super) struct Listed<T> {
    #[serde(alias = "_firestore_id")]
    pub(super) id: String,
    #[serde(flatten)]
    pub(super) doc: T,
}

#[derive(Deserialize)]
pub(super) struct PersonEvents {
    #[serde(default)]
    pub(super) event_ids: Vec<String>,
    #[serde(default)]
    pub(super) current_shelter_id: Option<String>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub(super) updated_at: FirestoreInstant,
}

/// What the expiry needs of a document of any of the three collections.
#[derive(Deserialize)]
pub(super) struct Expiring {
    #[serde(alias = "_firestore_id", default)]
    pub(super) id: Option<String>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub(super) expires_at: FirestoreInstant,
    #[serde(default)]
    pub(super) used: bool,
    #[serde(default)]
    pub(super) status: Option<String>,
}
