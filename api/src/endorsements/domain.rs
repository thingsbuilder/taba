//! The rules on an endorsement code (spec 3.9): it works once, until it expires, and only for a
//! phone within the radius of the one that made it. Nothing here is `async` or reads the
//! database.

use axum::http::StatusCode;
use firestore::FirestoreInstant;
use firestore::jiff::SignedDuration;
use serde_json::json;

use super::model::{EndorsementCodeDoc, Position};
use crate::error::ApiError;
use crate::geo;
use crate::shelters::valid_position;

pub(super) const CODE_TTL: SignedDuration = SignedDuration::from_mins(10);

impl EndorsementCodeDoc {
    /// A code works once and until it expires (spec 3.9.2).
    pub(super) fn is_usable(&self, now: FirestoreInstant) -> bool {
        !self.used && now < self.expires_at
    }

    /// The distance between the phone that made the code and the endorser's, which must be known
    /// to the metre and within `radius_m` (spec 3.9.3).
    pub(super) fn distance_to(&self, position: &Position, radius_m: f64) -> Result<f64, ApiError> {
        valid_position(position.lat, position.lon, position.accuracy)?;
        let distance_m = geo::haversine_m(self.lat, self.lon, position.lat, position.lon);
        if distance_m > radius_m {
            return Err(too_far(distance_m));
        }
        Ok(distance_m)
    }
}

fn too_far(distance_m: f64) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "too_far");
    error.detail = json!({ "distance_m": distance_m.round() });
    error
}
