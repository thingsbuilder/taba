//! Mediation of a search (spec 4.8, 7, 8.7): the three queues, the check and the decision.
//!
//! One file per layer: `model` (the stored match), `domain` (the rules as pure functions),
//! `persist` (the queries and the audit transaction), `http` (the routes) and `view` (the JSON
//! shown). The audit actions are written where the route records them.

mod domain;
mod http;
mod model;
mod persist;
mod view;

#[cfg(test)]
mod tests;

pub use domain::queue_for;
pub use http::{
    get_match, get_match_search_photo, get_match_thumbnail, list_matches, post_decision,
};
pub use model::*;
