//! The `volunteers` collection (spec 7) and `GET`/`PATCH /v1/me` (spec 8.1).
//!
//! Five layers, one file each: `model` (the stored volunteer and the profile the app sends),
//! `domain` (validation, roles and the patch as pure functions), `persist` (the write behind
//! the patch), `http` (the routes) and `view` (the JSON shown).

// The audit action stays here: the conformance tests look for it in this file.
const ACTION_UPDATE: &str = "volunteer.update";

mod domain;
mod http;
mod model;
mod persist;
mod view;

#[cfg(test)]
mod tests;

pub use domain::present;
pub use http::{get_me, patch_me};
pub use model::*;
pub use view::view;
