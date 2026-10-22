//! The Civil Defense dashboard (spec 3.10, 8.8): `GET /v1/dashboard/events`,
//! `GET /v1/dashboard/events/{id}` and `GET /v1/dashboard/events/{id}/daily.csv`, for a validator
//! (read) and the Civil Defense.
//!
//! Every cut counts the people in a shelter now (`current_shelter_id`), which is what the fixed
//! notice says the panel counts; whoever already left shows in the exits by destination. The
//! event's totals are the sum of its shelters. Counts use the Firestore `count()` aggregation; the
//! two things an aggregation cannot do (the exits, which live inside the `stays` array, and the
//! animals, which live inside the households) are bounded scans.
//!
//! Every bounded read fetches one row past its limit. When that extra row shows up, the read was
//! cut: the answer says so (`complete: false`, with the cut indicators in `partial_indicators`)
//! instead of passing a subset off as the total. `generated_at` is when the answer was built.
//!
//! Layers, one file each: `model` (the answers and the rows they are built from), `domain` (the
//! cuts and the small pure helpers), `persist` (the counts, scans and caches that read Firestore),
//! `http` (the routes) and `view` (the CSV shown). The panel only reads, so there is no audit
//! action here.

mod domain;
mod http;
mod model;
mod persist;
mod view;

#[cfg(test)]
mod tests;

pub(crate) use domain::count_value;
pub use http::{daily_csv, get_event, list_events};

// The paths other modules and the tests reach the panel by.
#[allow(unused_imports)]
pub use model::*;
