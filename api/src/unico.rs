//! Unico Human Token client (spec 5.1-5.6). The only module that talks to the Unico API
//! and the only one that reads `data.humanToken.value` (`extract_token`, spec 5.4).
//!
//! There is no sandbox and every call is billed: automated tests point `UNICO_URL` at a fake.
//!
//! Layers, one file each: `model` (journeys, results and the failures), `domain` (reading an
//! answer, mapping a failure to an API error, the backoff, describing a refused image) and
//! `client` (the calls, the permits, the retries and the degraded mode). `client` takes the
//! place of `persist` and `http`: this module reads no collection and serves no route, it only
//! calls out. There is no `view`, and no audit action is written here.

mod client;
mod domain;
mod model;

#[cfg(test)]
mod tests;

pub use client::Unico;
pub use model::*;
