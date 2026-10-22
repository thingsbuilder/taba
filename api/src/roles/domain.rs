//! The role names the app may change (spec 2.1, 8.4).

use axum::http::StatusCode;
use serde_json::json;

use crate::error::ApiError;
use crate::volunteers::GlobalRole;

/// Only the two roles the app may change; the others are the operator's.
pub(super) fn grantable(role: &str) -> Result<GlobalRole, ApiError> {
    match role {
        "mediator" => Ok(GlobalRole::Mediator),
        "protection" => Ok(GlobalRole::Protection),
        _ => {
            let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_role");
            error.detail = json!({ "field": "role" });
            Err(error)
        }
    }
}
