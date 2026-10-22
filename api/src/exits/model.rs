//! The request of the exit route, the plan an exit needs and what its transaction carries
//! (spec 3.8).

use axum::http::StatusCode;
use serde::Deserialize;

use crate::auth::Volunteer;
use crate::error::ApiError;
use crate::people::ExitDestination;

#[derive(Debug, Clone, Deserialize)]
pub struct Exception {
    pub(super) reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExitRequest {
    pub(super) person_id: String,
    #[serde(default)]
    pub(super) exit_destination: Option<ExitDestination>,
    #[serde(default)]
    pub(super) destination_municipality: Option<String>,
    #[serde(default)]
    pub(super) guardian_id: Option<String>,
    #[serde(default)]
    pub(super) capture_id: Option<String>,
    #[serde(default)]
    pub(super) exception: Option<Exception>,
}

/// What an exit needs before the stay closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Plan {
    Adult,
    Exception {
        reason: String,
    },
    /// The guardian must be revalidated with the capture (spec 3.8).
    Revalidate {
        guardian_id: String,
        capture_id: String,
    },
}

pub(super) fn invalid(code: &'static str) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code)
}

pub(super) fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

pub(super) fn not_in_shelter() -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "not_in_shelter")
}

/// Everything the transaction that closes the stay needs.
#[derive(Clone)]
pub(super) struct Exit {
    pub(super) shelter_id: String,
    pub(super) volunteer: Volunteer,
    pub(super) member: bool,
    pub(super) privileged: bool,
    pub(super) request: ExitRequest,
    pub(super) destination: ExitDestination,
    pub(super) municipality: Option<String>,
    /// The person the revalidation capture turned out to be, once it was resolved.
    pub(super) revalidated: Option<String>,
}
