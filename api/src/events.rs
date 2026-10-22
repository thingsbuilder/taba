//! Automatic events (spec 4.3): a shelter joins the active event within `EVENT_RADIUS_KM`, or
//! starts one. `PATCH /v1/events/{id}` renames it and sets the reference contacts (spec 8.2).
//!
//! Five layers, one file each: `model` (the stored document), `domain` (the name and the patch
//! as pure functions), `persist` (the assignment of a shelter, the gate on closed events and the
//! activity record), `http` (the route) and `view` (the JSON shown).

// The audit action stays here: the conformance tests look for it in this file.
const ACTION_UPDATE: &str = "event.update";

mod domain;
mod http;
mod model;
mod persist;
mod view;

#[cfg(test)]
mod tests;

pub use http::patch_event;
pub use model::*;
pub use persist::{assign, ensure_open, ensure_shelter_open, touch};
