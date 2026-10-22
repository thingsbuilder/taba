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

use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use firestore::jiff::SignedDuration;
use firestore::{FirestoreDb, FirestoreInstant};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::auth::{AppState, SESSIONS, SessionDoc, session_id};
use crate::error::ApiError;
use crate::store;

pub const COLLECTION: &str = "idempotency";
const TTL: SignedDuration = SignedDuration::from_hours(24 * 7);
const MAX_KEY_CHARS: usize = 128;
/// Larger responses are not kept; none of the writes answers with more than a few KB.
const MAX_STORED_BODY_BYTES: usize = 256 * 1024;
/// Larger than the body limit of the router, which is applied first.
const MAX_REQUEST_BODY_BYTES: usize = 8 * 1024 * 1024;
/// How long a claim is held. A request that runs longer than this can be claimed again by a
/// copy sent meanwhile (the slowest write, a photo through Unico, takes seconds).
const LEASE: SignedDuration = SignedDuration::from_secs(60);
/// How long a copy waits for the request that holds the claim before answering 503.
const WAIT: Duration = if cfg!(test) {
    Duration::from_secs(2)
} else {
    Duration::from_secs(10)
};
const POLL: Duration = Duration::from_millis(50);
/// Set by `layer` on the requests it serves (a value sent by the client is dropped): the digest
/// of the request, which `photo_person_id` binds the paid answer to.
const DIGEST_HEADER: &str = "x-taba-request-digest";

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
struct Asker {
    volunteer_id: String,
    route: String,
    fingerprint: String,
}

impl IdempotencyDoc {
    /// A claim whose lease ran out without a receipt.
    fn lapsed(&self, now: FirestoreInstant) -> bool {
        self.state == ClaimState::Pending && self.lease_until.is_none_or(|until| until <= now)
    }

    /// The refusal for a request that is not the one this record belongs to.
    fn refusal(&self, asker: &Asker) -> Option<ApiError> {
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

fn is_write(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn key_of(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn usable_key(key: &str) -> bool {
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

/// Who the request is signed in as, by the session alone (the handler checks the rest).
async fn signed_in_as(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let secret = headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    let session: SessionDoc = store::get(&state.db, SESSIONS, &session_id(secret))
        .await
        .ok()??;
    (store::now() < session.expires_at).then_some(session.volunteer_id)
}

/// `POST /v1/matches/{id}/decision`, whose answer carries the contact of the family.
fn is_match_decision(route: &str) -> bool {
    route.starts_with("POST /v1/matches/") && route.ends_with("/decision")
}

fn replay(doc: &IdempotencyDoc) -> Response {
    let status = StatusCode::from_u16(doc.http_status).unwrap_or(StatusCode::OK);
    if doc.body.is_empty() {
        return status.into_response();
    }
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        doc.body.clone(),
    )
        .into_response()
}

// ---- the claim: rules and persistence, no HTTP ----

enum Claim {
    /// The key is ours; the number is this claim's `attempts`.
    Won(u32),
    /// The key belongs to a record, `pending` with a live lease or `done`.
    Existing(IdempotencyDoc),
}

/// Creates the `pending` record of the key, or takes over one whose lease lapsed (same asker,
/// same payload), or returns what is there. A record past its `expires_at` counts as absent.
async fn claim(db: &FirestoreDb, key: &str, asker: &Asker) -> Result<Claim, ApiError> {
    // A copy that waits only reads, without a transaction: its reads must not hold the document
    // against the request that is about to settle it.
    let found = store::get::<IdempotencyDoc>(db, COLLECTION, key).await?;
    let now = store::now();
    let live = found.filter(|d| now < d.expires_at);
    match live {
        Some(doc) if !doc.lapsed(now) => Ok(Claim::Existing(doc)),
        Some(_) => take_over(db, key, asker).await,
        None => {
            let pending = pending_for(asker, 1);
            // `create` fails when another copy got there first; the next round reads its record.
            // An expired record is replaced in a transaction, as a lapsed one is.
            if store::create(db, COLLECTION, key, &pending).await? {
                Ok(Claim::Won(1))
            } else {
                take_over(db, key, asker).await
            }
        }
    }
}

fn pending_for(asker: &Asker, attempts: u32) -> IdempotencyDoc {
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

/// Claims the key in a transaction that reads it again: a lapsed claim of the same request, or
/// an expired record, becomes ours; anything else is returned as it is.
async fn take_over(db: &FirestoreDb, key: &str, asker: &Asker) -> Result<Claim, ApiError> {
    store::transact(db, |db, tx| {
        let (key, asker) = (key.to_string(), asker.clone());
        Box::pin(async move {
            let now = store::now();
            let found = store::get::<IdempotencyDoc>(&db, COLLECTION, &key).await?;
            let live = found.as_ref().filter(|d| now < d.expires_at);
            if let Some(doc) = live {
                if !doc.lapsed(now) || doc.refusal(&asker).is_some() {
                    return Ok(Claim::Existing(doc.clone()));
                }
            }
            let attempts = live.map_or(1, |d| d.attempts + 1);
            let pending = pending_for(&asker, attempts);
            if found.is_some() {
                store::tx_replace(&db, tx, COLLECTION, &key, &pending)?;
            } else {
                store::tx_create(&db, tx, COLLECTION, &key, &pending)?;
            }
            Ok(Claim::Won(attempts))
        })
    })
    .await
}

/// Ends claim number `attempt`: stores the answer, or (`None`) deletes the claim. A claim that
/// another request took over meanwhile is not touched.
async fn settle(
    db: &FirestoreDb,
    key: &str,
    attempt: u32,
    answer: Option<(u16, String)>,
) -> Result<(), ApiError> {
    store::transact(db, |db, tx| {
        let (key, answer) = (key.to_string(), answer.clone());
        Box::pin(async move {
            let Some(mut doc) = store::get::<IdempotencyDoc>(&db, COLLECTION, &key).await? else {
                return Ok(());
            };
            if doc.state != ClaimState::Pending || doc.attempts != attempt {
                return Ok(());
            }
            match answer {
                Some((http_status, body)) => {
                    doc.state = ClaimState::Done;
                    doc.lease_until = None;
                    doc.http_status = http_status;
                    doc.body = body;
                    store::tx_replace(&db, tx, COLLECTION, &key, &doc)
                }
                None => store::tx_delete(&db, tx, COLLECTION, &key),
            }
        })
    })
    .await
}

// ---- the layer: HTTP ----

pub async fn layer(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    request.headers_mut().remove(DIGEST_HEADER);
    let Some(key) = key_of(request.headers()).map(str::to_string) else {
        return next.run(request).await;
    };
    if !is_write(request.method()) {
        return next.run(request).await;
    }
    if !usable_key(&key) {
        return ApiError::new(StatusCode::BAD_REQUEST, "invalid_idempotency_key").into_response();
    }
    let Some(volunteer_id) = signed_in_as(&state, request.headers()).await else {
        return next.run(request).await;
    };
    let route = format!("{} {}", request.method(), request.uri().path());
    let (mut parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_REQUEST_BODY_BYTES).await else {
        return ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large").into_response();
    };
    let fingerprint = digest_of(&[
        parts.method.as_str().as_bytes(),
        parts.uri.path().as_bytes(),
        parts.uri.query().unwrap_or("").as_bytes(),
        &bytes,
    ]);
    if let Ok(value) = HeaderValue::from_str(&fingerprint) {
        parts.headers.insert(DIGEST_HEADER, value);
    }
    let asker = Asker {
        volunteer_id,
        route,
        fingerprint,
    };
    serve(
        &state,
        &key,
        &asker,
        Request::from_parts(parts, Body::from(bytes)),
        next,
    )
    .await
}

async fn serve(
    state: &AppState,
    key: &str,
    asker: &Asker,
    request: Request,
    next: Next,
) -> Response {
    let give_up = tokio::time::Instant::now() + WAIT;
    let attempt = loop {
        match claim(&state.db, key, asker).await {
            Err(error) => return error.into_response(),
            Ok(Claim::Won(attempt)) => break attempt,
            Ok(Claim::Existing(doc)) => {
                if let Some(refusal) = doc.refusal(asker) {
                    return refusal.into_response();
                }
                if doc.state == ClaimState::Done {
                    return replay(&doc);
                }
                if tokio::time::Instant::now() >= give_up {
                    let mut busy =
                        ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "idempotency_in_progress");
                    busy.retry_after = Some(1);
                    return busy.into_response();
                }
                tokio::time::sleep(POLL).await;
            }
        }
    };

    let response = next.run(request).await;
    if !response.status().is_success() {
        release(state, key, attempt).await;
        return response;
    }
    let (parts, body) = response.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_STORED_BODY_BYTES).await else {
        tracing::warn!(key, "response too large to keep for idempotency");
        release(state, key, attempt).await;
        return ApiError::internal().into_response();
    };
    // The decision of a match answers with the family's phone: it is not kept for a week. A retry
    // finds the match decided, which is also what it is.
    let (http_status, body) = if is_match_decision(&asker.route) {
        let decided = ApiError::match_already_decided();
        (
            decided.status.as_u16(),
            serde_json::json!({ "error": decided.code, "detail": {} }).to_string(),
        )
    } else {
        (
            parts.status.as_u16(),
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    };
    if let Err(error) = settle(&state.db, key, attempt, Some((http_status, body))).await {
        tracing::warn!(
            key,
            code = error.code,
            "could not keep the response for idempotency; the claim stays pending until its lease lapses"
        );
    }
    Response::from_parts(parts, Body::from(bytes))
}

/// Gives the key back after a refusal, so the same item can be sent again.
async fn release(state: &AppState, key: &str, attempt: u32) {
    if let Err(error) = settle(&state.db, key, attempt, None).await {
        tracing::warn!(
            key,
            code = error.code,
            "could not release the idempotency claim; it holds until its lease lapses"
        );
    }
}

/// What a paid call to Unico about a photo said, kept for the key of the request that made it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PhotoResult {
    volunteer_id: String,
    route: String,
    /// What the answer is bound to: a digest of the request (or of the photo). A key sent again
    /// with other content gets no answer from here.
    #[serde(default)]
    digest: String,
    /// The HMAC of the token, never the token.
    person_id: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    expires_at: FirestoreInstant,
}

/// The person a photo belongs to, asking Unico (a paid call) once per `Idempotency-Key` and
/// request content. The handlers that send a photo call Unico before their transaction, so a
/// request that fails after it (contention, a refusal) and is sent again with the same key would
/// pay a second time; the answer of the first call is kept under the key for as long as the key
/// lives and the retry reuses it (spec 8, 9.4). Without a usable key, or without the digest the
/// layer sets on the request, it simply asks.
pub async fn photo_person_id<F>(
    state: &AppState,
    headers: &HeaderMap,
    volunteer_id: &str,
    route: &str,
    ask: F,
) -> Result<String, ApiError>
where
    F: std::future::Future<Output = Result<String, ApiError>>,
{
    let digest = headers.get(DIGEST_HEADER).and_then(|v| v.to_str().ok());
    photo_person_id_of(state, headers, volunteer_id, route, digest, ask).await
}

/// `photo_person_id` for a caller that has its own digest of the photo.
pub async fn photo_person_id_of<F>(
    state: &AppState,
    headers: &HeaderMap,
    volunteer_id: &str,
    route: &str,
    digest: Option<&str>,
    ask: F,
) -> Result<String, ApiError>
where
    F: std::future::Future<Output = Result<String, ApiError>>,
{
    let key = key_of(headers).filter(|k| usable_key(k));
    let (Some(key), Some(digest)) = (key, digest.filter(|d| !d.is_empty())) else {
        return ask.await;
    };
    let id = format!("photo~{key}");
    if let Ok(Some(kept)) = store::get::<PhotoResult>(&state.db, COLLECTION, &id).await {
        if store::now() < kept.expires_at
            && kept.volunteer_id == volunteer_id
            && kept.route == route
            && kept.digest == digest
        {
            return Ok(kept.person_id);
        }
    }
    let person_id = ask.await?;
    let kept = PhotoResult {
        volunteer_id: volunteer_id.into(),
        route: route.into(),
        digest: digest.into(),
        person_id: person_id.clone(),
        expires_at: store::now() + TTL,
    };
    if let Err(error) = store::replace(&state.db, COLLECTION, &id, &kept).await {
        tracing::warn!(
            code = error.code,
            "could not keep the answer of Unico for the key"
        );
    }
    Ok(person_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        ORIGIN, TestApp, all_docs, audit_actions, new_id, north_of_origin, shelter_body,
    };
    use serde_json::{Value, json};

    async fn stored(app: &TestApp, key: &str) -> Option<IdempotencyDoc> {
        store::get(&app.state.db, COLLECTION, key).await.unwrap()
    }

    #[test]
    fn only_writes_with_a_usable_key_are_considered() {
        assert!(
            is_write(&Method::POST)
                && is_write(&Method::PATCH)
                && is_write(&Method::PUT)
                && is_write(&Method::DELETE)
        );
        assert!(!is_write(&Method::GET) && !is_write(&Method::HEAD));
        let mut headers = HeaderMap::new();
        assert_eq!(key_of(&headers), None);
        headers.insert("idempotency-key", " abc ".parse().unwrap());
        assert_eq!(key_of(&headers), Some("abc"));
        headers.insert("idempotency-key", "  ".parse().unwrap());
        assert_eq!(key_of(&headers), None);
    }

    #[tokio::test]
    async fn the_same_key_gives_the_same_response_and_nothing_runs_twice() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let key = new_id();
        let body = shelter_body(&new_id(), "Ginásio", ORIGIN);
        let (status, first) = app
            .send_keyed(
                "POST",
                "/v1/shelters",
                &ana.secret,
                &key,
                Some(body.clone()),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{first}");
        let audits = audit_actions(&app.state.db).await.len();

        let (status, again) = app
            .send_keyed("POST", "/v1/shelters", &ana.secret, &key, Some(body))
            .await;
        assert_eq!((status, &again), (StatusCode::CREATED, &first));
        assert_eq!(audit_actions(&app.state.db).await.len(), audits);
        assert_eq!(all_docs(&app.state.db, "shelters").await.len(), 1);
    }

    #[tokio::test]
    async fn the_same_key_does_not_run_an_effect_twice_even_when_the_request_is_not_naturally_repeatable()
     {
        // Creating an invite makes a new code every time; the key is what keeps it to one.
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let shelter = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let path = format!("/v1/shelters/{shelter}/invites");
        let key = new_id();
        let body = json!({ "kind": "qr", "role": "volunteer" });
        let (_, first) = app
            .send_keyed("POST", &path, &ana.secret, &key, Some(body.clone()))
            .await;
        let (_, again) = app
            .send_keyed("POST", &path, &ana.secret, &key, Some(body.clone()))
            .await;
        assert_eq!(again["code"], first["code"]);
        assert_eq!(all_docs(&app.state.db, "invites").await.len(), 1);
        // Without a key, a repeat is a new request.
        app.send("POST", &path, &ana.secret, Some(body)).await;
        assert_eq!(all_docs(&app.state.db, "invites").await.len(), 2);
    }

    #[tokio::test]
    async fn copies_of_a_request_sent_at_once_run_once() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let shelter = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let path = format!("/v1/shelters/{shelter}/invites");
        let key = new_id();
        let mut handles = Vec::new();
        for _ in 0..6 {
            let (app, path, secret, key) =
                (app.clone(), path.clone(), ana.secret.clone(), key.clone());
            handles.push(tokio::spawn(async move {
                app.send_keyed(
                    "POST",
                    &path,
                    &secret,
                    &key,
                    Some(json!({ "kind": "link", "role": "volunteer" })),
                )
                .await
            }));
        }
        let mut codes = Vec::new();
        for handle in handles {
            let (status, body) = handle.await.unwrap();
            assert_eq!(status, StatusCode::CREATED, "{body}");
            codes.push(body["code"].as_str().unwrap().to_string());
        }
        codes.dedup();
        assert_eq!(codes.len(), 1, "every copy got the first answer");
        assert_eq!(all_docs(&app.state.db, "invites").await.len(), 1);
    }

    #[tokio::test]
    async fn copies_sent_to_two_instances_at_once_run_once() {
        // Two instances share nothing but the database: no in-memory lock can serialize them.
        let one = TestApp::new().await;
        let two = TestApp::with_db(one.state.db.clone()).await;
        let ana = one.actor("Ana").await;
        let shelter = one.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let path = format!("/v1/shelters/{shelter}/invites");
        let key = new_id();
        let mut handles = Vec::new();
        for n in 0..6 {
            let (app, path, secret, key) = (
                if n % 2 == 0 { one.clone() } else { two.clone() },
                path.clone(),
                ana.secret.clone(),
                key.clone(),
            );
            handles.push(tokio::spawn(async move {
                app.send_keyed(
                    "POST",
                    &path,
                    &secret,
                    &key,
                    Some(json!({ "kind": "link", "role": "volunteer" })),
                )
                .await
            }));
        }
        let mut codes = Vec::new();
        for handle in handles {
            let (status, body) = handle.await.unwrap();
            assert_eq!(status, StatusCode::CREATED, "{body}");
            codes.push(body["code"].as_str().unwrap().to_string());
        }
        codes.dedup();
        assert_eq!(codes.len(), 1, "every copy got the first answer");
        assert_eq!(all_docs(&one.state.db, "invites").await.len(), 1);
        assert_eq!(stored(&one, &key).await.unwrap().attempts, 1);
    }

    #[tokio::test]
    async fn the_same_key_with_another_payload_is_a_conflict_and_changes_nothing() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let key = new_id();
        let first = shelter_body(&new_id(), "Ginásio", ORIGIN);
        let (status, _) = app
            .send_keyed("POST", "/v1/shelters", &ana.secret, &key, Some(first))
            .await;
        assert_eq!(status, StatusCode::CREATED);

        let other = shelter_body(&new_id(), "Outro nome", north_of_origin(5_000.0));
        let (status, body) = app
            .send_keyed("POST", "/v1/shelters", &ana.secret, &key, Some(other))
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::CONFLICT, Some("idempotency_key_reused"))
        );
        assert_eq!(all_docs(&app.state.db, "shelters").await.len(), 1);
    }

    #[tokio::test]
    async fn a_claim_that_lapsed_after_the_commit_runs_again_and_records_the_second_attempt() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let key = new_id();
        let body = shelter_body(&new_id(), "Ginásio", ORIGIN);
        let (status, first) = app
            .send_keyed(
                "POST",
                "/v1/shelters",
                &ana.secret,
                &key,
                Some(body.clone()),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
        // The process died after the commit and before the receipt: the claim is still pending
        // and its lease ran out.
        let mut doc = stored(&app, &key).await.unwrap();
        doc.state = ClaimState::Pending;
        doc.http_status = 0;
        doc.body.clear();
        doc.lease_until = Some(store::now() - SignedDuration::from_secs(1));
        store::replace(&app.state.db, COLLECTION, &key, &doc)
            .await
            .unwrap();

        let (status, again) = app
            .send_keyed("POST", "/v1/shelters", &ana.secret, &key, Some(body))
            .await;
        assert_eq!((status, &again), (StatusCode::CREATED, &first));
        assert_eq!(
            all_docs(&app.state.db, "shelters").await.len(),
            1,
            "the handler is idempotent by the id of the shelter"
        );
        let doc = stored(&app, &key).await.unwrap();
        assert_eq!((doc.state, doc.attempts), (ClaimState::Done, 2));
    }

    #[tokio::test]
    async fn a_copy_of_a_request_still_running_waits_and_then_asks_to_retry() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let key = new_id();
        let body = shelter_body(&new_id(), "Ginásio", ORIGIN);
        let live = IdempotencyDoc {
            volunteer_id: Some(ana.id.clone()),
            route: "POST /v1/shelters".into(),
            fingerprint: digest_of(&[b"POST", b"/v1/shelters", b"", body.to_string().as_bytes()]),
            state: ClaimState::Pending,
            attempts: 1,
            lease_until: Some(store::now() + SignedDuration::from_secs(60)),
            http_status: 0,
            body: String::new(),
            expires_at: store::now() + TTL,
        };
        store::replace(&app.state.db, COLLECTION, &key, &live)
            .await
            .unwrap();
        let (status, headers, answer) = app
            .request(
                "POST",
                "/v1/shelters",
                &[
                    ("authorization", format!("Bearer {}", ana.secret).as_str()),
                    ("idempotency-key", key.as_str()),
                ],
                Some(body),
            )
            .await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Some("idempotency_in_progress")
            )
        );
        assert_eq!(headers.get("retry-after").unwrap(), "1");
        assert!(all_docs(&app.state.db, "shelters").await.is_empty());
    }

    #[tokio::test]
    async fn a_replay_never_reaches_further_than_the_session_that_sends_it() {
        let app = TestApp::new().await;
        let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
        let shelter = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let path = format!("/v1/shelters/{shelter}/invites");
        let key = new_id();
        let body = json!({ "kind": "qr", "role": "volunteer" });
        let (status, first) = app
            .send_keyed("POST", &path, &ana.secret, &key, Some(body.clone()))
            .await;
        assert_eq!(status, StatusCode::CREATED);
        let code = first["code"].as_str().unwrap();

        // Another volunteer with the same key, route and payload does not get Ana's invite.
        let (status, stolen) = app
            .send_keyed("POST", &path, &bia.secret, &key, Some(body.clone()))
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!stolen.to_string().contains(code));
        // Nor does a session that ended.
        app.send("DELETE", "/v1/sessions/current", &ana.secret, None)
            .await;
        let (status, gone) = app
            .send_keyed("POST", &path, &ana.secret, &key, Some(body))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(!gone.to_string().contains(code));
    }

    #[tokio::test]
    async fn the_paid_answer_of_a_photo_is_reused_only_for_the_same_content() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", "photo-key".parse().unwrap());
        let asks = std::sync::atomic::AtomicU32::new(0);
        let ask = |person: &'static str| {
            let asks = &asks;
            async move {
                asks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(person.to_string())
            }
        };
        let route = "POST /v1/rescues";
        let (d1, d2) = (digest_of(&[b"photo one"]), digest_of(&[b"photo two"]));
        let call = |digest: String, person| {
            let (state, headers, id, ask) = (&app.state, &headers, &ana.id, ask(person));
            async move { photo_person_id_of(state, headers, id, route, Some(&digest), ask).await }
        };
        assert_eq!(call(d1.clone(), "p1").await.unwrap(), "p1");
        // The retry of the same content does not ask again.
        assert_eq!(call(d1.clone(), "other").await.unwrap(), "p1");
        // The same key with another photo never gets the first person.
        assert_eq!(call(d2.clone(), "p2").await.unwrap(), "p2");
        assert_eq!(asks.load(std::sync::atomic::Ordering::SeqCst), 2);
        // Without a digest it asks and keeps nothing.
        let nothing =
            photo_person_id_of(&app.state, &headers, &ana.id, route, None, ask("p3")).await;
        assert_eq!(nothing.unwrap(), "p3");
        assert_eq!(asks.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn the_digest_tells_apart_parts_that_run_together() {
        assert_ne!(digest_of(&[b"ab", b"c"]), digest_of(&[b"a", b"bc"]));
        assert_eq!(digest_of(&[b"x"]), digest_of(&[b"x"]));
    }

    #[tokio::test]
    async fn the_response_is_kept_for_seven_days_with_who_and_where() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let key = new_id();
        let id = new_id();
        let (_, answer) = app
            .send_keyed(
                "POST",
                "/v1/shelters",
                &ana.secret,
                &key,
                Some(shelter_body(&id, "Ginásio", ORIGIN)),
            )
            .await;
        let doc = stored(&app, &key).await.unwrap();
        assert_eq!(
            (
                doc.volunteer_id.as_deref(),
                doc.route.as_str(),
                doc.http_status
            ),
            (Some(ana.id.as_str()), "POST /v1/shelters", 201)
        );
        assert_eq!(serde_json::from_str::<Value>(&doc.body).unwrap(), answer);
        let ttl = doc.expires_at.as_second() - store::now().as_second();
        assert!((ttl - 7 * 24 * 3600).abs() <= 2, "{ttl}");
    }

    #[tokio::test]
    async fn a_refusal_is_not_kept_so_the_same_item_can_be_sent_again_with_an_explanation() {
        let app = TestApp::new().await;
        let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
        app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let key = new_id();
        let mut body = shelter_body(&new_id(), "Escola", north_of_origin(80.0));
        let (status, refused) = app
            .send_keyed(
                "POST",
                "/v1/shelters",
                &bia.secret,
                &key,
                Some(body.clone()),
            )
            .await;
        assert_eq!(
            (status, refused["error"].as_str()),
            (StatusCode::CONFLICT, Some("shelter_nearby"))
        );
        assert!(stored(&app, &key).await.is_none());

        body["other_site_reason"] = json!("Outro prédio");
        let (status, created) = app
            .send_keyed("POST", "/v1/shelters", &bia.secret, &key, Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert!(stored(&app, &key).await.is_some());
    }

    #[tokio::test]
    async fn a_key_reused_for_something_else_never_returns_the_first_answer() {
        let app = TestApp::new().await;
        let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
        let key = new_id();
        app.send_keyed(
            "POST",
            "/v1/shelters",
            &ana.secret,
            &key,
            Some(shelter_body(&new_id(), "Ginásio", ORIGIN)),
        )
        .await;

        // Another volunteer with the same key.
        let (status, body) = app
            .send_keyed(
                "POST",
                "/v1/shelters",
                &bia.secret,
                &key,
                Some(shelter_body(&new_id(), "Outro", north_of_origin(5_000.0))),
            )
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("idempotency_key_mismatch")
            )
        );
        assert!(!body.to_string().contains("Ginásio"));
        // The same volunteer, another route.
        let (status, body) = app
            .send_keyed(
                "PATCH",
                "/v1/me",
                &ana.secret,
                &key,
                Some(json!({ "name": "Ana B" })),
            )
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("idempotency_key_mismatch")
            )
        );
        assert_eq!(all_docs(&app.state.db, "shelters").await.len(), 1);
        assert_eq!(
            app.get_authed("/v1/me", &ana.secret).await.1["volunteer"]["name"],
            "Ana"
        );
    }

    #[tokio::test]
    async fn an_expired_record_is_ignored_while_the_ttl_has_not_deleted_it() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let key = new_id();
        let body = shelter_body(&new_id(), "Ginásio", ORIGIN);
        app.send_keyed(
            "POST",
            "/v1/shelters",
            &ana.secret,
            &key,
            Some(body.clone()),
        )
        .await;
        let mut doc = stored(&app, &key).await.unwrap();
        doc.route = "POST /v1/elsewhere".into();
        doc.expires_at = store::now() - SignedDuration::from_secs(1);
        store::replace(&app.state.db, COLLECTION, &key, &doc)
            .await
            .unwrap();
        // It would be a mismatch if it counted; instead the request runs and replaces it.
        let (status, _) = app
            .send_keyed("POST", "/v1/shelters", &ana.secret, &key, Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(stored(&app, &key).await.unwrap().route, "POST /v1/shelters");
    }

    #[tokio::test]
    async fn reads_unsigned_requests_and_bad_keys_are_left_alone() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        // A GET with a key is just a GET.
        let (status, _) = app
            .send_keyed("GET", "/v1/me", &ana.secret, "k1", None)
            .await;
        assert_eq!(status, StatusCode::OK);
        // A write with no session reaches the handler (which refuses it) and keeps nothing.
        let (status, _, _) = app
            .request(
                "PATCH",
                "/v1/me",
                &[("idempotency-key", "k2")],
                Some(json!({ "name": "X" })),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // A key that cannot be a document id is refused.
        for bad in ["a/b".to_string(), "k".repeat(129)] {
            let (status, body) = app
                .send_keyed(
                    "PATCH",
                    "/v1/me",
                    &ana.secret,
                    &bad,
                    Some(json!({ "name": "X" })),
                )
                .await;
            assert_eq!(
                (status, body["error"].as_str()),
                (StatusCode::BAD_REQUEST, Some("invalid_idempotency_key"))
            );
        }
        assert!(all_docs(&app.state.db, COLLECTION).await.is_empty());
    }

    #[tokio::test]
    async fn an_empty_response_is_replayed_empty() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let shelter = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let code = app.invite(&ana, &shelter, "qr", "volunteer").await;
        let key = new_id();
        let path = format!("/v1/invites/{code}");
        assert_eq!(
            app.send_keyed("DELETE", &path, &ana.secret, &key, None)
                .await
                .0,
            StatusCode::NO_CONTENT
        );
        // Without the key the second delete is a 410; with it, the first answer comes back.
        assert_eq!(
            app.send_keyed("DELETE", &path, &ana.secret, &key, None)
                .await
                .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            app.send("DELETE", &path, &ana.secret, None).await.0,
            StatusCode::GONE
        );
    }
}
