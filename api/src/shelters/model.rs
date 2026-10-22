//! The stored shelter and the types it is made of (spec 3.2, 3.9, 7): `shelters/{shelter_id}`.

use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};

pub const COLLECTION: &str = "shelters";
/// Largest `accuracy` accepted when creating a shelter or endorsing one (spec 4.2).
pub const MAX_ACCURACY_M: f64 = 100.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShelterStatus {
    Open,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confirmation {
    Unconfirmed,
    Confirmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationType {
    Validator,
    InPerson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndorsementKind {
    InPerson,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// True until Nominatim answers (spec 6.1.6).
    #[serde(default)]
    pub pending: bool,
}

/// The address as the app sends it; blank parts count as absent.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AddressInput {
    #[serde(default)]
    pub(super) text: Option<String>,
    #[serde(default)]
    pub(super) city: Option<String>,
    #[serde(default)]
    pub(super) state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Endorsement {
    pub kind: EndorsementKind,
    pub volunteer_id: String,
    pub endorser_shelter_id: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub at: FirestoreInstant,
    pub distance_m: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Revocation {
    pub by: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub at: FirestoreInstant,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShelterDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: f64,
    pub geohash: String,
    pub address: Address,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<u32>,
    pub created_by: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub client_created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    pub status: ShelterStatus,
    pub confirmation: Confirmation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation_type: Option<ConfirmationType>,
    pub can_endorse: bool,
    #[serde(default)]
    pub endorsements: Vec<Endorsement>,
    #[serde(default)]
    pub revocations: Vec<Revocation>,
    #[serde(default)]
    pub flagged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_site_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflicts_with: Option<String>,
    /// What the shelter gives and the rule of each item (spec 3.5.2); set by `PUT /v1/shelters/{id}/items`.
    #[serde(default, skip_serializing_if = "crate::deliveries::Items::is_empty")]
    pub items: crate::deliveries::Items,
}
