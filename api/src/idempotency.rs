//! Server-side idempotency (spec 8, 9.4): a write sent again with the same `Idempotency-Key`
//! gets the response of the first one, and nothing runs twice. The app uses the id of the
//! queue item as the key, so a retry after a lost response is harmless.
//!
//! The key is claimed before the handler runs, by creating `idempotency/{key}` as `pending`
//! (a transaction, so it works across instances). The claim carries who asked, the route and
//! a digest of method, path, query and body. Then:
//! - the same request while the claim is `pending` waits for it and gets its answer, or
//!   `503 idempotency_in_progress` (retryable) if it takes longer than `WAIT`;
//! - the same key for another volunteer or route is `422 idempotency_key_mismatch`, and for
//!   the same route with another payload `409 idempotency_key_reused`; neither sees an answer
//!   that is not theirs;
//! - a `2xx` answer is stored (`done`) and replayed for a week;
//! - a refusal releases the claim: it can have a different answer when the app sends the item
//!   again with an explanation, and it changed nothing the second run could repeat.
//!
//! This is not exactly-once. A process that dies after the handler committed and before the
//! receipt is stored leaves the claim `pending`; once its lease lapses the next copy claims it
//! again (`attempts` counts the claims) and the handler runs a second time. The handlers are
//! idempotent by the ids the app sends, except the paid call to Unico, which `photo_person_id`
//! keeps under the key. Keys apply to signed-in writes only: the public routes (login, invite
//! acceptance) are protected by their single-use captures.
//!
//! Layers, one file each: `model` (the claim and the answer kept under a key), `domain` (the
//! rules over them), `persist` (the claim and the kept photo answers in Firestore) and `http`
//! (the middleware). It has no `view`: the answers it replays are the ones the handlers wrote.
//! No audit action is written here.

mod domain;
mod http;
mod model;
mod persist;

#[cfg(test)]
mod tests;

pub use domain::digest_of;
pub use http::layer;
pub use model::COLLECTION;
pub use persist::{photo_person_id, photo_person_id_of};
