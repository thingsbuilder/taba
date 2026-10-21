//! `GET /v1/status` (spec 8.1): public, no personal data. Reads `system/status` (spec 7).
//! A missing document means the Unico integration is healthy.

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use crate::auth::AppState;
use crate::degraded;
use crate::error::ApiError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unico {
    Ok,
    Degraded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Limit,
    Unavailable,
}

/// Public view of `system/status`; `since` and `until` stay server-side. `pilot_responsible`
/// (`PILOT_RESPONSIBLE`) is for the consent screen; it is not personal data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub unico: Unico,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
    pub pilot_responsible: Option<String>,
}

pub async fn get_status(State(state): State<AppState>) -> Result<Json<Status>, ApiError> {
    let reason = degraded::blocking_reason(&state.db).await?;
    Ok(Json(Status {
        unico: if reason.is_some() {
            Unico::Degraded
        } else {
            Unico::Ok
        },
        reason,
        pilot_responsible: state.config.pilot_responsible.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::degraded::StatusDoc;
    use crate::router;
    use crate::test_support::{TestApp, body_json, fast_unico, test_config, test_db};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use firestore::FirestoreDb;
    use firestore::jiff::SignedDuration;
    use serde_json::json;
    use tower::ServiceExt;

    async fn call(db: FirestoreDb, path: &str) -> (StatusCode, serde_json::Value) {
        let app = TestApp::with_db(db).await;
        let response = router(app.state)
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        (response.status(), body_json(response).await)
    }

    const PILOT: &str = "Pilot Team, pilot@example.org";

    async fn seed(db: &FirestoreDb, doc: StatusDoc) {
        crate::store::replace(db, degraded::COLLECTION, degraded::DOCUMENT, &doc)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn missing_document_means_ok() {
        let db = test_db().await;
        let (status, body) = call(db, "/v1/status").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "unico": "ok", "pilot_responsible": PILOT }));
    }

    #[tokio::test]
    async fn degraded_document_is_reported_with_reason_only() {
        let db = test_db().await;
        let since = crate::store::now();
        seed(
            &db,
            StatusDoc {
                unico: Unico::Degraded,
                reason: Some(Reason::Limit),
                since: Some(since),
                until: None,
            },
        )
        .await;
        let (status, body) = call(db, "/v1/status").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({ "unico": "degraded", "reason": "limit", "pilot_responsible": PILOT })
        );
    }

    #[tokio::test]
    async fn unavailable_is_reported_while_it_runs_and_not_after() {
        let db = test_db().await;
        let now = crate::store::now();
        let doc = |until| StatusDoc {
            unico: Unico::Degraded,
            reason: Some(Reason::Unavailable),
            since: Some(now),
            until: Some(until),
        };
        seed(&db, doc(now + SignedDuration::from_mins(5))).await;
        let (_, body) = call(db.clone(), "/v1/status").await;
        assert_eq!(body["reason"], "unavailable");
        seed(&db, doc(now - SignedDuration::from_secs(1))).await;
        let (_, body) = call(db, "/v1/status").await;
        assert_eq!(body, json!({ "unico": "ok", "pilot_responsible": PILOT }));
    }

    #[tokio::test]
    async fn ok_document_has_no_reason() {
        let db = test_db().await;
        seed(
            &db,
            StatusDoc {
                unico: Unico::Ok,
                reason: None,
                since: None,
                until: None,
            },
        )
        .await;
        let (_, body) = call(db, "/v1/status").await;
        assert_eq!(body, json!({ "unico": "ok", "pilot_responsible": PILOT }));
    }

    #[tokio::test]
    async fn pilot_responsible_is_null_when_not_configured() {
        let db = test_db().await;
        let mut config = test_config("http://127.0.0.1:1");
        config.pilot_responsible = None;
        let state = AppState::new(db, config, fast_unico("http://127.0.0.1:1", None)).unwrap();
        let response = router(state)
            .oneshot(Request::get("/v1/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            body_json(response).await,
            json!({ "unico": "ok", "pilot_responsible": null })
        );
    }

    #[tokio::test]
    async fn body_over_6_mb_is_rejected() {
        let db = test_db().await;
        let request = Request::post("/v1/status")
            .header("content-length", 6 * 1024 * 1024 + 1)
            .body(Body::empty())
            .unwrap();
        let app = TestApp::with_db(db).await;
        let response = router(app.state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn unknown_route_uses_the_error_format() {
        let db = test_db().await;
        let (status, body) = call(db, "/v1/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, json!({ "error": "not_found", "detail": {} }));
    }
}
