//! Exits and occupancy (spec 3.8, 4.2): `POST /v1/shelters/{id}/exits` (adults, children,
//! exceptions) and "N of capacity" from the open stays. Owner C.
#![allow(dead_code)] // stage 0 scaffold of phase 3: the owner removes this once the module is wired

use crate::auth::Volunteer;
use crate::error::ApiError;

// ---- routes (owner C) ----

pub async fn post_exit(_volunteer: Volunteer) -> Result<(), ApiError> {
    Err(ApiError::not_implemented())
}
