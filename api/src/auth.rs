//! Login captures, sessions and the `Volunteer` extractor (spec 3.1, 5.2, 6.2.5, 6.3.2, 8.1).

use std::sync::Arc;

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
use crate::store::{self, fs_error, tx_create, tx_replace};
use crate::unico::{ProcessResult, Unico};
use crate::volunteers::{self, ProfileInput, VolunteerDoc};

const CAPTURES: &str = "captures";
const SESSIONS: &str = "sessions";
const CAPTURE_TTL: SignedDuration = SignedDuration::from_mins(15);
const RENEW_AFTER: SignedDuration = SignedDuration::from_hours(24);

#[derive(Clone)]
pub struct AppState {
    pub db: FirestoreDb,
    pub config: Arc<Config>,
    pub unico: Arc<Unico>,
    hmac_key: Arc<[u8]>,
    domain: Arc<str>,
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
        Ok(Self {
            db,
            config: Arc::new(config),
            unico: Arc::new(unico),
            hmac_key: hmac_key.into(),
            domain: domain.into(),
        })
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

fn new_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn device_id(headers: &HeaderMap) -> Result<String, ApiError> {
    headers
        .get("x-device-id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.len() <= 64)
        .map(str::to_string)
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "device_id_required"))
}

// ---- documents ----

#[allow(dead_code)] // only `login` is created in phase 1
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

fn days(n: u32) -> SignedDuration {
    SignedDuration::from_hours(24 * i64::from(n))
}

// ---- POST /v1/captures ----

#[derive(Debug, Deserialize)]
pub struct CreateCapture {
    purpose: Purpose,
}

pub async fn post_captures(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateCapture>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let device = device_id(&headers)?;
    if request.purpose != Purpose::Login {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_purpose",
        ));
    }
    let capture_id = uuid::Uuid::new_v4().to_string();
    let callback = format!("https://{}/return?capture={capture_id}", state.domain);
    let journey = state.unico.create_journey(&callback).await?;

    let now = FirestoreInstant::now();
    let doc = CaptureDoc {
        process_id: journey.process_id,
        purpose: Purpose::Login,
        device_id: device.clone(),
        status: CaptureStatus::Created,
        person_id: None,
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
async fn resolve_person(
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
    store::replace(&state.db, CAPTURES, capture_id, capture).await?;
    terminal_error(status).map_or_else(|| person_id.ok_or_else(ApiError::internal), Err)
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
    let existing: Option<VolunteerDoc> =
        store::get(db, volunteers::COLLECTION, &a.person_id).await?;
    let now = FirestoreInstant::now();
    let is_new = existing.is_none();
    let mut volunteer = match (existing, a.profile.as_ref()) {
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
    let session = SessionDoc {
        volunteer_id: a.person_id.clone(),
        device_id: a.device_id.clone(),
        created_at: now,
        renewed_at: now,
        expires_at: now + days(a.session_days),
    };

    tx_replace(db, tx, volunteers::COLLECTION, &a.person_id, &volunteer)?;
    tx_replace(
        db,
        tx,
        CAPTURES,
        &a.capture_id,
        &CaptureDoc {
            consumed: true,
            ..capture
        },
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
    let mut capture: CaptureDoc = store::get(&state.db, CAPTURES, &request.capture_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "capture_not_found"))?;
    if capture.device_id != device {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "capture_wrong_device"));
    }
    if capture.consumed {
        return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
    }
    if FirestoreInstant::now() >= capture.expires_at {
        return Err(ApiError::new(StatusCode::GONE, "capture_expired"));
    }
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
    let (volunteer, session) = state
        .db
        .run_transaction(|db, tx| {
            let args = args.clone();
            Box::pin(async move { login(&db, tx, &args).await.map_err(BackoffError::permanent) })
        })
        .await
        .map_err(fs_error)?;

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

/// Shelter membership as `GET /v1/me` reports it (spec 8.1). Filled in phase 2.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct Membership {
    pub shelter_id: String,
    pub name: String,
    pub role: String,
    pub confirmation: String,
}

/// The authenticated volunteer: profile, global roles (inside `doc`) and active memberships.
#[derive(Debug, Clone)]
pub struct Volunteer {
    pub id: String,
    pub doc: VolunteerDoc,
    pub memberships: Vec<Membership>,
    pub session_id: String,
}

fn unauthorized() -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized")
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
        let now = FirestoreInstant::now();
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
        Ok(Volunteer {
            id: session.volunteer_id,
            doc,
            memberships: vec![],
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
    async fn capture_needs_a_device_and_the_login_purpose() {
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
            .post("/v1/captures", DEVICE, json!({ "purpose": "invite" }))
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("unsupported_purpose")
            )
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
        doc.expires_at = FirestoreInstant::now() - SignedDuration::from_secs(1);
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
        session.expires_at = FirestoreInstant::now() - SignedDuration::from_secs(1);
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
        old.renewed_at = FirestoreInstant::now() - SignedDuration::from_hours(25);
        old.expires_at = FirestoreInstant::now() + SignedDuration::from_hours(24);
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
}
