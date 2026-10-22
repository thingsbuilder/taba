//! Alerts (spec 7, 8.8): `GET /v1/alerts` and `POST /v1/alerts/{id}/resolve`.
//! Phase 2 raises `shelter_flagged`, `event_overlap`, `volunteer_removed` and `endorser_flagged`;
//! phase 4 adds `search_expiring` and `family_search_received`, which only their volunteer sees.
//!
//! One file per layer: `model` (the stored alert), `domain` (who receives it, as pure functions),
//! `persist` (the queries and `add`), `http` (the routes) and `view` (the JSON shown).

mod domain;
mod http;
mod model;
mod persist;
mod view;

#[cfg(test)]
mod tests;

pub use http::{get_alerts, resolve_alert};
pub use model::*;
pub use persist::add;
