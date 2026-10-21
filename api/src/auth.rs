//! Login captures, sessions and the `Volunteer` extractor (spec 3.1, 5.2, 6.2.5, 6.3.2, 8.1).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, bail};
use axum::Json;
use axum::extract::{FromRef, FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use firestore::errors::BackoffError;
use firestore::jiff::SignedDuration;
use firestore::{
    FirestoreDb, FirestoreInstant, FirestoreTransaction, FirestoreWritePrecondition, paths,
};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::audit::{self, AuditEntry, LegalBasis};
use crate::config::Config;
use crate::error::ApiError;
use crate::members::{self, ShelterRole};
use crate::shelters::Confirmation;
use crate::store::{self, fs_error, tx_create, tx_replace};
use crate::unico::{ProcessResult, Unico};
use crate::volunteers::{self, GlobalRole, ProfileInput, VolunteerDoc};

const CAPTURES: &str = "captures";
pub const SESSIONS: &str = "sessions";
const CAPTURE_TTL: SignedDuration = SignedDuration::from_mins(15);
const MAX_LOGIN_ATTEMPTS: u32 = 10;
const RENEW_AFTER: SignedDuration = SignedDuration::from_hours(24);

#[derive(Clone)]
pub struct AppState {
    pub db: FirestoreDb,
    pub config: Arc<Config>,
    pub unico: Arc<Unico>,
    hmac_key: Arc<[u8]>,
    thumbnail_key: Arc<[u8]>,
    domain: Arc<str>,
    /// Serializes shelter creation (spec 6.2.4).
    pub shelter_lock: Arc<tokio::sync::Mutex<()>>,
    /// When each event's `last_activity_at` was last written (spec 4.3.3).
    pub event_activity: Arc<Mutex<HashMap<String, Instant>>>,
    /// One lock per `Idempotency-Key` being served, so two copies of a request run one after the other.
    pub idempotency_locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl FromRef<AppState> for FirestoreDb {
    fn from_ref(state: &AppState) -> Self {
        state.db.clone()
    }
}

impl AppState {
    /// Fails at startup when what login needs is not configured (spec 6.2.2).
    pub fn new(db: FirestoreDb, config: Config, unico: Unico) -> anyhow::Result<Self> {
        let Some(domain) = config.domain.clone() else {
            bail!("DOMAIN is required to serve");
        };
        let key = config
            .hmac_key
            .as_ref()
            .context("HMAC_KEY is required to serve")?;
        let hmac_key = STANDARD
            .decode(key.0.trim())
            .context("HMAC_KEY is not valid base64")?;
        if hmac_key.len() != 32 {
            bail!("HMAC_KEY must be 32 bytes in base64");
        }
        let thumbnail_key = STANDARD
            .decode(
                config
                    .thumbnail_key
                    .as_ref()
                    .context("THUMBNAIL_KEY is required to serve")?
                    .0
                    .trim(),
            )
            .context("THUMBNAIL_KEY is not valid base64")?;
        if thumbnail_key.len() != 32 {
            bail!("THUMBNAIL_KEY must be 32 bytes in base64");
        }
        let status_db = db.clone();
        Ok(Self {
            db,
            config: Arc::new(config),
            // Every call obeys and feeds the degraded mode (spec 5.5, 5.7).
            unico: Arc::new(unico.with_status(status_db)),
            hmac_key: hmac_key.into(),
            thumbnail_key: thumbnail_key.into(),
            domain: domain.into(),
            shelter_lock: Arc::default(),
            event_activity: Arc::default(),
            idempotency_locks: Arc::default(),
        })
    }

    /// The AES-256-GCM key of the thumbnails (spec 4.6); used by `thumbnail.rs`.
    pub fn thumbnail_key(&self) -> &[u8] {
        &self.thumbnail_key
    }

    /// The `people` id of a Human Token: its HMAC (spec 6.3.2); the token is not kept.
    pub fn person_id_for_token(&self, token: &str) -> String {
        derive_id(&self.hmac_key, token)
    }

    pub fn domain(&self) -> &str {
        &self.domain
    }
}

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

// ---- documents ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Login,
    Invite,
    Checkin,
    Revalidation,
    Biometrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    Created,
    Ok,
    Inconclusive,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureDoc {
    pub process_id: String,
    pub purpose: Purpose,
    pub device_id: String,
    pub status: CaptureStatus,
    /// HMAC of the token; the token itself is never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person_id: Option<String>,
    /// Who asked for the capture (session purposes only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volunteer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shelter_id: Option<String>,
    /// The person a revalidation must turn out to be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_person_id: Option<String>,
    pub consumed: bool,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub expires_at: FirestoreInstant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDoc {
    pub volunteer_id: String,
    pub device_id: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub renewed_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub expires_at: FirestoreInstant,
}

pub fn days(n: u32) -> SignedDuration {
    SignedDuration::from_hours(24 * i64::from(n))
}

// ---- POST /v1/captures ----

#[derive(Debug, Deserialize)]
pub struct CreateCapture {
    purpose: Purpose,
    #[serde(default)]
    shelter_id: Option<String>,
    #[serde(default)]
    expected_person_id: Option<String>,
}

pub async fn post_captures(
    State(state): State<AppState>,
    headers: HeaderMap,
    volunteer: Option<Volunteer>,
    Json(request): Json<CreateCapture>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let device = device_id(&headers)?;
    // `login` and `invite` are public (spec 10.4). The captures of phase 3 need a session and say
    // what they are for, so a capture can only be used for that shelter and that person:
    // `checkin` for a shelter the caller belongs to, `biometrics` for one person, `revalidation`
    // for one person in a shelter.
    let (volunteer_id, shelter_id, expected_person_id) = match request.purpose {
        Purpose::Login | Purpose::Invite => (None, None, None),
        purpose => {
            let volunteer = volunteer.ok_or_else(unauthorized)?;
            let target_required =
                || ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "capture_target_required");
            let needs_shelter = matches!(purpose, Purpose::Checkin | Purpose::Revalidation);
            let needs_person = matches!(purpose, Purpose::Biometrics | Purpose::Revalidation);
            if (needs_shelter && request.shelter_id.is_none())
                || (needs_person && request.expected_person_id.is_none())
            {
                return Err(target_required());
            }
            if let Some(shelter_id) = &request.shelter_id {
                let may = volunteer.role_in(shelter_id).is_some()
                    || (purpose == Purpose::Revalidation
                        && (volunteer.has_global(GlobalRole::CivilDefense)
                            || volunteer.has_global(GlobalRole::Protection)));
                if !may {
                    return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden"));
                }
            }
            (
                Some(volunteer.id),
                request.shelter_id,
                request.expected_person_id,
            )
        }
    };
    let capture_id = uuid::Uuid::new_v4().to_string();
    let callback = format!("https://{}/return?capture={capture_id}", state.domain);
    let journey = state.unico.create_journey(&callback).await?;

    let now = crate::store::now();
    let doc = CaptureDoc {
        process_id: journey.process_id,
        purpose: request.purpose,
        device_id: device.clone(),
        status: CaptureStatus::Created,
        person_id: None,
        volunteer_id,
        shelter_id,
        expected_person_id,
        consumed: false,
        created_at: now,
        expires_at: now + CAPTURE_TTL,
    };
    let entry = AuditEntry::new("capture.create", "capture", &capture_id)
        .on_device(&device)
        .basis(LegalBasis::FraudPrevention);
    state
        .db
        .run_transaction(|db, tx| {
            let (id, doc, entry) = (capture_id.clone(), doc.clone(), entry.clone());
            Box::pin(async move {
                let mut write = || -> Result<(), ApiError> {
                    tx_create(&db, tx, CAPTURES, &id, &doc)?;
                    audit::add(&db, tx, &entry)
                };
                write().map_err(BackoffError::permanent)
            })
        })
        .await
        .map_err(fs_error)?;

    Ok((
        StatusCode::CREATED,
        Json(
            json!({ "capture_id": capture_id, "link": journey.link, "expires_at": doc.expires_at.to_string() }),
        ),
    ))
}

// ---- POST /v1/sessions ----

#[derive(Debug, Deserialize)]
pub struct CreateSession {
    capture_id: String,
    #[serde(default)]
    profile: Option<ProfileInput>,
}

fn terminal_error(status: CaptureStatus) -> Option<ApiError> {
    let code = match status {
        CaptureStatus::Inconclusive => "capture_inconclusive",
        CaptureStatus::Failed => "capture_failed",
        CaptureStatus::Canceled => "capture_canceled",
        CaptureStatus::Created | CaptureStatus::Ok => return None,
    };
    Some(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code))
}

/// Gets the person behind a capture, asking Unico only while the capture is unresolved.
pub async fn resolve_person(
    state: &AppState,
    capture_id: &str,
    capture: &mut CaptureDoc,
) -> Result<String, ApiError> {
    if let Some(error) = terminal_error(capture.status) {
        return Err(error);
    }
    if let (CaptureStatus::Ok, Some(person_id)) = (capture.status, &capture.person_id) {
        return Ok(person_id.clone());
    }
    let (status, person_id) = match state.unico.get_process(&capture.process_id).await? {
        ProcessResult::Pending => {
            return Err(ApiError::new(StatusCode::ACCEPTED, "capture_pending"));
        }
        ProcessResult::Token(token) => {
            (CaptureStatus::Ok, Some(derive_id(&state.hmac_key, &token)))
        }
        ProcessResult::Inconclusive => (CaptureStatus::Inconclusive, None),
        ProcessResult::Failed => (CaptureStatus::Failed, None),
        ProcessResult::Canceled => (CaptureStatus::Canceled, None),
    };
    capture.status = status;
    capture.person_id = person_id.clone();
    // A token is not written here: the login transaction records it together with `consumed`.
    if status == CaptureStatus::Ok {
        return person_id.ok_or_else(ApiError::internal);
    }
    remember_result(state, capture_id, capture).await?;
    Err(terminal_error(status).unwrap_or_else(ApiError::internal))
}

/// Saves what Unico said about the capture (only `status` and `person_id`: a whole-document
/// write could undo a concurrent `consumed = true`).
pub async fn remember_result(
    state: &AppState,
    capture_id: &str,
    capture: &CaptureDoc,
) -> Result<(), ApiError> {
    state
        .db
        .fluent()
        .update()
        .fields(paths!(CaptureDoc::{status, person_id}))
        .in_col(CAPTURES)
        .document_id(capture_id)
        .object(capture)
        .execute::<()>()
        .await
        .map_err(fs_error)
}

#[derive(Clone)]
struct Login {
    capture_id: String,
    device_id: String,
    person_id: String,
    profile: Option<ProfileInput>,
    secret: String,
    session_days: u32,
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

/// The capture as it stands before Unico is asked: it must exist, belong to this device and
/// purpose, and be unused and unexpired (spec 5.2.4).
pub async fn usable_capture(
    state: &AppState,
    device: &str,
    capture_id: &str,
    purpose: Purpose,
) -> Result<CaptureDoc, ApiError> {
    let capture: CaptureDoc = store::get(&state.db, CAPTURES, capture_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "capture_not_found"))?;
    if capture.device_id != device {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "capture_wrong_device"));
    }
    if capture.purpose != purpose {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "wrong_capture_purpose",
        ));
    }
    if capture.consumed {
        return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
    }
    if crate::store::now() >= capture.expires_at {
        return Err(ApiError::new(StatusCode::GONE, "capture_expired"));
    }
    Ok(capture)
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

/// One transaction: consume the capture, create or load the volunteer, open the session.
async fn login(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    a: &Login,
) -> Result<(VolunteerDoc, SessionDoc), ApiError> {
    let capture: CaptureDoc = store::get(db, CAPTURES, &a.capture_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "capture_not_found"))?;
    if capture.consumed {
        return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
    }
    if capture.purpose != Purpose::Login {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "wrong_capture_purpose",
        ));
    }
    let existing: Option<VolunteerDoc> =
        store::get(db, volunteers::COLLECTION, &a.person_id).await?;
    let now = crate::store::now();
    let (volunteer, is_new) = volunteer_for_login(existing, a.profile.as_ref(), now)?;
    let session = new_session(&a.person_id, &a.device_id, a.session_days, now);

    tx_replace(db, tx, volunteers::COLLECTION, &a.person_id, &volunteer)?;
    tx_replace(
        db,
        tx,
        CAPTURES,
        &a.capture_id,
        &consumed(capture, &a.person_id),
    )?;
    tx_create(db, tx, SESSIONS, &session_id(&a.secret), &session)?;
    let audit_entry = |action: &str, kind: &str, target: &str| {
        AuditEntry::new(action, kind, target)
            .by(&a.person_id)
            .on_device(&a.device_id)
            .basis(LegalBasis::FraudPrevention)
    };
    if is_new {
        audit::add(
            db,
            tx,
            &audit_entry("volunteer.create", "volunteer", &a.person_id),
        )?;
    }
    audit::add(
        db,
        tx,
        &audit_entry("session.create", "session", &session_id(&a.secret)),
    )?;
    Ok((volunteer, session))
}

pub async fn post_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateSession>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let device = device_id(&headers)?;
    let mut capture = usable_capture(&state, &device, &request.capture_id, Purpose::Login).await?;
    let person_id = resolve_person(&state, &request.capture_id, &mut capture).await?;

    let secret = new_secret();
    let args = Login {
        capture_id: request.capture_id,
        device_id: device,
        person_id: person_id.clone(),
        profile: request.profile,
        secret: secret.clone(),
        session_days: state.config.session_days,
    };
    // Under contention Firestore aborts the transactions that lose the lock. The capture is
    // single use, so each loser looks again: consumed by the winner means 409, otherwise retry.
    let mut attempts = 0;
    let (volunteer, session) = loop {
        let logged_in = state
            .db
            .run_transaction(|db, tx| {
                let args = args.clone();
                Box::pin(
                    async move { login(&db, tx, &args).await.map_err(BackoffError::permanent) },
                )
            })
            .await
            .map_err(fs_error);
        match logged_in {
            Err(busy) if busy.code == store::CONTENTION => {
                let capture =
                    store::get::<CaptureDoc>(&state.db, CAPTURES, &args.capture_id).await?;
                if capture.is_some_and(|c| c.consumed) {
                    return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
                }
                attempts += 1;
                if attempts == MAX_LOGIN_ATTEMPTS {
                    return Err(busy);
                }
            }
            // A new volunteer still owes the profile: keep the result, Unico is not asked again.
            Err(e) if e.code == "profile_required" => {
                capture.status = CaptureStatus::Ok;
                capture.person_id = Some(person_id.clone());
                remember_result(&state, &args.capture_id, &capture).await?;
                return Err(e);
            }
            other => break other?,
        }
    };

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "session": secret,
            "expires_at": session.expires_at.to_string(),
            "volunteer": volunteers::view(&person_id, &volunteer),
        })),
    ))
}

// ---- DELETE /v1/sessions/current ----

pub async fn delete_current_session(
    State(state): State<AppState>,
    volunteer: Volunteer,
) -> Result<StatusCode, ApiError> {
    let entry =
        AuditEntry::new("session.delete", "session", &volunteer.session_id).by(&volunteer.id);
    state
        .db
        .run_transaction(|db, tx| {
            let (session_id, entry) = (volunteer.session_id.clone(), entry.clone());
            Box::pin(async move {
                let mut write = || -> Result<(), ApiError> {
                    store::tx_delete(&db, tx, SESSIONS, &session_id)?;
                    audit::add(&db, tx, &entry)
                };
                write().map_err(BackoffError::permanent)
            })
        })
        .await
        .map_err(fs_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- extractor ----

/// Shelter membership as `GET /v1/me` reports it (spec 8.1).
#[derive(Debug, Clone, Serialize)]
pub struct Membership {
    pub shelter_id: String,
    pub name: String,
    pub role: ShelterRole,
    pub confirmation: Confirmation,
    #[serde(skip)]
    pub event_id: Option<String>,
}

/// The authenticated volunteer: profile, global roles (inside `doc`) and active memberships.
#[derive(Debug, Clone)]
pub struct Volunteer {
    pub id: String,
    pub doc: VolunteerDoc,
    pub memberships: Vec<Membership>,
    pub session_id: String,
}

impl Volunteer {
    pub fn has_global(&self, role: GlobalRole) -> bool {
        self.doc.global_roles.contains(&role)
    }

    /// `validator` and `civil_defense` confirm and revoke shelters (spec 2.1).
    pub fn is_validator_or_civil_defense(&self) -> bool {
        self.has_global(GlobalRole::Validator) || self.has_global(GlobalRole::CivilDefense)
    }

    pub fn role_in(&self, shelter_id: &str) -> Option<ShelterRole> {
        self.memberships
            .iter()
            .find(|m| m.shelter_id == shelter_id)
            .map(|m| m.role)
    }

    pub fn is_admin_of(&self, shelter_id: &str) -> bool {
        self.role_in(shelter_id) == Some(ShelterRole::Admin)
    }

    pub fn has_admin_membership(&self) -> bool {
        self.memberships
            .iter()
            .any(|m| m.role == ShelterRole::Admin)
    }
}

fn unauthorized() -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized")
}

impl axum::extract::OptionalFromRequestParts<AppState> for Volunteer {
    type Rejection = ApiError;

    /// `None` without an `Authorization` header; a bad session is still an error.
    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Option<Self>, ApiError> {
        if !parts.headers.contains_key("authorization") {
            return Ok(None);
        }
        <Self as FromRequestParts<AppState>>::from_request_parts(parts, state)
            .await
            .map(Some)
    }
}

impl FromRequestParts<AppState> for Volunteer {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let secret = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(unauthorized)?;
        let session_id = session_id(secret);
        let mut session: SessionDoc = store::get(&state.db, SESSIONS, &session_id)
            .await?
            .ok_or_else(unauthorized)?;
        let now = crate::store::now();
        // The Firestore TTL deletes expired sessions some time after `expires_at`.
        if now >= session.expires_at {
            return Err(unauthorized());
        }
        let doc: VolunteerDoc =
            store::get(&state.db, volunteers::COLLECTION, &session.volunteer_id)
                .await?
                .ok_or_else(unauthorized)?;
        if doc.banned {
            return Err(ApiError::new(StatusCode::FORBIDDEN, "banned"));
        }

        if now.duration_since(session.renewed_at) > RENEW_AFTER {
            session.renewed_at = now;
            session.expires_at = now + days(state.config.session_days);
            // Only the two dates, and only if the session still exists: a logout must not be undone.
            let renewed = state
                .db
                .fluent()
                .update()
                .fields(paths!(SessionDoc::{renewed_at, expires_at}))
                .in_col(SESSIONS)
                .precondition(FirestoreWritePrecondition::Exists(true))
                .document_id(&session_id)
                .object(&session)
                .execute::<()>()
                .await;
            if let Err(err) = renewed {
                tracing::warn!(%err, "could not renew the session");
            }
        }
        let memberships = members::memberships_of(&state.db, &session.volunteer_id).await?;
        Ok(Volunteer {
            id: session.volunteer_id,
            doc,
            memberships,
            session_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestApp, all_docs, audit_actions};

    const DEVICE: &str = "device-a";
    const OTHER_DEVICE: &str = "device-b";

    fn key() -> Vec<u8> {
        vec![7u8; 32]
    }

    fn profile() -> Value {
        json!({ "name": "Ana", "show_phone": true, "language": "en" })
    }

    async fn new_capture(app: &TestApp, device: &str) -> (String, String) {
        let (status, body) = app
            .post("/v1/captures", device, json!({ "purpose": "login" }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let process_id = app.unico.with(|s| format!("proc-{}", s.posts.len()));
        (body["capture_id"].as_str().unwrap().to_string(), process_id)
    }

    async fn open_session(
        app: &TestApp,
        device: &str,
        capture_id: &str,
        profile: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut body = json!({ "capture_id": capture_id });
        if let Some(profile) = profile {
            body["profile"] = profile;
        }
        app.post("/v1/sessions", device, body).await
    }

    async fn capture_doc(app: &TestApp, id: &str) -> CaptureDoc {
        store::get(&app.state.db, CAPTURES, id)
            .await
            .unwrap()
            .unwrap()
    }

    // ---- identifiers ----

    #[test]
    fn derived_id_is_deterministic_43_chars_and_url_safe() {
        let id = derive_id(&key(), "some/token+with=chars");
        assert_eq!(id.len(), 43);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "{id}"
        );
        assert!(!id.contains('/'));
        assert_eq!(id, derive_id(&key(), "some/token+with=chars"));
    }

    #[test]
    fn derived_id_depends_on_both_the_key_and_the_token() {
        let id = derive_id(&key(), "token-1");
        assert_ne!(id, derive_id(&key(), "token-2"));
        assert_ne!(id, derive_id(&[8u8; 32], "token-1"));
        assert!(!id.contains("token-1"));
    }

    #[test]
    fn session_ids_are_the_sha256_hex_of_the_secret() {
        let id = session_id("secret");
        assert_eq!(id.len(), 64);
        assert_eq!(
            id,
            "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b"
        );
        assert_ne!(id, session_id("secret2"));
    }

    #[test]
    fn secrets_are_32_random_bytes_in_base64url() {
        let a = new_secret();
        assert_eq!(URL_SAFE_NO_PAD.decode(&a).unwrap().len(), 32);
        assert_ne!(a, new_secret());
    }

    #[tokio::test]
    async fn startup_requires_domain_and_a_32_byte_hmac_key() {
        use crate::test_support::{fast_unico, test_config, test_db};
        let build = |edit: &dyn Fn(&mut Config)| {
            let mut config = test_config("http://127.0.0.1:1");
            edit(&mut config);
            config
        };
        let db = test_db().await;
        let try_state = |config: Config| {
            AppState::new(db.clone(), config, fast_unico("http://127.0.0.1:1", None))
        };
        assert!(try_state(build(&|_| {})).is_ok());
        assert!(try_state(build(&|c| c.domain = None)).is_err());
        assert!(try_state(build(&|c| c.hmac_key = None)).is_err());
        assert!(
            try_state(build(
                &|c| c.hmac_key = Some(crate::config::Secret("not base64!".into()))
            ))
            .is_err()
        );
        let short = STANDARD.encode([1u8; 16]);
        assert!(
            try_state(build(
                &|c| c.hmac_key = Some(crate::config::Secret(short.clone()))
            ))
            .is_err()
        );
        // The thumbnail key is required too, with the same shape (spec 6.2.2).
        assert!(try_state(build(&|c| c.thumbnail_key = None)).is_err());
        assert!(
            try_state(build(
                &|c| c.thumbnail_key = Some(crate::config::Secret("not base64!".into()))
            ))
            .is_err()
        );
        assert!(
            try_state(build(
                &|c| c.thumbnail_key = Some(crate::config::Secret(short.clone()))
            ))
            .is_err()
        );
    }

    // ---- POST /v1/captures ----

    #[tokio::test]
    async fn capture_is_created_for_the_device_with_a_journey_for_this_domain() {
        let app = TestApp::new().await;
        let (status, body) = app
            .post("/v1/captures", DEVICE, json!({ "purpose": "login" }))
            .await;
        assert_eq!(status, StatusCode::CREATED);
        let id = body["capture_id"].as_str().unwrap();
        assert_eq!(
            body["link"],
            "https://journey.test/proc-1?collect-data=true"
        );
        assert_eq!(
            app.unico.with(|s| s.posts[0].clone()),
            json!({ "flow": "humantoken", "callbackUri": format!("https://taba.example.org/return?capture={id}") })
        );

        let doc = capture_doc(&app, id).await;
        assert_eq!(
            (doc.process_id.as_str(), doc.device_id.as_str()),
            ("proc-1", DEVICE)
        );
        assert_eq!(
            (doc.purpose, doc.status, doc.consumed, doc.person_id),
            (Purpose::Login, CaptureStatus::Created, false, None)
        );
        assert_eq!(
            doc.expires_at.as_second() - doc.created_at.as_second(),
            15 * 60
        );
        assert_eq!(body["expires_at"], doc.expires_at.to_string());
        assert_eq!(audit_actions(&app.state.db).await, ["capture.create"]);
    }

    #[tokio::test]
    async fn capture_needs_a_device_and_the_session_purposes_a_session() {
        let app = TestApp::new().await;
        let (status, _, body) = app
            .request(
                "POST",
                "/v1/captures",
                &[],
                Some(json!({ "purpose": "login" })),
            )
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("device_id_required"))
        );
        let (status, body) = app
            .post("/v1/captures", DEVICE, json!({ "purpose": "revalidation" }))
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNAUTHORIZED, Some("unauthorized"))
        );
        assert_eq!(app.unico.post_count(), 0);
        assert!(all_docs(&app.state.db, CAPTURES).await.is_empty());
    }

    #[tokio::test]
    async fn capture_is_not_stored_when_unico_refuses() {
        let app = TestApp::new().await;
        app.unico.with(|s| {
            s.post_replies
                .push_back((429, json!({ "message": "monthly process limit reached" })))
        });
        let (status, body) = app
            .post("/v1/captures", DEVICE, json!({ "purpose": "login" }))
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, Some("unico_limit"))
        );
        assert!(all_docs(&app.state.db, CAPTURES).await.is_empty());
    }

    #[tokio::test]
    async fn busy_unico_answers_503_with_retry_after() {
        let app = TestApp::new().await;
        app.unico.with(|s| {
            s.post_replies
                .extend((0..9).map(|_| (429, json!({ "message": "slow" }))))
        });
        let (status, headers, body) = app
            .request(
                "POST",
                "/v1/captures",
                &[("x-device-id", DEVICE)],
                Some(json!({ "purpose": "login" })),
            )
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, Some("unico_busy"))
        );
        assert_eq!(headers["retry-after"], "30");
    }

    // ---- POST /v1/sessions ----

    #[tokio::test]
    async fn pending_capture_answers_202_and_can_be_retried() {
        let app = TestApp::new().await;
        let (capture_id, process_id) = new_capture(&app, DEVICE).await;
        let (status, body) = open_session(&app, DEVICE, &capture_id, Some(profile())).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::ACCEPTED, Some("capture_pending"))
        );
        assert!(!capture_doc(&app, &capture_id).await.consumed);

        app.unico.finish_with_token(&process_id, "tok");
        let (status, _) = open_session(&app, DEVICE, &capture_id, Some(profile())).await;
        assert_eq!(status, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn new_volunteer_needs_a_profile_and_unico_is_asked_only_once() {
        let app = TestApp::new().await;
        let (capture_id, process_id) = new_capture(&app, DEVICE).await;
        app.unico.finish_with_token(&process_id, "tok-ana");

        let (status, body) = open_session(&app, DEVICE, &capture_id, None).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("profile_required"))
        );
        assert!(all_docs(&app.state.db, "volunteers").await.is_empty());
        assert!(all_docs(&app.state.db, SESSIONS).await.is_empty());

        let (status, body) = open_session(&app, DEVICE, &capture_id, Some(profile())).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(
            app.unico.get_count(),
            1,
            "the resolved capture is not polled again"
        );

        let expected_id = derive_id(&key(), "tok-ana");
        assert_eq!(
            body["volunteer"],
            json!({ "id": expected_id, "name": "Ana", "phone": null, "show_phone": true, "language": "en" })
        );
        let secret = body["session"].as_str().unwrap();
        assert_eq!(URL_SAFE_NO_PAD.decode(secret).unwrap().len(), 32);
        let session: SessionDoc = store::get(&app.state.db, SESSIONS, &session_id(secret))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (session.volunteer_id.as_str(), session.device_id.as_str()),
            (expected_id.as_str(), DEVICE)
        );
        assert_eq!(
            session.expires_at.as_second() - session.created_at.as_second(),
            7 * 24 * 3600
        );
        assert_eq!(body["expires_at"], session.expires_at.to_string());
        assert_eq!(
            audit_actions(&app.state.db).await,
            ["capture.create", "session.create", "volunteer.create"]
        );
    }

    #[tokio::test]
    async fn the_token_is_never_stored() {
        let app = TestApp::new().await;
        let (secret, volunteer_id) = app.login(DEVICE, "very-secret-token", "Ana").await;
        for collection in [CAPTURES, SESSIONS, "volunteers", "audit_log"] {
            let docs = serde_json::to_string(&all_docs(&app.state.db, collection).await).unwrap();
            assert!(!docs.contains("very-secret-token"), "{collection}");
            assert!(
                !docs.contains(&secret),
                "{collection} holds the raw session secret"
            );
        }
        assert_eq!(volunteer_id.len(), 43);
        let capture = all_docs(&app.state.db, CAPTURES).await;
        assert_eq!(capture[0]["person_id"], volunteer_id);
    }

    #[tokio::test]
    async fn the_same_person_on_another_device_is_the_same_volunteer() {
        let app = TestApp::new().await;
        let (first, id_a) = app.login(DEVICE, "tok-ana", "Ana").await;
        let (capture_id, process_id) = new_capture(&app, OTHER_DEVICE).await;
        app.unico.finish_with_token(&process_id, "tok-ana");
        let (status, body) = open_session(&app, OTHER_DEVICE, &capture_id, None).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "an existing profile needs no new one: {body}"
        );
        assert_eq!(body["volunteer"]["id"], id_a);
        assert_ne!(body["session"], first.as_str());
        assert_eq!(all_docs(&app.state.db, "volunteers").await.len(), 1);
        assert_eq!(all_docs(&app.state.db, SESSIONS).await.len(), 2);
    }

    #[tokio::test]
    async fn capture_is_consumed_once() {
        // spec 15, criterion 6
        let app = TestApp::new().await;
        let (capture_id, process_id) = new_capture(&app, DEVICE).await;
        app.unico.finish_with_token(&process_id, "tok");
        let (status, _) = open_session(&app, DEVICE, &capture_id, Some(profile())).await;
        assert_eq!(status, StatusCode::CREATED);
        assert!(capture_doc(&app, &capture_id).await.consumed);

        let (status, body) = open_session(&app, DEVICE, &capture_id, Some(profile())).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::CONFLICT, Some("capture_consumed"))
        );
        assert_eq!(all_docs(&app.state.db, SESSIONS).await.len(), 1);
    }

    #[tokio::test]
    async fn only_the_device_that_created_the_capture_can_consume_it() {
        // spec 15, criterion 6
        let app = TestApp::new().await;
        let (capture_id, process_id) = new_capture(&app, DEVICE).await;
        app.unico.finish_with_token(&process_id, "tok");

        let (status, body) = open_session(&app, OTHER_DEVICE, &capture_id, Some(profile())).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::FORBIDDEN, Some("capture_wrong_device"))
        );
        assert_eq!(
            app.unico.get_count(),
            0,
            "a foreign device does not even trigger a Unico call"
        );
        let (status, _, body) = app
            .request(
                "POST",
                "/v1/sessions",
                &[],
                Some(json!({ "capture_id": capture_id })),
            )
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("device_id_required"))
        );

        assert!(!capture_doc(&app, &capture_id).await.consumed);
        let (status, _) = open_session(&app, DEVICE, &capture_id, Some(profile())).await;
        assert_eq!(status, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn expired_and_unknown_captures_are_refused() {
        let app = TestApp::new().await;
        let (capture_id, process_id) = new_capture(&app, DEVICE).await;
        app.unico.finish_with_token(&process_id, "tok");
        let mut doc = capture_doc(&app, &capture_id).await;
        doc.expires_at = crate::store::now() - SignedDuration::from_secs(1);
        store::replace(&app.state.db, CAPTURES, &capture_id, &doc)
            .await
            .unwrap();

        let (status, body) = open_session(&app, DEVICE, &capture_id, Some(profile())).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::GONE, Some("capture_expired"))
        );
        let (status, body) = open_session(&app, DEVICE, "nope", Some(profile())).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::NOT_FOUND, Some("capture_not_found"))
        );
        assert!(all_docs(&app.state.db, SESSIONS).await.is_empty());
    }

    #[tokio::test]
    async fn journeys_without_a_token_create_no_volunteer() {
        // spec 15, criterion 1
        let cases = [
            (
                json!({ "state": "PROCESS_STATE_FINISHED", "data": { "humanToken": { "value": "" } } }),
                "capture_inconclusive",
                422,
            ),
            (
                json!({ "state": "PROCESS_STATE_FINISHED", "data": { "humanToken": { "value": "  " } } }),
                "capture_inconclusive",
                422,
            ),
            (
                json!({ "state": "PROCESS_STATE_FINISHED", "data": {} }),
                "capture_inconclusive",
                422,
            ),
            (
                json!({ "state": "PROCESS_STATE_FAILED" }),
                "capture_failed",
                422,
            ),
            (
                json!({ "state": "PROCESS_STATE_CANCELED" }),
                "capture_canceled",
                422,
            ),
            (
                json!({ "state": "PROCESS_STATE_UNSPECIFIED" }),
                "unico_unexpected_state",
                502,
            ),
            (
                json!({ "state": "PROCESS_STATE_AWAITING_FOR_DOCUMENT" }),
                "unico_unexpected_state",
                502,
            ),
        ];
        for (process, code, http) in cases {
            let app = TestApp::new().await;
            let (capture_id, process_id) = new_capture(&app, DEVICE).await;
            app.unico
                .with(|s| s.processes.insert(process_id, process.clone()));
            let (status, body) = open_session(&app, DEVICE, &capture_id, Some(profile())).await;
            assert_eq!(
                (status.as_u16(), body["error"].as_str()),
                (http, Some(code)),
                "{process}"
            );
            assert!(
                all_docs(&app.state.db, "volunteers").await.is_empty(),
                "{process}"
            );
            assert!(
                all_docs(&app.state.db, SESSIONS).await.is_empty(),
                "{process}"
            );
        }
    }

    #[tokio::test]
    async fn a_failed_journey_is_remembered_and_not_polled_again() {
        let app = TestApp::new().await;
        let (capture_id, process_id) = new_capture(&app, DEVICE).await;
        app.unico.set_state(&process_id, "PROCESS_STATE_CANCELED");
        for _ in 0..2 {
            let (status, body) = open_session(&app, DEVICE, &capture_id, Some(profile())).await;
            assert_eq!(
                (status, body["error"].as_str()),
                (StatusCode::UNPROCESSABLE_ENTITY, Some("capture_canceled"))
            );
        }
        assert_eq!(app.unico.get_count(), 1);
    }

    #[tokio::test]
    async fn unico_errors_while_resolving_are_reported() {
        let app = TestApp::new().await;
        let (capture_id, _) = new_capture(&app, DEVICE).await;
        app.unico.with(|s| {
            s.get_replies
                .push_back((403, json!({ "message": "PERMISSION_DENIED" })))
        });
        let (status, body) = open_session(&app, DEVICE, &capture_id, Some(profile())).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Some("unico_misconfigured")
            )
        );
    }

    #[tokio::test]
    async fn banned_volunteer_gets_403_and_no_session() {
        let app = TestApp::new().await;
        let (_, id) = app.login(DEVICE, "tok-ana", "Ana").await;
        let mut doc: VolunteerDoc = store::get(&app.state.db, volunteers::COLLECTION, &id)
            .await
            .unwrap()
            .unwrap();
        doc.banned = true;
        store::replace(&app.state.db, volunteers::COLLECTION, &id, &doc)
            .await
            .unwrap();

        let (capture_id, process_id) = new_capture(&app, OTHER_DEVICE).await;
        app.unico.finish_with_token(&process_id, "tok-ana");
        let (status, body) = open_session(&app, OTHER_DEVICE, &capture_id, None).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::FORBIDDEN, Some("banned"))
        );
        assert_eq!(all_docs(&app.state.db, SESSIONS).await.len(), 1);
        assert!(!capture_doc(&app, &capture_id).await.consumed);
    }

    #[tokio::test]
    async fn invalid_profiles_are_rejected() {
        let app = TestApp::new().await;
        let (capture_id, process_id) = new_capture(&app, DEVICE).await;
        app.unico.finish_with_token(&process_id, "tok");
        let bad = [
            json!({ "name": "  ", "show_phone": false, "language": "en" }),
            json!({ "name": "x".repeat(101), "show_phone": false, "language": "en" }),
            json!({ "name": "Ana", "show_phone": false, "language": "fr" }),
            json!({ "name": "Ana", "phone": "1".repeat(31), "show_phone": false, "language": "en" }),
        ];
        for profile in bad {
            let (status, body) =
                open_session(&app, DEVICE, &capture_id, Some(profile.clone())).await;
            assert_eq!(
                (status, body["error"].as_str()),
                (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_profile")),
                "{profile}"
            );
        }
        assert!(!capture_doc(&app, &capture_id).await.consumed);
    }

    #[tokio::test]
    async fn profile_is_trimmed_and_blank_phone_means_none() {
        let app = TestApp::new().await;
        let (capture_id, process_id) = new_capture(&app, DEVICE).await;
        app.unico.finish_with_token(&process_id, "tok");
        let profile = json!({ "name": "  Ana Souza ", "phone": "  ", "show_phone": false, "language": "pt-BR" });
        let (_, body) = open_session(&app, DEVICE, &capture_id, Some(profile)).await;
        assert_eq!(
            (
                body["volunteer"]["name"].as_str(),
                &body["volunteer"]["phone"]
            ),
            (Some("Ana Souza"), &Value::Null)
        );
    }

    // ---- sessions in use ----

    #[tokio::test]
    async fn session_authenticates_requests() {
        let app = TestApp::new().await;
        let (secret, id) = app.login(DEVICE, "tok", "Ana").await;
        let (status, body) = app.get_authed("/v1/me", &secret).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["volunteer"]["id"], id);
    }

    #[tokio::test]
    async fn missing_malformed_and_unknown_credentials_are_401() {
        let app = TestApp::new().await;
        app.login(DEVICE, "tok", "Ana").await;
        let (status, _, body) = app.request("GET", "/v1/me", &[], None).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNAUTHORIZED, Some("unauthorized"))
        );
        let (status, _, _) = app
            .request("GET", "/v1/me", &[("authorization", "Basic abc")], None)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = app.get_authed("/v1/me", "unknown-secret").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    async fn stored_session(app: &TestApp, secret: &str) -> SessionDoc {
        store::get(&app.state.db, SESSIONS, &session_id(secret))
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn expired_session_is_refused() {
        let app = TestApp::new().await;
        let (secret, _) = app.login(DEVICE, "tok", "Ana").await;
        let mut session = stored_session(&app, &secret).await;
        session.expires_at = crate::store::now() - SignedDuration::from_secs(1);
        store::replace(&app.state.db, SESSIONS, &session_id(&secret), &session)
            .await
            .unwrap();
        let (status, _) = app.get_authed("/v1/me", &secret).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn session_is_renewed_only_after_a_day() {
        let app = TestApp::new().await;
        let (secret, _) = app.login(DEVICE, "tok", "Ana").await;

        let before = stored_session(&app, &secret).await;
        app.get_authed("/v1/me", &secret).await;
        let after = stored_session(&app, &secret).await;
        assert_eq!(
            before.renewed_at, after.renewed_at,
            "used within a day: not renewed"
        );

        let mut old = before.clone();
        old.renewed_at = crate::store::now() - SignedDuration::from_hours(25);
        old.expires_at = crate::store::now() + SignedDuration::from_hours(24);
        store::replace(&app.state.db, SESSIONS, &session_id(&secret), &old)
            .await
            .unwrap();
        let (status, _) = app.get_authed("/v1/me", &secret).await;
        assert_eq!(status, StatusCode::OK);
        let renewed = stored_session(&app, &secret).await;
        assert!(renewed.renewed_at.as_second() > old.renewed_at.as_second() + 24 * 3600);
        assert!(renewed.expires_at.as_second() - renewed.renewed_at.as_second() == 7 * 24 * 3600);
        assert_eq!(renewed.created_at, old.created_at);
    }

    #[tokio::test]
    async fn banning_stops_existing_sessions() {
        let app = TestApp::new().await;
        let (secret, id) = app.login(DEVICE, "tok", "Ana").await;
        let mut doc: VolunteerDoc = store::get(&app.state.db, volunteers::COLLECTION, &id)
            .await
            .unwrap()
            .unwrap();
        doc.banned = true;
        store::replace(&app.state.db, volunteers::COLLECTION, &id, &doc)
            .await
            .unwrap();
        let (status, body) = app.get_authed("/v1/me", &secret).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::FORBIDDEN, Some("banned"))
        );
    }

    #[tokio::test]
    async fn logout_deletes_only_the_current_session_and_is_audited() {
        let app = TestApp::new().await;
        let (first, _) = app.login(DEVICE, "tok", "Ana").await;
        let (capture_id, process_id) = new_capture(&app, OTHER_DEVICE).await;
        app.unico.finish_with_token(&process_id, "tok");
        let (_, body) = open_session(&app, OTHER_DEVICE, &capture_id, None).await;
        let second = body["session"].as_str().unwrap().to_string();

        let auth = format!("Bearer {first}");
        let (status, _, _) = app
            .request(
                "DELETE",
                "/v1/sessions/current",
                &[("authorization", auth.as_str())],
                None,
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(
            app.get_authed("/v1/me", &first).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(app.get_authed("/v1/me", &second).await.0, StatusCode::OK);
        assert!(
            audit_actions(&app.state.db)
                .await
                .contains(&"session.delete".to_string())
        );
        let (status, _, _) = app
            .request("DELETE", "/v1/sessions/current", &[], None)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn login_audit_entries_carry_ids_device_and_legal_basis() {
        let app = TestApp::new().await;
        let (_, id) = app.login(DEVICE, "tok", "Ana").await;
        let entries = all_docs(&app.state.db, "audit_log").await;
        let login = entries
            .iter()
            .find(|e| e["action"] == "session.create")
            .unwrap();
        assert_eq!(
            (
                &login["volunteer_id"],
                &login["device_id"],
                &login["legal_basis"]
            ),
            (&json!(id), &json!(DEVICE), &json!("fraud_prevention"))
        );
        assert!(
            !serde_json::to_string(&entries).unwrap().contains("Ana"),
            "audit entries hold no names"
        );
    }

    #[tokio::test]
    async fn concurrent_requests_open_exactly_one_session() {
        // spec 15, criterion 6, under concurrency
        let app = Arc::new(TestApp::new().await);
        let (capture_id, process_id) = new_capture(&app, DEVICE).await;
        app.unico.finish_with_token(&process_id, "tok");
        let calls: Vec<_> = (0..40)
            .map(|_| {
                let (app, capture_id) = (app.clone(), capture_id.clone());
                tokio::spawn(async move {
                    open_session(&app, DEVICE, &capture_id, Some(profile())).await
                })
            })
            .collect();
        let mut created = 0;
        for call in calls {
            let (status, body) = call.await.unwrap();
            match status {
                StatusCode::CREATED => created += 1,
                StatusCode::CONFLICT => assert_eq!(body["error"], "capture_consumed"),
                other => panic!("unexpected {other}: {body}"),
            }
        }
        assert_eq!(created, 1);
        assert_eq!(all_docs(&app.state.db, SESSIONS).await.len(), 1);
        assert_eq!(all_docs(&app.state.db, "volunteers").await.len(), 1);
    }
}
