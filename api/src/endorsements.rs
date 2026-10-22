//! In-person endorsement (spec 3.9, 8.3): the admin of a new shelter shows a code; an admin of
//! a confirmed shelter that may endorse reads it, within `SHELTER_RADIUS_M` of the first phone,
//! and confirms the new shelter.
//!
//! Four layers, one file each: `model` (the stored code and the request bodies), `domain` (when
//! a code is usable and how far the endorser may be, as pure functions), `persist` (the two
//! transactions) and `http` (the routes). There is no `view`: the answers are small JSON bodies
//! built in the routes, and the shelter is shown by `shelters`.

// The audit actions stay here: the conformance tests look for them in this file.
const ACTION_CODE: &str = "endorsement.code";
const ACTION_CREATE: &str = "endorsement.create";

mod domain;
mod http;
mod model;
mod persist;

#[cfg(test)]
mod tests;

pub use http::{create_code, endorse};
pub use model::*;
