//! Global roles and shelter memberships (spec 2.1, 2.3, 3.3, 4.4). The operator's side of the
//! roles is `taba role grant|revoke` for `validator` and `civil_defense`; `mediator` and
//! `protection` are granted through the app from phase 4. Members, removal, leaving and
//! banning are the routes of spec 8.4.
//!
//! Five layers, one file each: `model` (the stored membership and its vocabulary), `domain`
//! (the rules as pure functions over the documents), `persist` (queries and the transactions
//! that read, apply the rules and write), `http` (the routes) and `view` (the JSON shown).

// The audit actions stay here: the conformance tests look for them in this file.
const ACTION_PROMOTE: &str = "member.promote";
const ACTION_REMOVE: &str = "member.remove";
const ACTION_LEAVE: &str = "member.leave";
const ACTION_BAN: &str = "volunteer.ban";

mod domain;
mod http;
mod model;
mod persist;
mod view;

#[cfg(test)]
mod tests;

pub use domain::parse_cli_role;
pub use http::{ban_volunteer, leave_shelter, list_members, promote_member, remove_member};
pub use model::*;
pub use persist::{
    active_in_shelter, active_of_volunteer, change_role, memberships_of, touch_shelter_event,
};
