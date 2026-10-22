//! The stored person and the vocabulary around it (spec 4.1, 7).

use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit::LegalBasis;
use crate::error::ApiError;

pub const COLLECTION: &str = "people";

/// Never goes down (spec 4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Verified,
    Assisted,
    NoBiometrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildStatus {
    WithGuardian,
    Separated,
    Unaccompanied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardianLink {
    Declared,
    DocumentChecked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guardian {
    pub person_id: String,
    pub link: GuardianLink,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_by: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub checked_at: Option<FirestoreInstant>,
}

/// Only these four booleans exist; anything else is a `422` (criterion 38).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Needs {
    #[serde(default)]
    pub continuous_medication: bool,
    #[serde(default)]
    pub dietary_restriction: bool,
    #[serde(default)]
    pub reduced_mobility: bool,
    #[serde(default)]
    pub health_follow_up: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disability {
    Mobility,
    Visual,
    Hearing,
    Intellectual,
    Psychosocial,
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vulnerabilities {
    #[serde(default)]
    pub pregnant: bool,
    #[serde(default)]
    pub lactating: bool,
    #[serde(default)]
    pub disability: Vec<Disability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indigenous_people: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrant_language: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    CheckinElsewhere,
    Exit,
    ExceptionalExit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitDestination {
    OwnHome,
    RelativesOrFriends,
    RentalAid,
    OtherShelter,
    Other,
    Unknown,
}

/// One passage through a shelter (spec 7); open while `left_at` is empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stay {
    pub shelter_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub entered_at: FirestoreInstant,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub left_at: Option<FirestoreInstant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_reason: Option<ExitReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_destination: Option<ExitDestination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_municipality: Option<String>,
    pub registered_by: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Checkin,
    Rescue,
}

/// `people/{person_id}`: the HMAC of the token, or the client's UUID while `no_biometrics`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    pub level: Level,
    #[serde(default)]
    pub biometrics_declined: bool,
    #[serde(default)]
    pub former_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub social_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wristband: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub birth_year: Option<u16>,
    #[serde(default)]
    pub is_child: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_status: Option<ChildStatus>,
    #[serde(default)]
    pub guardians: Vec<Guardian>,
    /// `guardians[].person_id` as a flat list, so a query can find every child of a guardian (the
    /// late biometrics move them to the new id). Kept in step with `guardians`: see `sync_guardian_ids`.
    #[serde(default)]
    pub guardian_ids: Vec<String>,
    #[serde(default)]
    pub needs: Needs,
    #[serde(default)]
    pub vulnerabilities: Vulnerabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub household_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_shelter_id: Option<String>,
    #[serde(default)]
    pub stays: Vec<Stay>,
    #[serde(default)]
    pub event_ids: Vec<String>,
    #[serde(default)]
    pub rescue_ids: Vec<String>,
    /// AES-256-GCM, nonce first (spec 4.6); stored as a Firestore bytes value.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::thumbnail::optional_bytes"
    )]
    pub thumbnail_encrypted: Option<Vec<u8>>,
    pub origin: Origin,
    pub legal_basis: LegalBasis,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub updated_at: FirestoreInstant,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub client_edited_at: Option<FirestoreInstant>,
}

impl Level {
    fn rank(self) -> u8 {
        match self {
            Level::NoBiometrics => 0,
            Level::Assisted => 1,
            Level::Verified => 2,
        }
    }

    /// The level never goes down (spec 4.1).
    pub fn raised_to(self, other: Level) -> Level {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

impl PersonDoc {
    /// Makes `guardian_ids` match `guardians`; call it after any change to the guardians.
    pub fn sync_guardian_ids(&mut self) {
        self.guardian_ids = self.guardians.iter().map(|g| g.person_id.clone()).collect();
    }

    /// The document id of a person read from the database. A stored person without one cannot be
    /// addressed again, so it is logged and refused instead of being turned into an empty id.
    pub fn stored_id(&self) -> Result<&str, ApiError> {
        self.id.as_deref().ok_or_else(|| {
            tracing::error!("integrity: a person document came back without an id");
            ApiError::internal()
        })
    }
}

pub fn person_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "person_not_found")
}

pub(super) fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

pub(super) fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_person");
    error.detail = json!({ "field": field });
    error
}
