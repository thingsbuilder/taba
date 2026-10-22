//! People (spec 3.4, 4.1, 4.11, 4.12, 7, 8.5). Check-in in three ways, edits, late biometrics
//! and the history rules.
//!
//! Four layers, one file each: `model` (the stored document), `domain` (rules as pure
//! functions over it), `persist` (transactions that read, call the domain and write what it
//! returns), `http` (the routes) and `view` (the JSON shown).

// The audit actions stay here: the conformance tests look for them in this file.
const ACTION_CHECKIN: &str = "person.checkin";
const ACTION_UPDATE: &str = "person.update";
const ACTION_BIOMETRICS: &str = "person.biometrics";

mod domain;
mod http;
mod model;
mod persist;
mod view;

#[cfg(test)]
mod tests;

pub use domain::drop_thumbnail_if_identified;
pub use http::{get_person, list_people, needs_selfie, patch_person, post_checkin, put_biometrics};
pub use model::*;
