//! The family's search link (spec 3.7 item 11, 7, 10.4): a one-use link of 12 hours that the
//! volunteer shares; the family fills the search in a public page.
//!
//! The two public routes carry no session. What protects them: the code (128 bits), one use,
//! 12 hours, at most `MAX_ATTEMPTS` failed submissions per link, a per-process limit per device
//! and per IP, the 6 MB body limit of the router and the 800 KB limit per image (both refused
//! before Unico is called). Nothing the family sends is logged.
//!
//! The submit claims the link in a transaction (a lease of `LEASE`), runs the search pipeline
//! (`searches::register_photos`, owned by the search module, which writes its own transaction)
//! and then marks the link `used` together with the audit entry and the alert. Of N concurrent
//! submits exactly one gets the lease; the others see `contention` (while the first runs) or
//! `search_link_invalid` (after it finished). A failure releases the lease; `unico_degraded`
//! does not count as an attempt, so the link stays valid for a later try.
//!
//! Photos are not paid twice: the search id and `SearchOwner::key` are derived from the link
//! code, so a retry after a failure reuses what Unico already answered, with no session.
//!
//! One file per layer: `model` (the stored link), `domain` (the code, the derived search id and
//! the rate limit), `persist` (the claim, the consumption and the release) and `http` (the routes).

mod domain;
mod http;
mod model;
mod persist;

#[cfg(test)]
mod tests;

pub use http::{delete_search_link, get_search_link, post_search_link, submit_search_link};
pub use model::*;
