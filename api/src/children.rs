//! Children (spec 3.4.1, 4.9): guardians, `child_status` changes and the `child_separated` alert.
//! Owner C.
#![allow(dead_code)] // stage 0 scaffold of phase 3: the owner removes this once the module is wired

use crate::auth::Volunteer;
use crate::error::ApiError;

// ---- routes (owner C) ----

pub async fn post_guardian(_volunteer: Volunteer) -> Result<(), ApiError> {
    Err(ApiError::not_implemented())
}
