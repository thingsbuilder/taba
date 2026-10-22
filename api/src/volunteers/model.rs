//! The stored volunteer and the profile the app sends (spec 7, 8.1).

use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};

pub const COLLECTION: &str = "volunteers";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GlobalRole {
    Validator,
    CivilDefense,
    Mediator,
    Protection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleGrant {
    pub role: GlobalRole,
    pub by: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub at: FirestoreInstant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolunteerDoc {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    pub show_phone: bool,
    pub language: String,
    #[serde(default)]
    pub global_roles: Vec<GlobalRole>,
    #[serde(default)]
    pub role_grants: Vec<RoleGrant>,
    #[serde(default)]
    pub banned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banned_by: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub banned_at: Option<FirestoreInstant>,
    #[serde(default)]
    pub flagged: bool,
    #[serde(default)]
    pub flag_reasons: Vec<String>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub last_seen_at: FirestoreInstant,
}

/// Profile as the app sends it (spec 8.1).
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileInput {
    pub name: String,
    #[serde(default)]
    pub phone: Option<String>,
    pub show_phone: bool,
    pub language: String,
}
