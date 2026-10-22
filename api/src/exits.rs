//! Exits and occupancy (spec 3.8, 4.2): `POST /v1/shelters/{id}/exits` (adults, children,
//! exceptions) and "N of capacity" from the open stays.
//!
//! One file per layer under `exits/`: `model` (the request, the plan and what the transaction
//! carries), `domain` (who may exit and how, as pure functions), `persist` (the occupancy count
//! and the transactions) and `http` (the route). There is no `view`: the answer is three
//! fields built inside the handler. `fixtures` is test support the other modules use.

mod domain;
mod http;
mod model;
mod persist;

#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod tests;

pub use domain::vacancies;
pub use http::post_exit;
pub use persist::occupancy;
