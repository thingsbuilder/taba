//! Shelters (spec 3.2, 3.9, 4.2, 7, 8.2, 8.3): creation under the 100 m rule, edits, closing,
//! and confirmation by a validator.
//!
//! Five layers, one file each: `model` (the stored document and its types), `domain` (validation
//! and transitions as pure functions), `persist` (the transactions), `http` (the routes) and
//! `view` (the JSON shown).

// The audit actions stay here: the conformance tests look for them in this file.
const ACTION_CREATE: &str = "shelter.create";
const ACTION_UPDATE: &str = "shelter.update";
const ACTION_CLOSE: &str = "shelter.close";
const ACTION_CONFIRM: &str = "shelter.confirm";
const ACTION_REVOKE: &str = "shelter.revoke";

mod domain;
mod http;
mod model;
mod persist;
mod view;

#[cfg(test)]
mod tests;

pub use domain::valid_position;
pub use http::{
    close_shelter, confirm_shelter, get_shelter, patch_shelter, post_shelters, revoke_shelter,
};
pub use model::*;
pub use view::view_for;
