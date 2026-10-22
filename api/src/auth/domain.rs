//! The rules of a login (spec 3.1, 5.2, 6.3.2): how ids and secrets are made, when a capture
//! has ended, who may sign in and what a session is. Nothing here is `async` or reads the
//! database.

use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use firestore::FirestoreInstant;
use firestore::jiff::SignedDuration;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

use super::model::{CaptureDoc, CaptureStatus, SessionDoc};
use crate::error::ApiError;
use crate::volunteers::{ProfileInput, VolunteerDoc};

pub(super) const CAPTURE_TTL: SignedDuration = SignedDuration::from_mins(15);
pub(super) const MAX_LOGIN_ATTEMPTS: u32 = 10;
pub(super) const RENEW_AFTER: SignedDuration = SignedDuration::from_hours(24);

/// `volunteer_id = base64url(HMAC-SHA256(HMAC_KEY, token))`, 43 characters (spec 6.3.2).
/// The token itself is never stored or returned.
pub fn derive_id(key: &[u8], token: &str) -> String {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(token.as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

/// `sessions/{sha256(secret)}`: only the hash is stored.
pub fn session_id(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn new_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn device_id(headers: &HeaderMap) -> Result<String, ApiError> {
    headers
        .get("x-device-id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.len() <= 64)
        .map(str::to_string)
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "device_id_required"))
}

pub fn days(n: u32) -> SignedDuration {
    SignedDuration::from_hours(24 * i64::from(n))
}

pub(super) fn terminal_error(status: CaptureStatus) -> Option<ApiError> {
    let code = match status {
        CaptureStatus::Inconclusive => "capture_inconclusive",
        CaptureStatus::Failed => "capture_failed",
        CaptureStatus::Canceled => "capture_canceled",
        CaptureStatus::Created | CaptureStatus::Ok => return None,
    };
    Some(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code))
}

/// The volunteer a login or an invite acceptance signs in: the existing profile, or a new one
/// built from `profile`. A banned volunteer is refused; so is a new one without a profile.
/// The flag says whether the volunteer is new.
pub fn volunteer_for_login(
    existing: Option<VolunteerDoc>,
    profile: Option<&ProfileInput>,
    now: FirestoreInstant,
) -> Result<(VolunteerDoc, bool), ApiError> {
    let is_new = existing.is_none();
    let mut volunteer = match (existing, profile) {
        (Some(v), _) if v.banned => return Err(ApiError::new(StatusCode::FORBIDDEN, "banned")),
        (Some(v), _) => v,
        (None, Some(profile)) => VolunteerDoc::from_profile(profile)?,
        (None, None) => {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "profile_required",
            ));
        }
    };
    volunteer.last_seen_at = now;
    Ok((volunteer, is_new))
}

pub fn new_session(
    volunteer_id: &str,
    device_id: &str,
    session_days: u32,
    now: FirestoreInstant,
) -> SessionDoc {
    SessionDoc {
        volunteer_id: volunteer_id.into(),
        device_id: device_id.into(),
        created_at: now,
        renewed_at: now,
        expires_at: now + days(session_days),
    }
}

/// The capture marked as used by `person_id`, for the transaction that consumes it.
pub fn consumed(capture: CaptureDoc, person_id: &str) -> CaptureDoc {
    CaptureDoc {
        consumed: true,
        status: CaptureStatus::Ok,
        person_id: Some(person_id.into()),
        ..capture
    }
}
