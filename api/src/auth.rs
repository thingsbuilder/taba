//! Login captures, sessions and the `Volunteer` extractor (spec 3.1, 5.2, 6.2.5, 6.3.2, 8.1).
//!
//! Five layers, one file each: `model` (the stored capture and session, the `Volunteer` and the
//! `AppState`), `domain` (ids, secrets, the capture's end and who may sign in, as pure
//! functions), `persist` (Unico's answer, the login transaction and the session lookup),
//! `http` (the routes and the extractor) and `view` (the JSON shown).

// The audit actions stay here: the conformance tests look for them in this file.
const ACTION_CAPTURE_CREATE: &str = "capture.create";
const ACTION_VOLUNTEER_CREATE: &str = "volunteer.create";
const ACTION_SESSION_CREATE: &str = "session.create";
const ACTION_SESSION_DELETE: &str = "session.delete";

mod domain;
mod http;
mod model;
mod persist;
mod view;

#[cfg(test)]
mod tests;

// `derive_id` is only used by the unit tests of other modules.
#[cfg(test)]
pub use domain::derive_id;
pub use domain::{consumed, device_id, new_secret, new_session, session_id, volunteer_for_login};
pub use http::{delete_current_session, post_captures, post_sessions};
pub use model::*;
pub use persist::{remember_result, resolve_person, usable_capture};
