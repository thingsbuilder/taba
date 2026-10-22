//! What the module keeps: the claim on an `Idempotency-Key`, the asker it belongs to and the
//! answer of a paid photo call kept under the key.

use firestore::FirestoreInstant;
use firestore::jiff::SignedDuration;
use serde::{Deserialize, Serialize};

pub const COLLECTION: &str = "idempotency";
pub(super) const TTL: SignedDuration = SignedDuration::from_hours(24 * 7);
/// How long a claim is held. A request that runs longer than this can be claimed again by a
/// copy sent meanwhile (the slowest write, a photo through Unico, takes seconds).
pub(super) const LEASE: SignedDuration = SignedDuration::from_secs(60);
/// Set by `layer` on the requests it serves (a value sent by the client is dropped): the digest
/// of the request, which `photo_person_id` binds the paid answer to.
pub(super) const DIGEST_HEADER: &str = "x-taba-request-digest";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimState {
    Pending,
    Done,
}

fn done() -> ClaimState {
    ClaimState::Done
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdempotencyDoc {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volunteer_id: Option<String>,
    pub route: String,
    /// Digest of method, path, query and body. Empty on records from before the claim existed.
    #[serde(default)]
    pub fingerprint: String,
    /// Records from before the claim existed have no state: they are answers.
    #[serde(default = "done")]
    pub state: ClaimState,
    /// How many times the key was claimed; more than one means a claim lapsed without a receipt.
    #[serde(default)]
    pub attempts: u32,
    /// While `pending`, the claim holds until here.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub lease_until: Option<FirestoreInstant>,
    #[serde(default)]
    pub http_status: u16,
    /// The JSON text of the response body.
    #[serde(default)]
    pub body: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub expires_at: FirestoreInstant,
}

/// Who is asking, and what.
#[derive(Clone)]
pub(super) struct Asker {
    pub(super) volunteer_id: String,
    pub(super) route: String,
    pub(super) fingerprint: String,
}

/// What a paid call to Unico about a photo said, kept for the key of the request that made it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PhotoResult {
    pub(super) volunteer_id: String,
    pub(super) route: String,
    /// What the answer is bound to: a digest of the request (or of the photo). A key sent again
    /// with other content gets no answer from here.
    #[serde(default)]
    pub(super) digest: String,
    /// The HMAC of the token, never the token.
    pub(super) person_id: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub(super) expires_at: FirestoreInstant,
}
