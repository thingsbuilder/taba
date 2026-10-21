//! `GET /v1/status` (spec 8.1): public, no personal data. Reads `system/status` (spec 7).
//! A missing document means the Unico integration is healthy.

use axum::Json;
use axum::extract::State;
use firestore::FirestoreDb;
use serde::{Deserialize, Serialize};

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

/// Public view of `system/status`; `since` and `until` stay server-side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub unico: Unico,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
}

pub async fn get_status(State(db): State<FirestoreDb>) -> Result<Json<Status>, ApiError> {
    let doc: Option<Status> = db
        .fluent()
        .select()
        .by_id_in("system")
        .obj()
        .one("status")
        .await
        .map_err(|err| {
            tracing::error!(%err, "failed to read system/status");
            ApiError::internal()
        })?;
    Ok(Json(doc.unwrap_or(Status {
        unico: Unico::Ok,
        reason: None,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router;
    use crate::test_support::{body_json, test_db};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    async fn call(db: FirestoreDb, path: &str) -> (StatusCode, serde_json::Value) {
        let response = router(db)
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        (response.status(), body_json(response).await)
    }

    #[tokio::test]
    async fn missing_document_means_ok() {
        let db = test_db().await;
        let (status, body) = call(db, "/v1/status").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "unico": "ok" }));
    }

    #[tokio::test]
    async fn degraded_document_is_reported_with_reason_only() {
        let db = test_db().await;
        let seed =
            json!({ "unico": "degraded", "reason": "limit", "since": "2026-01-01T00:00:00Z" });
        db.fluent()
            .insert()
            .into("system")
            .document_id("status")
            .object(&seed)
            .execute::<()>()
            .await
            .unwrap();
        let (status, body) = call(db, "/v1/status").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "unico": "degraded", "reason": "limit" }));
    }

    #[tokio::test]
    async fn ok_document_has_no_reason() {
        let db = test_db().await;
        let seed = json!({ "unico": "ok" });
        db.fluent()
            .insert()
            .into("system")
            .document_id("status")
            .object(&seed)
            .execute::<()>()
            .await
            .unwrap();
        let (_, body) = call(db, "/v1/status").await;
        assert_eq!(body, json!({ "unico": "ok" }));
    }

    #[tokio::test]
    async fn body_over_6_mb_is_rejected() {
        let db = test_db().await;
        let request = Request::post("/v1/status")
            .header("content-length", 6 * 1024 * 1024 + 1)
            .body(Body::empty())
            .unwrap();
        let response = router(db).oneshot(request).await.unwrap();
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
