//! The rules on a rescue (spec 3.6, 2.3, 4.6, 4.12): the errors it answers with, the validation
//! of what the app sends and who may see a rescued person. Plain functions, nothing `async`.

use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde_json::{Value, json};

use crate::audit::LegalBasis;
use crate::auth::Volunteer;
use crate::error::ApiError;
use crate::people::{Level, Origin, PersonDoc};
use crate::volunteers::GlobalRole;

pub(super) fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

pub(super) fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
    error.detail = json!({ "field": field });
    error
}

pub(super) fn shelter_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found")
}

pub(super) fn rescue_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "rescue_not_found")
}

/// Trimmed; blank counts as absent.
pub(super) fn text(
    value: Option<String>,
    field: &str,
    max: usize,
) -> Result<Option<String>, ApiError> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim();
    if value.chars().count() > max {
        return Err(invalid(field));
    }
    Ok((!value.is_empty()).then(|| value.to_string()))
}

pub(super) fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ApiError> {
    serde_json::from_value(value).map_err(|err| {
        let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
        error.detail = json!({ "message": err.to_string() });
        error
    })
}

pub(super) fn valid_point(lat: f64, lon: f64) -> Result<(), ApiError> {
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(invalid("position"));
    }
    Ok(())
}

/// Registering a rescue: a volunteer or admin of a shelter, or the Civil Defense (spec 2.3).
pub(super) fn may_register(volunteer: &Volunteer) -> bool {
    !volunteer.memberships.is_empty() || volunteer.has_global(GlobalRole::CivilDefense)
}

pub(super) fn fresh_person(id: &str, level: Level, now: FirestoreInstant) -> PersonDoc {
    PersonDoc {
        id: Some(id.into()),
        level,
        biometrics_declined: false,
        former_ids: vec![],
        name: None,
        social_name: None,
        wristband: None,
        birth_year: None,
        is_child: false,
        child_status: None,
        guardians: vec![],
        guardian_ids: vec![],
        needs: Default::default(),
        vulnerabilities: Default::default(),
        household_id: None,
        current_shelter_id: None,
        stays: vec![],
        event_ids: vec![],
        rescue_ids: vec![],
        thumbnail_encrypted: None,
        origin: Origin::Rescue,
        legal_basis: LegalBasis::VitalInterest,
        created_at: now,
        updated_at: now,
        client_edited_at: None,
    }
}

/// A person who has never been in a shelter and is not in one now: the only case in which the
/// volunteer of the landing point sees them (one criterion for the record and the thumbnail).
pub(super) fn unsheltered(person: &PersonDoc) -> bool {
    person.current_shelter_id.is_none() && person.stays.is_empty()
}
