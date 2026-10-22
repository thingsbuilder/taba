//! Deliveries (spec 3.5, 4.7, 7, 8.6): `POST /v1/shelters/{id}/deliveries`, and the admin side of
//! them (`PUT /v1/shelters/{id}/items`, `GET /v1/shelters/{id}/deliveries?status=` and
//! `POST /v1/deliveries/{id}/review`, re-exported by `deliveries_admin`).
//!
//! Five layers, one file each: `model` (the stored document and its types), `domain` (the rules as
//! pure functions), `persist` (the reads and the transaction that records a delivery), `http` (the
//! routes) and `view` (the JSON shown).

// The audit actions stay here: the conformance tests look for them in this file. The action of
// a recorded delivery is built in `persist` as `delivery.{}{}{}`.
const ACTION_ITEMS: &str = "shelter.items";
const ACTION_REVIEW: &str = "delivery.review";
const ACTION_REVALIDATION_FAILED: &str = "delivery.revalidation_failed";

mod domain;
mod http;
mod model;
mod persist;
mod view;

#[cfg(test)]
mod tests;

pub use http::{list_deliveries, post_delivery, put_items, review_delivery};
pub use model::*;
