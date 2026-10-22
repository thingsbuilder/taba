//! The stored rescue (spec 7) and the registration the transaction works from.

use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};

use crate::people::Level;

pub const COLLECTION: &str = "rescues";

/// `rescues/{rescue_id}` (UUID of the client), spec 7.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RescueDoc {
    pub person_id: String,
    pub volunteer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landing_point: Option<String>,
    /// The phone's clock.
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub captured_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub received_at: FirestoreInstant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_shelter_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The request after validation, with what the transaction needs.
#[derive(Clone)]
pub(super) struct Registration {
    pub(super) rescue_id: String,
    pub(super) person_id: String,
    pub(super) level: Level,
    pub(super) volunteer_id: String,
    pub(super) device: Option<String>,
    pub(super) lat: f64,
    pub(super) lon: f64,
    pub(super) accuracy_m: f64,
    pub(super) captured_at: FirestoreInstant,
    pub(super) name: Option<String>,
    pub(super) wristband: Option<String>,
    pub(super) note: Option<String>,
    pub(super) landing_point: Option<String>,
    pub(super) destination_shelter_id: Option<String>,
    pub(super) event_id: Option<String>,
    /// Encrypted JPEG; only when the rescued person has neither name nor wristband (spec 4.6).
    pub(super) thumbnail: Option<Vec<u8>>,
}
