//! Global roles granted from the app (spec 2.1, 8.4): `mediator` and `protection`, by a
//! validator or the Civil Defense. `validator` and `civil_defense` stay with `taba role`.
//!
//! Three layers, one file each: `domain` (which roles the app may change), `persist` (the
//! transaction that changes the volunteer and records it) and `http` (the routes). There is
//! no `model` (the volunteer is stored by `volunteers`) and no `view` (the routes answer
//! with an empty body).

mod domain;
mod http;
mod persist;

#[cfg(test)]
mod tests;

pub use http::{delete_role, post_role};
