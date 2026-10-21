//! API error format (spec 6.2.3): `{"error": "<stable_code>", "detail": {...}}`.
//! The server never translates text; the app maps the stable code.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub detail: Value,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str) -> Self {
        Self {
            status,
            code,
            detail: json!({}),
        }
    }

    pub fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": self.code, "detail": self.detail })),
        )
            .into_response()
    }
}

/// Fallback for unknown routes, so every error body has the same shape.
pub async fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not_found")
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    async fn body_json(response: Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn error_has_stable_code_and_detail() {
        let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "profile_required");
        error.detail = json!({ "field": "name" });
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body_json(response).await,
            json!({ "error": "profile_required", "detail": { "field": "name" } })
        );
    }

    #[tokio::test]
    async fn detail_defaults_to_empty_object() {
        let response = ApiError::internal().into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body_json(response).await,
            json!({ "error": "internal_error", "detail": {} })
        );
    }
}
