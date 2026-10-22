//! Households (spec 3.5.1, 7, 8.5).
//!
//! A person is in at most one household: `people.household_id` is the single source of truth,
//! read and written in the same transaction as `households.member_ids` (spec 6.2.4). Reading a
//! household shows names and levels only, never the shelter a member is in nor their history
//! (spec 10.3, criterion 35).
//!
//! One file per layer under `households/`: `model` (the stored document and the request
//! bodies), `domain` (validation and who may act, as pure functions), `persist` (reads),
//! `http` (the routes, which hold their transactions) and `view` (the JSON shown).

mod domain;
mod http;
mod model;
mod persist;
mod view;

#[cfg(test)]
pub(crate) mod tests;

pub use domain::shelter_of_access;
pub use http::{get_household, patch_household, post_household};
pub use model::*;
pub use persist::{load_household, load_members, touch_event};
pub use view::members_view;
