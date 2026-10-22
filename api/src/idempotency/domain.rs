//! The rules on a key and its claim: which requests are considered, what a claim looks like
//! and when it lapsed or is refused. Plain functions over the documents, nothing `async`.

use axum::http::{HeaderMap, Method, StatusCode};
use firestore::FirestoreInstant;
use sha2::{Digest, Sha256};

use super::model::{Asker, ClaimState, IdempotencyDoc, LEASE, TTL};
use crate::error::ApiError;
use crate::store;

pub(super) const MAX_KEY_CHARS: usize = 128;

impl IdempotencyDoc {
    /// A claim whose lease ran out without a receipt.
    pub(super) fn lapsed(&self, now: FirestoreInstant) -> bool {
        self.state == ClaimState::Pending && self.lease_until.is_none_or(|until| until <= now)
    }

    /// The refusal for a request that is not the one this record belongs to.
    pub(super) fn refusal(&self, asker: &Asker) -> Option<ApiError> {
        if self.volunteer_id.as_deref() != Some(asker.volunteer_id.as_str())
            || self.route != asker.route
        {
            return Some(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "idempotency_key_mismatch",
            ));
        }
        (!self.fingerprint.is_empty() && self.fingerprint != asker.fingerprint)
            .then(|| ApiError::new(StatusCode::CONFLICT, "idempotency_key_reused"))
    }
}

pub(super) fn is_write(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

pub(super) fn key_of(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

pub(super) fn usable_key(key: &str) -> bool {
    key.chars().count() <= MAX_KEY_CHARS && !key.contains('/') && !key.contains('~')
}

/// The SHA-256 of the parts, in hex. Each part is length-prefixed, so ("ab", "c") and ("a", "bc")
/// differ.
pub fn digest_of(parts: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part);
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// `POST /v1/matches/{id}/decision`, whose answer carries the contact of the family.
pub(super) fn is_match_decision(route: &str) -> bool {
    route.starts_with("POST /v1/matches/") && route.ends_with("/decision")
}

pub(super) fn pending_for(asker: &Asker, attempts: u32) -> IdempotencyDoc {
    IdempotencyDoc {
        volunteer_id: Some(asker.volunteer_id.clone()),
        route: asker.route.clone(),
        fingerprint: asker.fingerprint.clone(),
        state: ClaimState::Pending,
        attempts,
        lease_until: Some(store::now() + LEASE),
        http_status: 0,
        body: String::new(),
        expires_at: store::now() + TTL,
    }
}
