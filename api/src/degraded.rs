//! Degraded mode (spec 5.5, 5.7): `system/status`, the monthly limit, three failures in a row,
//! the `503 unico_degraded` gate and `taba unico resume`.
//!
//! The state lives in one document so that `taba unico resume`, a separate process, takes effect
//! on the next request. `unico.rs` reads it before every call and calls `enter_limit` and
//! `enter_unavailable` from the outcome of the calls.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use firestore::jiff::SignedDuration;
use firestore::{FirestoreDb, FirestoreInstant};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::alerts::{self, AlertDoc, AlertKind};
use crate::auth::AppState;
use crate::error::ApiError;
use crate::status::{Reason, Unico};
use crate::store::{self, tx_replace};

/// Collection and document of the status (spec 7: `system/status`).
pub const COLLECTION: &str = "system";
pub const DOCUMENT: &str = "status";
/// How long `unavailable` lasts before the next real call tests Unico again (spec 5.7).
const UNAVAILABLE_FOR: SignedDuration = SignedDuration::from_mins(5);

/// The whole `system/status` document; `GET /v1/status` shows only `unico` and `reason`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusDoc {
    pub unico: Unico,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub since: Option<FirestoreInstant>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub until: Option<FirestoreInstant>,
}

impl StatusDoc {
    fn degraded(reason: Reason, now: FirestoreInstant, until: Option<FirestoreInstant>) -> Self {
        Self {
            unico: Unico::Degraded,
            reason: Some(reason),
            since: Some(now),
            until,
        }
    }

    /// Why Unico is off limits at `now`, if it is. An `unavailable` period whose `until` has
    /// passed no longer counts: the mode ends by itself (spec 5.7).
    pub fn active_reason(&self, now: FirestoreInstant) -> Option<Reason> {
        match (self.unico, self.reason) {
            (Unico::Degraded, Some(Reason::Limit)) => Some(Reason::Limit),
            (Unico::Degraded, Some(Reason::Unavailable)) if self.until.is_some_and(|u| now < u) => {
                Some(Reason::Unavailable)
            }
            _ => None,
        }
    }
}

pub async fn read(db: &FirestoreDb) -> Result<Option<StatusDoc>, ApiError> {
    store::get(db, COLLECTION, DOCUMENT).await
}

/// Why Unico is off limits right now, if it is.
pub async fn blocking_reason(db: &FirestoreDb) -> Result<Option<Reason>, ApiError> {
    Ok(read(db)
        .await?
        .and_then(|doc| doc.active_reason(store::now())))
}

/// `Err(503 unico_degraded)` while the degraded mode is on. Handlers that reach Unico through
/// `unico.rs` get this from the client; call it before doing any write that would be wasted.
#[allow(dead_code)] // used by the handlers of the routes that depend on Unico
pub async fn ensure_available(db: &FirestoreDb) -> Result<(), ApiError> {
    match blocking_reason(db).await? {
        None => Ok(()),
        Some(_) => Err(ApiError::unico_degraded()),
    }
}

/// To be applied (`route_layer`) to the routes that always need Unico: answers
/// `503 unico_degraded` while the mode is on.
#[allow(dead_code)] // applied by the owners of the routes that depend on Unico
pub async fn require_unico(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    match ensure_available(&state.db).await {
        Ok(()) => next.run(request).await,
        Err(err) => err.into_response(),
    }
}

/// The monthly limit was reached: `degraded`/`limit` until `taba unico resume`, and one
/// `unico_limit` alert for the Civil Defense, written in the same transaction (spec 5.5).
/// Calls that arrive here while the limit is already recorded change nothing.
pub async fn enter_limit(db: &FirestoreDb) -> Result<(), ApiError> {
    store::transact(db, |db, tx| {
        Box::pin(async move {
            let now = store::now();
            let current = read(&db).await?;
            if current.and_then(|doc| doc.active_reason(now)) == Some(Reason::Limit) {
                return Ok(());
            }
            let doc = StatusDoc::degraded(Reason::Limit, now, None);
            tx_replace(&db, tx, COLLECTION, DOCUMENT, &doc)?;
            let alert = AlertDoc::new(AlertKind::UnicoLimit, "system", "unico", json!({}));
            alerts::add(&db, tx, &alert)
        })
    })
    .await
}

/// Three failures in a row: `degraded`/`unavailable` for five minutes. Never replaces the
/// limit (which needs the operator) nor extends a period already running.
pub async fn enter_unavailable(db: &FirestoreDb) -> Result<(), ApiError> {
    store::transact(db, |db, tx| {
        Box::pin(async move {
            let now = store::now();
            if read(&db)
                .await?
                .and_then(|doc| doc.active_reason(now))
                .is_some()
            {
                return Ok(());
            }
            let doc = StatusDoc::degraded(Reason::Unavailable, now, Some(now + UNAVAILABLE_FOR));
            tx_replace(&db, tx, COLLECTION, DOCUMENT, &doc)
        })
    })
    .await
}

/// `taba unico resume`: leaves the degraded mode, whatever the reason. Returns whether the
/// document said `degraded`. The server makes no test call, because every call is billed.
pub async fn resume(db: &FirestoreDb) -> Result<bool, ApiError> {
    store::transact(db, |db, tx| {
        Box::pin(async move {
            let degraded = read(&db)
                .await?
                .is_some_and(|doc| doc.unico == Unico::Degraded);
            if degraded {
                let doc = StatusDoc {
                    unico: Unico::Ok,
                    reason: None,
                    since: Some(store::now()),
                    until: None,
                };
                tx_replace(&db, tx, COLLECTION, DOCUMENT, &doc)?;
            }
            Ok(degraded)
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{HeaderMap, Request as HttpRequest, StatusCode};
    use axum::middleware;
    use axum::routing::get;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::test_support::{
        FakeUnico, TEST_ACCESS_KEY, TestApp, all_docs, fast_unico, test_config, test_db,
    };

    const MONTHLY_LIMIT: &str = "Your monthly process limit has been reached";

    /// A `TestApp` whose Unico client obeys and updates the degraded mode, as in `serve`.
    async fn app() -> TestApp {
        let db = test_db().await;
        let unico = FakeUnico::start().await;
        let client = fast_unico(&unico.url, Some(TEST_ACCESS_KEY)).with_status(db.clone());
        let state = AppState::new(db, test_config(&unico.url), client).unwrap();
        TestApp { state, unico }
    }

    fn db(app: &TestApp) -> &FirestoreDb {
        &app.state.db
    }

    async fn seed(app: &TestApp, doc: StatusDoc) {
        store::replace(db(app), COLLECTION, DOCUMENT, &doc)
            .await
            .unwrap();
    }

    fn limit_reply(app: &TestApp) {
        app.unico.with(|s| {
            s.post_replies
                .push_back((429, json!({ "message": MONTHLY_LIMIT })))
        });
    }

    /// `POST /v1/captures`, the cheapest route that makes one Unico call.
    async fn login_capture(app: &TestApp) -> (StatusCode, Value) {
        app.post("/v1/captures", "device-1", json!({ "purpose": "login" }))
            .await
    }

    /// A queue item sent as a signed-in write that makes one Unico call.
    async fn send_item(app: &TestApp, session: &str, key: &str) -> (StatusCode, HeaderMap, Value) {
        let auth = format!("Bearer {session}");
        app.request(
            "POST",
            "/v1/captures",
            &[
                ("authorization", auth.as_str()),
                ("x-device-id", "device-1"),
                ("idempotency-key", key),
            ],
            Some(json!({ "purpose": "login" })),
        )
        .await
    }

    async fn public_status(app: &TestApp) -> Value {
        let (status, _, body) = app.request("GET", "/v1/status", &[], None).await;
        assert_eq!(status, StatusCode::OK);
        body
    }

    async fn stored(app: &TestApp) -> Option<StatusDoc> {
        read(db(app)).await.unwrap()
    }

    #[tokio::test]
    async fn monthly_limit_turns_the_mode_on_alerts_and_stops_every_call() {
        // spec 15, criteria 2 and 3
        let app = app().await;
        assert_eq!(public_status(&app).await["unico"], "ok");
        limit_reply(&app);

        let (status, body) = login_capture(&app).await;
        assert_eq!(
            (status.as_u16(), &body["error"]),
            (503, &json!("unico_limit"))
        );
        assert_eq!(app.unico.post_count(), 1, "the limit is not retried");

        let doc = stored(&app).await.unwrap();
        assert_eq!(
            (doc.unico, doc.reason, doc.until),
            (Unico::Degraded, Some(Reason::Limit), None)
        );
        assert!(doc.since.is_some());
        let alerts = all_docs(db(&app), "alerts").await;
        assert_eq!(alerts.len(), 1, "{alerts:?}");
        assert_eq!(alerts[0]["kind"], "unico_limit");
        assert_eq!(alerts[0]["for_civil_defense"], true);
        let status_body = public_status(&app).await;
        assert_eq!(status_body["unico"], "degraded");
        assert_eq!(status_body["reason"], "limit");

        // No call reaches Unico from now on, creating or polling.
        let (status, body) = login_capture(&app).await;
        assert_eq!(
            (status.as_u16(), &body["error"]),
            (503, &json!("unico_degraded"))
        );
        assert_eq!(
            app.state.unico.get_process("any").await,
            Err(crate::unico::UnicoError::Degraded)
        );
        assert_eq!(app.unico.post_count(), 1);
        assert_eq!(app.unico.get_count(), 0);
        assert_eq!(all_docs(db(&app), "alerts").await.len(), 1);
    }

    #[tokio::test]
    async fn resume_lets_the_calls_through_again() {
        let app = app().await;
        limit_reply(&app);
        login_capture(&app).await;
        assert_eq!(login_capture(&app).await.0, StatusCode::SERVICE_UNAVAILABLE);

        assert!(resume(db(&app)).await.unwrap());
        assert_eq!(
            public_status(&app).await,
            json!({ "unico": "ok", "pilot_responsible": "Pilot Team, pilot@example.org" })
        );
        let (status, _) = login_capture(&app).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(app.unico.post_count(), 2);
        // The alert stays: the Civil Defense resolves it.
        assert_eq!(all_docs(db(&app), "alerts").await.len(), 1);
    }

    #[tokio::test]
    async fn resume_with_nothing_to_resume_changes_nothing() {
        let app = app().await;
        assert!(!resume(db(&app)).await.unwrap());
        assert_eq!(stored(&app).await, None);
        seed(
            &app,
            StatusDoc {
                unico: Unico::Ok,
                reason: None,
                since: None,
                until: None,
            },
        )
        .await;
        assert!(!resume(db(&app)).await.unwrap());
    }

    #[tokio::test]
    async fn resume_also_ends_an_unavailable_period() {
        let app = app().await;
        enter_unavailable(db(&app)).await.unwrap();
        assert_eq!(
            blocking_reason(db(&app)).await.unwrap(),
            Some(Reason::Unavailable)
        );
        assert!(resume(db(&app)).await.unwrap());
        assert_eq!(blocking_reason(db(&app)).await.unwrap(), None);
    }

    #[tokio::test]
    async fn concurrent_limit_answers_write_one_alert() {
        let app = app().await;
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let db = db(&app).clone();
                tokio::spawn(async move { enter_limit(&db).await })
            })
            .collect();
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        assert_eq!(all_docs(db(&app), "alerts").await.len(), 1);
        assert_eq!(stored(&app).await.unwrap().reason, Some(Reason::Limit));
    }

    #[tokio::test]
    async fn plain_429_ends_in_busy_and_leaves_the_mode_off() {
        // spec 15, criterion 2
        let app = app().await;
        app.unico.with(|s| {
            s.post_replies
                .extend((0..10).map(|_| (429, json!({ "message": "too many requests" }))))
        });
        let (status, headers, body) = app
            .request(
                "POST",
                "/v1/captures",
                &[("x-device-id", "device-1")],
                Some(json!({ "purpose": "login" })),
            )
            .await;
        assert_eq!(
            (status.as_u16(), &body["error"]),
            (503, &json!("unico_busy"))
        );
        assert_eq!(headers["retry-after"], "30");
        assert_eq!(
            app.unico.post_count(),
            5,
            "the first call plus four retries"
        );
        assert_eq!(stored(&app).await, None);
        assert!(all_docs(db(&app), "alerts").await.is_empty());
    }

    #[tokio::test]
    async fn three_failures_in_a_row_turn_on_unavailable_for_five_minutes() {
        let app = app().await;
        app.unico
            .with(|s| s.post_replies.extend((0..6).map(|_| (500, json!({})))));
        for expected_posts in [2, 4] {
            let (status, body) = login_capture(&app).await;
            assert_eq!(
                (status.as_u16(), &body["error"]),
                (502, &json!("unico_unavailable"))
            );
            assert_eq!(app.unico.post_count(), expected_posts);
            assert_eq!(stored(&app).await, None, "two failures are not enough");
        }
        login_capture(&app).await;
        assert_eq!(app.unico.post_count(), 6);

        let doc = stored(&app).await.unwrap();
        assert_eq!(
            (doc.unico, doc.reason),
            (Unico::Degraded, Some(Reason::Unavailable))
        );
        let (since, until) = (doc.since.unwrap(), doc.until.unwrap());
        assert_eq!(until.as_microsecond() - since.as_microsecond(), 300_000_000);
        assert!(
            all_docs(db(&app), "alerts").await.is_empty(),
            "only the limit alerts"
        );
        let status_body = public_status(&app).await;
        assert_eq!(status_body["reason"], "unavailable");

        let (status, body) = login_capture(&app).await;
        assert_eq!(
            (status.as_u16(), &body["error"]),
            (503, &json!("unico_degraded"))
        );
        assert_eq!(app.unico.post_count(), 6);
    }

    #[tokio::test]
    async fn an_answer_in_between_resets_the_count_of_failures() {
        let app = app().await;
        app.unico.with(|s| {
            s.post_replies.extend([
                (500, json!({})),
                (500, json!({})),
                (500, json!({})),
                (500, json!({})),
                (200, json!({ "id": "p", "link": "https://j/p" })),
                (500, json!({})),
                (500, json!({})),
                (500, json!({})),
                (500, json!({})),
            ])
        });
        for _ in 0..2 {
            login_capture(&app).await; // two failures
        }
        assert_eq!(login_capture(&app).await.0, StatusCode::CREATED); // reset
        for _ in 0..2 {
            login_capture(&app).await; // two failures again
        }
        assert_eq!(stored(&app).await, None);
    }

    #[tokio::test]
    async fn after_the_five_minutes_the_next_call_tests_unico() {
        let app = app().await;
        let past = store::now() - SignedDuration::from_secs(1);
        seed(
            &app,
            StatusDoc::degraded(Reason::Unavailable, past, Some(past)),
        )
        .await;
        assert_eq!(blocking_reason(db(&app)).await.unwrap(), None);
        assert_eq!(public_status(&app).await["unico"], "ok");
        assert_eq!(login_capture(&app).await.0, StatusCode::CREATED);
        assert_eq!(app.unico.post_count(), 1);
    }

    #[tokio::test]
    async fn a_failed_test_after_the_period_turns_the_mode_on_again() {
        let app = app().await;
        app.unico
            .with(|s| s.post_replies.extend((0..8).map(|_| (500, json!({})))));
        for _ in 0..3 {
            login_capture(&app).await;
        }
        let first = stored(&app).await.unwrap();
        // The period ends; the single call that tests Unico fails.
        let past = store::now() - SignedDuration::from_secs(1);
        seed(
            &app,
            StatusDoc::degraded(Reason::Unavailable, past, Some(past)),
        )
        .await;
        login_capture(&app).await;
        let second = stored(&app).await.unwrap();
        assert!(second.until.unwrap() > first.until.unwrap());
        assert_eq!(app.unico.post_count(), 8);
    }

    #[tokio::test]
    async fn unavailable_neither_replaces_the_limit_nor_extends_itself() {
        let app = app().await;
        enter_limit(db(&app)).await.unwrap();
        let limit = stored(&app).await;
        enter_unavailable(db(&app)).await.unwrap();
        assert_eq!(stored(&app).await, limit);
        resume(db(&app)).await.unwrap();

        enter_unavailable(db(&app)).await.unwrap();
        let running = stored(&app).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        enter_unavailable(db(&app)).await.unwrap();
        assert_eq!(stored(&app).await, running);
    }

    #[tokio::test]
    async fn the_limit_replaces_a_running_unavailable_period_and_alerts() {
        let app = app().await;
        enter_unavailable(db(&app)).await.unwrap();
        enter_limit(db(&app)).await.unwrap();
        let doc = stored(&app).await.unwrap();
        assert_eq!((doc.reason, doc.until), (Some(Reason::Limit), None));
        assert_eq!(all_docs(db(&app), "alerts").await.len(), 1);
    }

    #[tokio::test]
    async fn the_api_only_flow_also_records_the_limit() {
        let app = app().await;
        limit_reply(&app);
        let err = app
            .state
            .unico
            .token_from_image(b"photo")
            .await
            .unwrap_err();
        assert_eq!(err, crate::unico::UnicoError::Limit);
        assert_eq!(
            blocking_reason(db(&app)).await.unwrap(),
            Some(Reason::Limit)
        );
        assert_eq!(
            app.state
                .unico
                .token_from_image(b"photo")
                .await
                .unwrap_err(),
            crate::unico::UnicoError::Degraded
        );
        assert_eq!(app.unico.post_count(), 1);
    }

    #[tokio::test]
    async fn an_oversize_image_is_refused_with_413_whatever_the_mode() {
        // spec 15, criterion 5
        let app = app().await;
        let big = vec![0u8; 800 * 1024 + 1];
        let err = app.state.unico.token_from_image(&big).await.unwrap_err();
        let api: ApiError = err.into();
        assert_eq!((api.status.as_u16(), api.code), (413, "image_too_large"));
        enter_limit(db(&app)).await.unwrap();
        let err = app.state.unico.token_from_image(&big).await.unwrap_err();
        assert_eq!(err, crate::unico::UnicoError::ImageTooLarge);
        assert_eq!(app.unico.post_count(), 0);
    }

    #[tokio::test]
    async fn the_gate_answers_503_only_while_the_mode_is_on() {
        let app = app().await;
        let router = Router::new()
            .route("/probe", get(|| async { "reached" }))
            .route_layer(middleware::from_fn_with_state(
                app.state.clone(),
                require_unico,
            ));
        let probe = || async {
            let response = router
                .clone()
                .oneshot(HttpRequest::get("/probe").body(Body::empty()).unwrap())
                .await
                .unwrap();
            (response.status(), body_json_or_text(response).await)
        };
        assert_eq!(probe().await, (StatusCode::OK, json!("reached")));
        enter_limit(db(&app)).await.unwrap();
        let (status, body) = probe().await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "unico_degraded");
        resume(db(&app)).await.unwrap();
        assert_eq!(probe().await.0, StatusCode::OK);
        // The same check, for handlers.
        enter_unavailable(db(&app)).await.unwrap();
        assert_eq!(
            ensure_available(db(&app)).await.unwrap_err().code,
            "unico_degraded"
        );
    }

    async fn body_json_or_text(response: Response) -> Value {
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    }

    #[tokio::test]
    async fn the_same_queue_item_never_makes_a_second_unico_call() {
        // spec 15, criterion 55
        let app = app().await;
        let (session, _) = app.login("device-1", "tok-ana", "Ana").await;
        let calls_after_login = app.unico.post_count();
        let send = || send_item(&app, &session, "queue-item-1");
        let (first_status, _, first) = send().await;
        let (second_status, _, second) = send().await;
        assert_eq!(
            (first_status, second_status),
            (StatusCode::CREATED, StatusCode::CREATED)
        );
        assert_eq!(first, second);
        assert_eq!(app.unico.post_count(), calls_after_login + 1);
    }

    #[tokio::test]
    async fn a_queue_item_retried_during_the_limit_makes_no_call() {
        // spec 15, criteria 3 and 55
        let app = app().await;
        let (session, _) = app.login("device-1", "tok-ana", "Ana").await;
        let calls_after_login = app.unico.post_count();
        let send = || send_item(&app, &session, "queue-item-2");
        limit_reply(&app);
        assert_eq!(send().await.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(send().await.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(app.unico.post_count(), calls_after_login + 1);
        resume(db(&app)).await.unwrap();
        assert_eq!(send().await.0, StatusCode::CREATED);
        assert_eq!(send().await.0, StatusCode::CREATED);
        assert_eq!(app.unico.post_count(), calls_after_login + 2);
    }

    #[tokio::test]
    async fn status_document_round_trips_with_timestamps() {
        let app = app().await;
        let now = store::now();
        let doc = StatusDoc::degraded(Reason::Unavailable, now, Some(now + UNAVAILABLE_FOR));
        seed(&app, doc.clone()).await;
        assert_eq!(stored(&app).await, Some(doc));
    }

    #[test]
    fn only_a_running_period_or_the_limit_blocks() {
        let now = store::now();
        let later = now + SignedDuration::from_secs(1);
        let earlier = now - SignedDuration::from_secs(1);
        let ok = StatusDoc {
            unico: Unico::Ok,
            reason: None,
            since: None,
            until: None,
        };
        assert_eq!(ok.active_reason(now), None);
        assert_eq!(
            StatusDoc::degraded(Reason::Limit, earlier, None).active_reason(now),
            Some(Reason::Limit)
        );
        assert_eq!(
            StatusDoc::degraded(Reason::Unavailable, earlier, Some(later)).active_reason(now),
            Some(Reason::Unavailable)
        );
        assert_eq!(
            StatusDoc::degraded(Reason::Unavailable, earlier, Some(earlier)).active_reason(now),
            None
        );
        assert_eq!(
            StatusDoc::degraded(Reason::Unavailable, earlier, None).active_reason(now),
            None
        );
    }
}
