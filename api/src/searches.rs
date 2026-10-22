//! Search for missing people (spec 3.7, 7, 8.7): `POST /v1/searches` and the photo pipeline that
//! the family link shares.
//!
//! One file per layer: `model` (the stored search and its inputs), `domain` (the rules and the
//! validation of the body, pure), `persist` (the quota, the Unico pipeline and the transaction),
//! `http` (the route) and `view` (the answer).

mod domain;
mod http;
mod model;
mod persist;
mod view;

#[cfg(test)]
mod tests;

pub use domain::parse_input;
pub use http::post_search;
pub use model::*;
pub use persist::{register_photos, take_daily_slot};
pub use view::answer;
