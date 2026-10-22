//! The stored event and its small types (spec 4.3, 7): `events/{event_id}`.

use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};

pub const COLLECTION: &str = "events";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    Active,
    Closed,
    Purged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosureReason {
    AllSheltersClosed,
    Inactivity,
}

/// What an event keeps of its numbers when its data is discarded (spec 7, `events.stats`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventStats {
    pub shelters: u32,
    pub unique_people: u32,
    pub households: u32,
    pub rescues: u32,
    pub deliveries: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceContacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub civil_defense_phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_protection_phone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDoc {
    pub name: String,
    pub auto_named: bool,
    pub status: EventStatus,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub last_activity_at: FirestoreInstant,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub closed_at: Option<FirestoreInstant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closure_reason: Option<ClosureReason>,
    /// When the event's data is discarded: `closed_at` + `PURGE_AFTER_DAYS` (spec 4.3).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub purge_at: Option<FirestoreInstant>,
    /// When the pseudonymized deliveries of the event are deleted: `closed_at` + `DELIVERY_RETENTION_DAYS`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub delivery_purge_at: Option<FirestoreInstant>,
    /// When the discard of the event ran.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub purged_at: Option<FirestoreInstant>,
    /// Written by the discard, before the data goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<EventStats>,
    #[serde(default)]
    pub reference_contacts: ReferenceContacts,
}
