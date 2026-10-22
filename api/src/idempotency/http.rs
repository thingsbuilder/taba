//! The middleware that puts a write with an `Idempotency-Key` through the claim, and the replay
//! of the stored answer.

use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::domain::{digest_of, is_match_decision, is_write, key_of, usable_key};
use super::model::{Asker, ClaimState, DIGEST_HEADER, IdempotencyDoc};
use super::persist::{Claim, claim, release, settle, signed_in_as};
use crate::auth::AppState;
use crate::error::ApiError;

/// Larger responses are not kept; none of the writes answers with more than a few KB.
const MAX_STORED_BODY_BYTES: usize = 256 * 1024;
/// Larger than the body limit of the router, which is applied first.
const MAX_REQUEST_BODY_BYTES: usize = 8 * 1024 * 1024;
/// How long a copy waits for the request that holds the claim before answering 503.
const WAIT: Duration = if cfg!(test) {
    Duration::from_secs(2)
} else {
    Duration::from_secs(10)
};
const POLL: Duration = Duration::from_millis(50);

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
