//! Deliveries (spec 3.5.3, 4.7, 7, 8.6): `POST /v1/shelters/{id}/deliveries`. Stage 0: the data and
//! the route answer `501 not_implemented`; the types here are shared with `deliveries_admin.rs`.
#![allow(dead_code)] // stage 0 scaffold: the owner of this module removes this line

use std::collections::BTreeMap;

use axum::Json;
use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::Volunteer;
use crate::error::ApiError;

pub const COLLECTION: &str = "deliveries";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemScope {
    PerPerson,
    PerHousehold,
}

/// One item a shelter gives (spec 3.5.2); `shelters.items` maps the item name to this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemSpec {
    pub scope: ItemScope,
    /// Days between two deliveries of the item; absent means the default of 7. Ignored when `once_per_event`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_days: Option<u32>,
    #[serde(default)]
    pub once_per_event: bool,
    #[serde(default)]
    pub high_value: bool,
}

/// `shelters.items` (spec 7): item name to its rule.
pub type Items = BTreeMap<String, ItemSpec>;

/// How the person who took the item was checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    Card,
    Liveness,
    AdminApproval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Ok,
    InReview,
    ReviewedOk,
    Irregular,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonKind {
    DuplicateInPeriod,
    LivenessUnavailable,
}

/// Why a delivery waits for review (spec 4.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reason {
    pub kind: ReasonKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_delivery_id: Option<String>,
}

/// `deliveries/{delivery_id}` (UUID of the client), spec 7. `person_id` is a token id or, for a
/// `no_biometrics` person, a client UUID; `late biometrics` repoints it (`people::repoint_person_refs`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryDoc {
    pub shelter_id: String,
    pub shelter_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub household_id: Option<String>,
    /// Phase 6 fills it when the delivery is pseudonymized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub household_pseudonym: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_code: Option<String>,
    pub item: String,
    pub scope: ItemScope,
    pub quantity: u32,
    pub delivered_by: String,
    /// The phone's clock.
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub delivered_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub received_at: FirestoreInstant,
    pub verification: Verification,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    pub status: DeliveryStatus,
    #[serde(default)]
    pub reasons: Vec<Reason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_note: Option<String>,
    #[serde(default)]
    pub pseudonymized: bool,
}

/// `POST /v1/shelters/{id}/deliveries`: `{id, card_code, person_id, item, quantity, delivered_at,
/// capture_id?, admin_approval?: true}`. `201 {delivery_id, status, reasons}`; `428 liveness_required`;
/// `409 revalidation_failed`; `403 admin_approval_required`; `404 card_not_found | person_not_in_household`.
pub async fn post_delivery(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}
