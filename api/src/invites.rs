//! Invites to a shelter (spec 3.3, 8.4): a QR code for someone present (30 min, admin or
//! volunteer) or a link for someone remote (24 h, volunteer only). Single use, made on the
//! server, and always accepted with a liveness capture (`purpose = invite`).
//!
//! Five layers, one file each: `model` (the stored invite), `domain` (when it is valid, who it
//! admits and how it is spent, as pure functions), `persist` (queries and the transactions),
//! `http` (the routes) and `view` (the JSON shown).

// The audit actions stay here: the conformance tests look for them in this file.
const ACTION_CREATE: &str = "invite.create";
const ACTION_CANCEL: &str = "invite.cancel";
const ACTION_ACCEPT: &str = "invite.accept";

mod domain;
mod http;
mod model;
mod persist;
mod view;

#[cfg(test)]
mod tests;

pub use http::{accept_invite, cancel_invite, create_invite, get_invite, list_invites};
pub use model::*;
