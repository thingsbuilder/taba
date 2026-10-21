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

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use firestore::FirestoreInstant;
use firestore::jiff::SignedDuration;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::alerts::{self, AlertDoc, AlertKind};
use crate::audit::{self, AuditEntry};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::searches::{self, RegisterOutcome, SearchInput, SearchOwner, SearchVia};
use crate::shelters::{self, Confirmation, ShelterDoc};
use crate::store::{self, tx_create, tx_replace};
use crate::volunteers::GlobalRole;

pub const COLLECTION: &str = "search_links";
const TTL: SignedDuration = SignedDuration::from_hours(12);
/// Failed submissions a link takes before it answers `410`.
const MAX_ATTEMPTS: u32 = 5;
/// How long one submit holds the link while Unico is being asked (Unico's own deadline is 60 s).
const LEASE: SignedDuration = SignedDuration::from_secs(120);
/// Submits per hour from one device (`X-Device-Id`) on one IP, and from one IP alone.
const MAX_PER_DEVICE_HOUR: usize = 20;
const MAX_PER_IP_HOUR: usize = 100;
const LIMIT_WINDOW: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkStatus {
    Active,
    Used,
    Canceled,
}

/// `search_links/{code}` (128 bits, base64url); TTL on `expires_at`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchLinkDoc {
    pub created_by: String,
    pub shelter_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    pub status: LinkStatus,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    /// `created_at` + 12 hours.
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub expires_at: FirestoreInstant,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub used_at: Option<FirestoreInstant>,
    /// The search the submission made, once the link is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_id: Option<String>,
    /// Failed submissions, for the limit per link.
    #[serde(default)]
    pub attempts: u32,
    /// A submit is running until this instant; no other one may start.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub processing_until: Option<FirestoreInstant>,
}

impl SearchLinkDoc {
    /// Whether the family can still submit: active, not expired, attempts left.
    fn is_valid(&self, now: FirestoreInstant) -> bool {
        self.status == LinkStatus::Active && now < self.expires_at && self.attempts < MAX_ATTEMPTS
    }
}

fn new_code() -> String {
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

fn busy() -> ApiError {
    let mut error = ApiError::new(StatusCode::SERVICE_UNAVAILABLE, store::CONTENTION);
    error.retry_after = Some(1);
    error
}

/// The search the submit makes: always the same for one link, so a retry writes the same document.
fn search_id_of(code: &str) -> String {
    let digest = Sha256::digest(code.as_bytes());
    let bytes: [u8; 16] = digest[..16].try_into().expect("sha256 has 32 bytes");
    uuid::Uuid::from_bytes(bytes).to_string()
}

// ---- POST /v1/search-links ----

#[derive(Debug, Deserialize)]
pub struct CreateLink {
    shelter_id: String,
}

/// `POST /v1/search-links`: `201 {code, url, expires_at}`; counts in `SEARCHES_PER_DAY` (`429 search_limit`).
/// Who may: a member of a `confirmed` shelter, or `civil_defense`, `mediator`, `protection`.
pub async fn post_search_link(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Json(request): Json<CreateLink>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let shelter: ShelterDoc = store::get(&state.db, shelters::COLLECTION, &request.shelter_id)
        .await?
        .ok_or_else(forbidden)?;
    let global = [
        GlobalRole::CivilDefense,
        GlobalRole::Mediator,
        GlobalRole::Protection,
    ]
    .into_iter()
    .any(|role| volunteer.has_global(role));
    let member_of_confirmed = volunteer.role_in(&request.shelter_id).is_some()
        && shelter.confirmation == Confirmation::Confirmed;
    if !global && !member_of_confirmed {
        return Err(forbidden());
    }

    let now = store::now();
    let code = new_code();
    let link = SearchLinkDoc {
        created_by: volunteer.id.clone(),
        shelter_id: request.shelter_id.clone(),
        event_id: shelter.event_id.clone(),
        status: LinkStatus::Active,
        created_at: now,
        expires_at: now + TTL,
        used_at: None,
        search_id: None,
        attempts: 0,
        processing_until: None,
    };
    let limit = state.config.searches_per_day;
    store::transact(&state.db, |db, tx| {
        let (code, link, by) = (code.clone(), link.clone(), volunteer.id.clone());
        Box::pin(async move {
            searches::take_daily_slot(&db, tx, &by, limit).await?;
            tx_create(&db, tx, COLLECTION, &code, &link)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new("search_link.create", "search_link", &code)
                    .by(&by)
                    .in_shelter(&link.shelter_id),
            )
        })
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "code": code,
            "url": format!("https://{}/f/{code}", state.domain()),
            "expires_at": link.expires_at.to_string(),
        })),
    ))
}

// ---- DELETE /v1/search-links/{code} ----

/// `DELETE /v1/search-links/{code}`: cancels it; only who made it or the Civil Defense.
pub async fn delete_search_link(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(code): Path<String>,
) -> Result<StatusCode, ApiError> {
    store::transact(&state.db, |db, tx| {
        let (code, volunteer) = (code.clone(), volunteer.clone());
        Box::pin(async move {
            let mut link = store::get::<SearchLinkDoc>(&db, COLLECTION, &code)
                .await?
                .filter(|l| l.status == LinkStatus::Active && store::now() < l.expires_at)
                .ok_or_else(ApiError::search_link_invalid)?;
            if link.created_by != volunteer.id && !volunteer.has_global(GlobalRole::CivilDefense) {
                return Err(forbidden());
            }
            link.status = LinkStatus::Canceled;
            link.processing_until = None;
            tx_replace(&db, tx, COLLECTION, &code, &link)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new("search_link.cancel", "search_link", &code)
                    .by(&volunteer.id)
                    .in_shelter(&link.shelter_id),
            )
        })
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- GET /v1/search-links/{code} ----

/// `GET /v1/search-links/{code}`, public: `{valid: true}`, or `410 search_link_invalid` for a
/// link that is expired, used, canceled, out of attempts or unknown. Nothing about who made it.
pub async fn get_search_link(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<Value>, ApiError> {
    store::get::<SearchLinkDoc>(&state.db, COLLECTION, &code)
        .await?
        .filter(|l| l.is_valid(store::now()))
        .ok_or_else(ApiError::search_link_invalid)?;
    Ok(Json(json!({ "valid": true })))
}

// ---- POST /v1/search-links/{code}/submit ----

/// A sliding window per key, in this process's memory.
// ponytail: the counters live in memory (single e2-micro instance) and reset on restart; the
// per-link attempts in Firestore still cap the damage. With more than one instance, count in
// Firestore with a TTL document per key and hour.
fn allow(key: &str, max: usize) -> bool {
    static HITS: OnceLock<Mutex<HashMap<String, Vec<Instant>>>> = OnceLock::new();
    let mut hits = HITS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let now = Instant::now();
    if hits.len() > 10_000 {
        hits.retain(|_, at| at.iter().any(|t| now.duration_since(*t) < LIMIT_WINDOW));
    }
    let at = hits.entry(key.to_string()).or_default();
    at.retain(|t| now.duration_since(*t) < LIMIT_WINDOW);
    if at.len() >= max {
        return false;
    }
    at.push(now);
    true
}

/// The device header and the IP the proxy saw: the last `X-Forwarded-For` entry is the one the
/// proxy appended, the earlier ones the client may have made up.
fn rate_limit(headers: &HeaderMap) -> Result<(), ApiError> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let ip = header("x-forwarded-for")
        .and_then(|v| v.rsplit(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("unknown");
    let device = header("x-device-id")
        .map(str::trim)
        .filter(|v| v.len() <= 64)
        .unwrap_or("none");
    if allow(&format!("ip~{ip}"), MAX_PER_IP_HOUR)
        && allow(&format!("device~{device}~{ip}"), MAX_PER_DEVICE_HOUR)
    {
        Ok(())
    } else {
        let mut error = ApiError::new(StatusCode::TOO_MANY_REQUESTS, "too_many_requests");
        error.retry_after = Some(60);
        Err(error)
    }
}

/// `POST /v1/search-links/{code}/submit`, public: the same pipeline as `POST /v1/searches`
/// (`searches::register_photos`) and the same answer; `410 search_link_invalid`,
/// `503 unico_degraded` keeps the link.
pub async fn submit_search_link(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
    Json(mut request): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    rate_limit(&headers)?;
    // The family sends no id: it is derived from the link, so a retry is the same search.
    if let Some(body) = request.as_object_mut() {
        body.insert("id".into(), search_id_of(&code).into());
    }
    let input = searches::parse_input(request)?;
    let device = headers
        .get("x-device-id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.len() <= 64)
        .map(str::to_string);
    run_submit(&state, &code, device, input, |owner, input| {
        searches::register_photos(&state, owner, input)
    })
    .await
}

/// Claim the link, run `register`, consume the link. `register` is `searches::register_photos`
/// in production; the tests pass a fake so no Unico is involved.
async fn run_submit<F, Fut>(
    state: &AppState,
    code: &str,
    device: Option<String>,
    input: SearchInput,
    register: F,
) -> Result<(StatusCode, Json<Value>), ApiError>
where
    F: FnOnce(SearchOwner, SearchInput) -> Fut,
    Fut: Future<Output = Result<RegisterOutcome, ApiError>>,
{
    let link = store::transact(&state.db, |db, tx| {
        let code = code.to_string();
        Box::pin(async move {
            let now = store::now();
            let mut link = store::get::<SearchLinkDoc>(&db, COLLECTION, &code)
                .await?
                .filter(|l| l.is_valid(now))
                .ok_or_else(ApiError::search_link_invalid)?;
            if link.processing_until.is_some_and(|until| now < until) {
                return Err(busy());
            }
            link.processing_until = Some(now + LEASE);
            tx_replace(&db, tx, COLLECTION, &code, &link)?;
            Ok(link)
        })
    })
    .await?;

    let owner = SearchOwner {
        volunteer_id: link.created_by.clone(),
        device_id: device.clone(),
        origin_shelter_id: Some(link.shelter_id.clone()),
        event_id: link.event_id.clone(),
        via: SearchVia::FamilyLink,
        key: Some(code.to_string()),
    };
    let search_id = input.id.clone();
    let outcome = match register(owner, input).await {
        Ok(outcome) => outcome,
        Err(error) => {
            // Degraded Unico and contention are not the family's fault: no attempt is spent.
            let spent = u32::from(!matches!(error.code, "unico_degraded" | store::CONTENTION));
            release(state, code, spent).await;
            return Err(error);
        }
    };

    store::transact(&state.db, |db, tx| {
        let (code, search_id, device) = (code.to_string(), search_id.clone(), device.clone());
        Box::pin(async move {
            let mut link = store::get::<SearchLinkDoc>(&db, COLLECTION, &code)
                .await?
                .ok_or_else(ApiError::search_link_invalid)?;
            let now = store::now();
            link.status = LinkStatus::Used;
            link.used_at = Some(now);
            link.search_id = Some(search_id.clone());
            link.processing_until = None;
            tx_replace(&db, tx, COLLECTION, &code, &link)?;
            let mut entry = AuditEntry::new("search_link.submit", "search_link", &code)
                .by(&link.created_by)
                .in_shelter(&link.shelter_id);
            if let Some(device) = &device {
                entry = entry.on_device(device);
            }
            audit::add(&db, tx, &entry)?;
            // Informational, only for who made the link, never the content.
            let mut alert = AlertDoc::new(
                AlertKind::FamilySearchReceived,
                "search",
                &search_id,
                json!({ "usable_photos": outcome.usable_photos }),
            )
            .about_volunteers(vec![link.created_by.clone()]);
            alert.informational = true;
            alert.for_civil_defense = false;
            alert.event_id = link.event_id.clone();
            alerts::add(&db, tx, &alert)
        })
    })
    .await?;
    Ok(searches::answer(outcome))
}

/// Gives the link back after a failed run, spending `spent` attempts.
async fn release(state: &AppState, code: &str, spent: u32) {
    let released = store::transact(&state.db, |db, tx| {
        let code = code.to_string();
        Box::pin(async move {
            let Some(mut link) = store::get::<SearchLinkDoc>(&db, COLLECTION, &code).await? else {
                return Ok(());
            };
            link.processing_until = None;
            link.attempts += spent;
            tx_replace(&db, tx, COLLECTION, &code, &link)
        })
    })
    .await;
    if let Err(error) = released {
        // The lease expires by itself.
        tracing::warn!(code = error.code, "could not release a search link");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::searches::{Family, MissingPerson};
    use base64::engine::general_purpose::STANDARD;
    const MAX_IMAGE_BYTES: usize = 800 * 1024;
    use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, audit_actions, new_id};

    fn photo(bytes: usize) -> String {
        STANDARD.encode(vec![0xFFu8; bytes])
    }

    fn input(code: &str) -> SearchInput {
        SearchInput {
            id: search_id_of(code),
            photos: vec![vec![1, 2, 3]],
            missing_person: MissingPerson {
                name: "Maria".into(),
                nickname: None,
                notes: "secret notes".into(),
            },
            family: Family {
                name: "Ana".into(),
                phone: "5551999990000".into(),
                relationship: "daughter".into(),
                language: "pt-BR".into(),
            },
        }
    }

    fn body(photos: Vec<String>) -> Value {
        json!({
            "photos_base64": photos,
            "missing_person": { "name": "Maria", "notes": "" },
            "family": { "name": "Ana", "phone": "5551999990000", "relationship": "daughter", "language": "pt-BR" },
        })
    }

    async fn setup() -> (TestApp, Actor, String) {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.confirmed_shelter(&admin, "Ginásio", ORIGIN).await;
        (app, admin, shelter)
    }

    async fn make_link(app: &TestApp, who: &Actor, shelter: &str) -> String {
        let (status, body) = app
            .send(
                "POST",
                "/v1/search-links",
                &who.secret,
                Some(json!({ "shelter_id": shelter })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["code"].as_str().unwrap().to_string()
    }

    async fn create_status(app: &TestApp, who: &Actor, shelter: &str) -> StatusCode {
        let body = Some(json!({ "shelter_id": shelter }));
        app.send("POST", "/v1/search-links", &who.secret, body)
            .await
            .0
    }

    async fn link_doc(app: &TestApp, code: &str) -> SearchLinkDoc {
        store::get(&app.state.db, COLLECTION, code)
            .await
            .unwrap()
            .unwrap()
    }

    async fn public(
        app: &TestApp,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let xff = format!("test-{}", new_id());
        let (status, _, json) = app
            .request(method, path, &[("x-forwarded-for", xff.as_str())], body)
            .await;
        (status, json)
    }

    fn ok(usable: u32) -> Result<RegisterOutcome, ApiError> {
        Ok(RegisterOutcome {
            usable_photos: usable,
        })
    }

    async fn submit(app: &TestApp, code: &str) -> Result<(StatusCode, Json<Value>), ApiError> {
        run_submit(&app.state, code, None, input(code), |_, _| async { ok(2) }).await
    }

    // ---- create ----

    #[tokio::test]
    async fn create_makes_a_12_hour_link_in_the_volunteers_name_and_audits_it() {
        let (app, admin, shelter) = setup().await;
        let (status, body) = app
            .send(
                "POST",
                "/v1/search-links",
                &admin.secret,
                Some(json!({ "shelter_id": shelter })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
        let code = body["code"].as_str().unwrap();
        assert_eq!(URL_SAFE_NO_PAD.decode(code).unwrap().len(), 16);
        assert_eq!(
            body["url"],
            format!("https://{}/f/{code}", crate::test_support::TEST_DOMAIN)
        );
        let link = link_doc(&app, code).await;
        assert_eq!(link.created_by, admin.id);
        assert_eq!(link.shelter_id, shelter);
        assert_eq!(link.status, LinkStatus::Active);
        assert_eq!(link.attempts, 0);
        assert_eq!(link.expires_at.duration_since(link.created_at), TTL);
        assert_eq!(body["expires_at"], link.expires_at.to_string());
        assert!(
            audit_actions(&app.state.db)
                .await
                .contains(&"search_link.create".to_string())
        );
    }

    #[tokio::test]
    async fn create_permissions() {
        let (app, _admin, shelter) = setup().await;
        // A member of an unconfirmed shelter.
        let other = app.actor("Other").await;
        let unconfirmed = app
            .create_shelter(
                &other,
                "Galpão",
                crate::test_support::north_of_origin(5000.0),
            )
            .await;
        assert_eq!(
            create_status(&app, &other, &unconfirmed).await,
            StatusCode::FORBIDDEN
        );
        // Not a member of the confirmed shelter.
        assert_eq!(
            create_status(&app, &other, &shelter).await,
            StatusCode::FORBIDDEN
        );
        // Unknown shelter.
        assert_eq!(
            create_status(&app, &other, "nope").await,
            StatusCode::FORBIDDEN
        );
        // Global roles do not need to be members.
        for role in [
            GlobalRole::CivilDefense,
            GlobalRole::Mediator,
            GlobalRole::Protection,
        ] {
            let who = app.actor(&format!("g-{role:?}")).await;
            app.grant(&who, role).await;
            assert_eq!(
                create_status(&app, &who, &unconfirmed).await,
                StatusCode::CREATED,
                "{role:?}"
            );
        }
        // Validator alone is not enough.
        let validator = app.actor("Validator").await;
        app.grant(&validator, GlobalRole::Validator).await;
        assert_eq!(
            create_status(&app, &validator, &shelter).await,
            StatusCode::FORBIDDEN
        );
        // No session.
        let (status, _, _) = app
            .request(
                "POST",
                "/v1/search-links",
                &[],
                Some(json!({ "shelter_id": shelter })),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_counts_in_the_daily_search_limit() {
        let (mut app, admin, shelter) = setup().await;
        std::sync::Arc::make_mut(&mut app.state.config).searches_per_day = 2;
        make_link(&app, &admin, &shelter).await;
        make_link(&app, &admin, &shelter).await;
        let (status, body) = app
            .send(
                "POST",
                "/v1/search-links",
                &admin.secret,
                Some(json!({ "shelter_id": shelter })),
            )
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::TOO_MANY_REQUESTS, Some("search_limit"))
        );
        assert_eq!(all_docs(&app.state.db, COLLECTION).await.len(), 2);
    }

    // ---- cancel ----

    #[tokio::test]
    async fn cancel_by_creator_or_civil_defense_only() {
        let (app, admin, shelter) = setup().await;
        let code = make_link(&app, &admin, &shelter).await;
        let path = format!("/v1/search-links/{code}");
        let stranger = app.actor("Stranger").await;
        assert_eq!(
            app.send("DELETE", &path, &stranger.secret, None).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.send("DELETE", &path, &admin.secret, None).await.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(link_doc(&app, &code).await.status, LinkStatus::Canceled);
        assert_eq!(
            app.send("DELETE", &path, &admin.secret, None).await.0,
            StatusCode::GONE
        );
        assert!(
            audit_actions(&app.state.db)
                .await
                .contains(&"search_link.cancel".to_string())
        );

        let code = make_link(&app, &admin, &shelter).await;
        let dc = app.actor("DC").await;
        app.grant(&dc, GlobalRole::CivilDefense).await;
        let status = app
            .send(
                "DELETE",
                &format!("/v1/search-links/{code}"),
                &dc.secret,
                None,
            )
            .await
            .0;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    // ---- public get ----

    #[tokio::test]
    async fn get_says_only_whether_the_link_is_valid() {
        let (app, admin, shelter) = setup().await;
        let code = make_link(&app, &admin, &shelter).await;
        let path = format!("/v1/search-links/{code}");
        let (status, body) = public(&app, "GET", &path, None).await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "valid": true })));

        let gone = json!({ "error": "search_link_invalid", "detail": {} });
        // Unknown.
        assert_eq!(
            public(&app, "GET", "/v1/search-links/nope", None).await,
            (StatusCode::GONE, gone.clone())
        );
        // Expired.
        let mut link = link_doc(&app, &code).await;
        link.expires_at = store::now() - SignedDuration::from_mins(1);
        store::replace(&app.state.db, COLLECTION, &code, &link)
            .await
            .unwrap();
        assert_eq!(
            public(&app, "GET", &path, None).await,
            (StatusCode::GONE, gone.clone())
        );
        // Used.
        let used = make_link(&app, &admin, &shelter).await;
        drop(submit(&app, &used).await.unwrap());
        assert_eq!(
            public(&app, "GET", &format!("/v1/search-links/{used}"), None).await,
            (StatusCode::GONE, gone.clone())
        );
        // Canceled.
        let canceled = make_link(&app, &admin, &shelter).await;
        app.send(
            "DELETE",
            &format!("/v1/search-links/{canceled}"),
            &admin.secret,
            None,
        )
        .await;
        assert_eq!(
            public(&app, "GET", &format!("/v1/search-links/{canceled}"), None).await,
            (StatusCode::GONE, gone)
        );
    }

    // ---- submit ----

    #[tokio::test]
    async fn submit_answers_like_a_search_uses_the_link_once_and_warns_only_its_creator() {
        let (app, admin, shelter) = setup().await;
        let code = make_link(&app, &admin, &shelter).await;
        let owner = Arc::new(Mutex::new(None));
        let seen = owner.clone();
        let (status, Json(answer)) = run_submit(
            &app.state,
            &code,
            Some("dev-x".into()),
            input(&code),
            |o, i| async move {
                *seen.lock().unwrap() = Some((o, i.id));
                ok(3)
            },
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(
            answer,
            json!({ "message": "search_registered", "usable_photos": 3 })
        );

        let (o, id) = owner.lock().unwrap().take().unwrap();
        assert_eq!(
            (o.volunteer_id.as_str(), o.via),
            (admin.id.as_str(), SearchVia::FamilyLink)
        );
        assert_eq!(o.origin_shelter_id.as_deref(), Some(shelter.as_str()));
        assert_eq!(o.key.as_deref(), Some(code.as_str()));
        let link = link_doc(&app, &code).await;
        assert_eq!(
            (link.status, link.search_id.as_deref()),
            (LinkStatus::Used, Some(id.as_str()))
        );
        assert!(link.used_at.is_some() && link.processing_until.is_none());

        // The alert: informational, for the creator, no content.
        let alerts = all_docs(&app.state.db, alerts::COLLECTION).await;
        let alert = alerts
            .iter()
            .find(|a| a["kind"] == "family_search_received")
            .unwrap();
        assert_eq!(alert["informational"], true);
        assert_eq!(alert["volunteer_ids"], json!([admin.id]));
        assert_eq!(alert["for_civil_defense"], false);
        assert_eq!(alert["detail"], json!({ "usable_photos": 3 }));
        let text = alert.to_string();
        assert!(
            !text.contains("Maria")
                && !text.contains("secret notes")
                && !text.contains("5551999990000")
        );
        assert!(
            audit_actions(&app.state.db)
                .await
                .contains(&"search_link.submit".to_string())
        );

        // Used: the pipeline does not run again.
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let again = run_submit(&app.state, &code, None, input(&code), |_, _| async move {
            counted.fetch_add(1, Ordering::SeqCst);
            ok(1)
        })
        .await
        .unwrap_err();
        assert_eq!(
            (again.status, again.code),
            (StatusCode::GONE, "search_link_invalid")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn expired_and_canceled_links_do_not_process() {
        let (app, admin, shelter) = setup().await;
        let expired = make_link(&app, &admin, &shelter).await;
        let mut link = link_doc(&app, &expired).await;
        link.expires_at = store::now() - SignedDuration::from_mins(1);
        store::replace(&app.state.db, COLLECTION, &expired, &link)
            .await
            .unwrap();
        let canceled = make_link(&app, &admin, &shelter).await;
        app.send(
            "DELETE",
            &format!("/v1/search-links/{canceled}"),
            &admin.secret,
            None,
        )
        .await;

        let calls = Arc::new(AtomicUsize::new(0));
        for code in [expired.as_str(), canceled.as_str(), "unknown"] {
            let counted = calls.clone();
            let error = run_submit(&app.state, code, None, input(code), |_, _| async move {
                counted.fetch_add(1, Ordering::SeqCst);
                ok(1)
            })
            .await
            .unwrap_err();
            assert_eq!(error.code, "search_link_invalid");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn degraded_unico_keeps_the_link_and_spends_no_attempt() {
        let (app, admin, shelter) = setup().await;
        let code = make_link(&app, &admin, &shelter).await;
        for _ in 0..(MAX_ATTEMPTS + 2) {
            let error = run_submit(&app.state, &code, None, input(&code), |_, _| async {
                Err(ApiError::unico_degraded())
            })
            .await
            .unwrap_err();
            assert_eq!(
                (error.status, error.code),
                (StatusCode::SERVICE_UNAVAILABLE, "unico_degraded")
            );
        }
        let link = link_doc(&app, &code).await;
        assert_eq!((link.status, link.attempts), (LinkStatus::Active, 0));
        assert!(link.processing_until.is_none());
        assert_eq!(
            public(&app, "GET", &format!("/v1/search-links/{code}"), None)
                .await
                .0,
            StatusCode::OK
        );
        assert!(submit(&app, &code).await.is_ok());
    }

    #[tokio::test]
    async fn the_link_gives_up_after_max_failed_attempts() {
        let (app, admin, shelter) = setup().await;
        let code = make_link(&app, &admin, &shelter).await;
        for n in 1..=MAX_ATTEMPTS {
            let error = run_submit(&app.state, &code, None, input(&code), |_, _| async {
                Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "no_face"))
            })
            .await
            .unwrap_err();
            assert_eq!(error.code, "no_face");
            assert_eq!(link_doc(&app, &code).await.attempts, n);
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let error = run_submit(&app.state, &code, None, input(&code), |_, _| async move {
            counted.fetch_add(1, Ordering::SeqCst);
            ok(1)
        })
        .await
        .unwrap_err();
        assert_eq!(
            (error.status, error.code),
            (StatusCode::GONE, "search_link_invalid")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            public(&app, "GET", &format!("/v1/search-links/{code}"), None)
                .await
                .0,
            StatusCode::GONE
        );
    }

    #[tokio::test]
    async fn concurrent_submits_give_exactly_one_success() {
        let (app, admin, shelter) = setup().await;
        let code = make_link(&app, &admin, &shelter).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let (state, code, calls) = (app.state.clone(), code.clone(), calls.clone());
                tokio::spawn(async move {
                    run_submit(&state, &code, None, input(&code), |_, _| async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        ok(1)
                    })
                    .await
                })
            })
            .collect();
        let mut successes = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(_) => successes += 1,
                Err(e) => assert!(
                    matches!(e.code, "search_link_invalid" | store::CONTENTION),
                    "unexpected {}",
                    e.code
                ),
            }
        }
        assert_eq!(successes, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(link_doc(&app, &code).await.status, LinkStatus::Used);
        let alerts = all_docs(&app.state.db, alerts::COLLECTION).await;
        assert_eq!(
            alerts
                .iter()
                .filter(|a| a["kind"] == "family_search_received")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn the_search_id_and_key_are_the_same_on_a_retry() {
        let (app, admin, shelter) = setup().await;
        let code = make_link(&app, &admin, &shelter).await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        for fail in [true, false] {
            let seen = seen.clone();
            let _ = run_submit(&app.state, &code, None, input(&code), |o, i| async move {
                seen.lock().unwrap().push((i.id, o.key));
                if fail {
                    Err(ApiError::unico_degraded())
                } else {
                    ok(1)
                }
            })
            .await;
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0], seen[1]);
        assert_eq!(seen[0].1.as_deref(), Some(code.as_str()));
    }

    // ---- limits of the public route (refused before Unico and before the link is touched) ----

    #[tokio::test]
    async fn an_image_above_800_kb_is_refused_without_touching_the_link_or_unico() {
        let (app, admin, shelter) = setup().await;
        let logins = app.unico.post_count();
        let code = make_link(&app, &admin, &shelter).await;
        let path = format!("/v1/search-links/{code}/submit");
        let (status, answer) = public(
            &app,
            "POST",
            &path,
            Some(body(vec![photo(MAX_IMAGE_BYTES + 1)])),
        )
        .await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::PAYLOAD_TOO_LARGE, Some("image_too_large"))
        );
        let link = link_doc(&app, &code).await;
        assert_eq!((link.status, link.attempts), (LinkStatus::Active, 0));
        assert_eq!(app.unico.post_count(), logins);
    }

    #[tokio::test]
    async fn the_body_is_validated() {
        let (app, admin, shelter) = setup().await;
        let logins = app.unico.post_count();
        let code = make_link(&app, &admin, &shelter).await;
        let path = format!("/v1/search-links/{code}/submit");
        let bad = |photos: Vec<String>| body(photos);
        for (what, request) in [
            ("no photos", bad(vec![])),
            ("six photos", bad(vec![photo(10); 6])),
            ("not base64", bad(vec!["***".into()])),
        ] {
            let (status, _) = public(&app, "POST", &path, Some(request)).await;
            assert!(
                status == StatusCode::UNPROCESSABLE_ENTITY,
                "{what}: {status}"
            );
        }
        let mut request = body(vec![photo(10)]);
        request["family"]["language"] = json!("fr");
        assert_eq!(
            public(&app, "POST", &path, Some(request)).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let mut request = body(vec![photo(10)]);
        request["missing_person"]["name"] = json!("  ");
        assert_eq!(
            public(&app, "POST", &path, Some(request)).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(link_doc(&app, &code).await.attempts, 0);
        assert_eq!(app.unico.post_count(), logins);
    }

    #[tokio::test]
    async fn a_dead_link_answers_410_through_the_route() {
        let (app, _, _) = setup().await;
        let (status, answer) = public(
            &app,
            "POST",
            "/v1/search-links/nope/submit",
            Some(body(vec![photo(10)])),
        )
        .await;
        // Until the search module lands `register_photos` answers 501, but a dead link never gets there.
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::GONE, Some("search_link_invalid"))
        );
    }

    #[tokio::test]
    async fn submits_are_limited_per_device_and_per_ip() {
        let (app, _, _) = setup().await;
        let path = "/v1/search-links/nope/submit";
        let ip = format!("ip-{}", new_id());
        let mut statuses = Vec::new();
        for _ in 0..=MAX_PER_DEVICE_HOUR {
            let (status, _, _) = app
                .request(
                    "POST",
                    path,
                    &[("x-forwarded-for", ip.as_str()), ("x-device-id", "d1")],
                    Some(body(vec![photo(10)])),
                )
                .await;
            statuses.push(status);
        }
        assert!(
            statuses[..MAX_PER_DEVICE_HOUR]
                .iter()
                .all(|s| *s == StatusCode::GONE)
        );
        assert_eq!(statuses[MAX_PER_DEVICE_HOUR], StatusCode::TOO_MANY_REQUESTS);
        // Another device on the same IP still gets through (until the IP's own ceiling).
        let (status, _, _) = app
            .request(
                "POST",
                path,
                &[("x-forwarded-for", ip.as_str()), ("x-device-id", "d2")],
                Some(body(vec![photo(10)])),
            )
            .await;
        assert_eq!(status, StatusCode::GONE);
        // A client-made first entry of X-Forwarded-For does not dodge the limit: the last one counts.
        let spoof = format!("1.2.3.{}, {ip}", 4);
        let (status, _, _) = app
            .request(
                "POST",
                path,
                &[("x-forwarded-for", spoof.as_str()), ("x-device-id", "d1")],
                Some(body(vec![photo(10)])),
            )
            .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    }
}
