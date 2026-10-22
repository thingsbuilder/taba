//! Children (spec 3.4.1, 4.9): guardians, who may change `child_status`, and the
//! `child_separated` alert.
//!
//! One file per layer under `children/`: `model` (what a write asks), `domain` (the rules as
//! pure functions), `persist` (the alert, added to the transaction of the write) and `http`
//! (the guardian route). There is no `view`: the one JSON this module shows is the guardian
//! list, built inside the handler.

mod domain;
mod http;
mod model;
mod persist;

#[cfg(test)]
mod tests;

pub use domain::{can_change_status, change_child_state, merge_child_state};
pub use http::post_guardian;
pub use model::ChildChange;
pub use persist::raise_child_separated;
