//! Thumbnail of a person with no name or wristband (spec 4.6): AES-256-GCM with `THUMBNAIL_KEY`
//! and `GET /v1/people/{id}/thumbnail`. Owner A.
#![allow(dead_code)] // stage 0 scaffold of phase 3: the owner removes this once the module is wired

use crate::auth::Volunteer;
use crate::error::ApiError;

pub async fn get_thumbnail(_volunteer: Volunteer) -> Result<(), ApiError> {
    Err(ApiError::not_implemented())
}
