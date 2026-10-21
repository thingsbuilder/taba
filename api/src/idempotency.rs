//! Server-side idempotency (spec 8, 9.4): a write sent again with the same `Idempotency-Key`
//! gets the response of the first one, and nothing runs twice. The app uses the id of the
//! queue item as the key, so a retry after a lost response is harmless.
//!
//! Only successful (`2xx`) responses are kept. A refusal such as `409 shelter_nearby` can have
//! a different answer when the app sends the item again with an explanation, and it changed
//! nothing the second run could repeat. Keys apply to signed-in writes only: the public routes
//! (login, invite acceptance) are protected by their single-use captures.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use firestore::FirestoreInstant;
use firestore::jiff::SignedDuration;
use serde::{Deserialize, Serialize};

use crate::auth::{AppState, SESSIONS, SessionDoc, session_id};
use crate::error::ApiError;
use crate::store;

pub const COLLECTION: &str = "idempotency";
const TTL: SignedDuration = SignedDuration::from_hours(24 * 7);
const MAX_KEY_CHARS: usize = 128;
/// Larger responses are not kept; none of the writes answers with more than a few KB.
const MAX_STORED_BODY_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdempotencyDoc {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volunteer_id: Option<String>,
    pub route: String,
    pub http_status: u16,
    /// The JSON text of the response body.
    pub body: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub expires_at: FirestoreInstant,
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

pub async fn layer(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let Some(key) = key_of(request.headers()).map(str::to_string) else {
        return next.run(request).await;
    };
    if !is_write(request.method()) {
        return next.run(request).await;
    }
    if key.chars().count() > MAX_KEY_CHARS || key.contains('/') {
        return ApiError::new(StatusCode::BAD_REQUEST, "invalid_idempotency_key").into_response();
    }
    let Some(volunteer_id) = signed_in_as(&state, request.headers()).await else {
        return next.run(request).await;
    };
    let route = format!("{} {}", request.method(), request.uri().path());

    // Two copies of one request run one after the other, so the second finds the first's answer.
    let lock = {
        let mut locks = state
            .idempotency_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks.entry(key.clone()).or_default().clone()
    };
    let response = {
        let _turn = lock.lock().await;
        serve(&state, &key, &volunteer_id, &route, request, next).await
    };
    let mut locks = state
        .idempotency_locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // The map and this function hold one reference each; more means someone is waiting.
    if std::sync::Arc::strong_count(&lock) == 2 {
        locks.remove(&key);
    }
    response
}

async fn serve(
    state: &AppState,
    key: &str,
    volunteer_id: &str,
    route: &str,
    request: Request,
    next: Next,
) -> Response {
    match store::get::<IdempotencyDoc>(&state.db, COLLECTION, key).await {
        Ok(Some(doc)) if store::now() < doc.expires_at => {
            // The key was used for something else: never hand out another request's answer.
            if doc.volunteer_id.as_deref() != Some(volunteer_id) || doc.route != route {
                return ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_mismatch")
                    .into_response();
            }
            return replay(&doc);
        }
        Ok(_) => {}
        Err(error) => return error.into_response(),
    }

    let response = next.run(request).await;
    if !response.status().is_success() {
        return response;
    }
    let (parts, body) = response.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_STORED_BODY_BYTES).await else {
        tracing::warn!(key, "response too large to keep for idempotency");
        return ApiError::internal().into_response();
    };
    let doc = IdempotencyDoc {
        volunteer_id: Some(volunteer_id.into()),
        route: route.into(),
        http_status: parts.status.as_u16(),
        body: String::from_utf8_lossy(&bytes).into_owned(),
        expires_at: store::now() + TTL,
    };
    if let Err(error) = store::replace(&state.db, COLLECTION, key, &doc).await {
        tracing::warn!(
            key,
            code = error.code,
            "could not keep the response for idempotency"
        );
    }
    Response::from_parts(parts, Body::from(bytes))
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
        assert!(
            app.state.idempotency_locks.lock().unwrap().is_empty(),
            "the lock table is cleaned up"
        );
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
