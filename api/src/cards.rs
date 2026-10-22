//! Cards of a household (spec 3.5.1, 7, 8.5).
//!
//! A card belongs to a household and works in any shelter of the network, so reading one needs
//! only a membership; changing one needs the household's shelter (see `households`). The code
//! is made on the phone: 8 characters of Crockford base32, uppercase. `cards/{code}` is created
//! with a precondition that it does not exist, so a code is never reused, not even after a cancel.
//!
//! One file per layer under `cards/`: `model` (the stored document and the request bodies),
//! `domain` (the code and the cancel, as pure functions), `persist` (reads), `http` (the routes,
//! which hold their transactions) and `view` (the JSON shown).

mod domain;
mod http;
mod model;
mod persist;
mod view;

#[cfg(test)]
mod tests;

pub use domain::crockford;
pub use http::{cancel_card, get_card, post_card};
pub use model::*;
