//! Rescue registration (spec 3.6, 7, 8.7, 8.2): `POST /v1/rescues`, `PATCH /v1/rescues/{id}` and
//! `GET /v1/shelters/nearby`.
//!
//! Layers, one file each: `model` (the stored rescue and the registration), `domain` (the
//! rules and validation as plain functions), `persist` (the reads and the transaction),
//! `http` (the routes) and `view` (the JSON shown).

// The audit actions stay here: the conformance tests look for them in this file.
const ACTION_CREATE: &str = "rescue.create";
const ACTION_UPDATE: &str = "rescue.update";

mod domain;
mod http;
mod model;
mod persist;
mod view;

#[cfg(test)]
mod tests;

pub use http::{nearby_shelters, patch_rescue, post_rescue};
pub use model::*;
pub use persist::{can_see_thumbnail, registered_rescue_of};
