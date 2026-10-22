//! Audit log (spec 4.10, 7): every write from phase 1 on records an `audit_log` entry.
//! Entries carry ids only, never a name or a photo. `expires_at` is one year ahead (the TTL
//! policy is in `firestore.indexes.json`). The informational anomaly alerts of spec 4.10 follow.
//!
//! Layers, one file each: `model` (the entry, the legal basis and the activities), `domain`
//! (what the anomaly rules remember and decide, plain code) and `persist` (the entry in the
//! transaction and the alerts it raises). It has no routes and shows no JSON, so no `http` or
//! `view`. The actions are written by the modules that own the writes, not here.

mod domain;
mod model;
mod persist;

#[cfg(test)]
mod tests;

pub use model::*;
pub use persist::{add, note_activity};
