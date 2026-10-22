//! API error format (spec 6.2.3): `{"error": "<stable_code>", "detail": {...}}`.
//! The server never translates text; the app maps the stable code.

use axum::Json;
use axum::http::StatusCode;
use axum::http::header::RETRY_AFTER;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub detail: Value,
    /// Seconds for the `Retry-After` header (`503 unico_busy`).
    pub retry_after: Option<u32>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str) -> Self {
        Self {
            status,
            code,
            detail: json!({}),
            retry_after: None,
        }
    }

    pub fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
    }

    // Phase 3 codes (doc da fase 3, "O que a fase cria"); `no_face`, `invalid_image` and
    // `image_too_large` already come from `UnicoError`.
    pub fn unico_degraded() -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unico_degraded")
    }

    pub fn protection_role_required() -> Self {
        Self::new(StatusCode::FORBIDDEN, "protection_role_required")
    }

    pub fn revalidation_failed() -> Self {
        Self::new(StatusCode::CONFLICT, "revalidation_failed")
    }

    pub fn guardian_not_verified() -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "guardian_not_verified")
    }

    pub fn person_in_other_household() -> Self {
        Self::new(StatusCode::CONFLICT, "person_in_other_household")
    }

    pub fn household_has_card() -> Self {
        Self::new(StatusCode::CONFLICT, "household_has_card")
    }

    pub fn code_taken() -> Self {
        Self::new(StatusCode::CONFLICT, "code_taken")
    }
}

// Phase 4 codes (doc da fase 4, "O que a fase cria"). Each is raised by the module that owns the rule.
impl ApiError {
    pub fn search_limit() -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, "search_limit")
    }

    pub fn match_already_decided() -> Self {
        Self::new(StatusCode::CONFLICT, "match_already_decided")
    }

    pub fn event_closed() -> Self {
        Self::new(StatusCode::CONFLICT, "event_closed")
    }

    pub fn search_link_invalid() -> Self {
        Self::new(StatusCode::GONE, "search_link_invalid")
    }
}

// Phase 5 codes (doc da fase 5, "O que a fase cria"). Each is raised by the module that owns the rule.
#[allow(dead_code)] // stage 0 scaffold: the owners of phase 5 start using these
impl ApiError {
    /// A route that is registered but not written yet.
    pub fn not_implemented() -> Self {
        Self::new(StatusCode::NOT_IMPLEMENTED, "not_implemented")
    }

    pub fn liveness_required() -> Self {
        Self::new(StatusCode::PRECONDITION_REQUIRED, "liveness_required")
    }

    pub fn admin_approval_required() -> Self {
        Self::new(StatusCode::FORBIDDEN, "admin_approval_required")
    }

    pub fn card_not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "card_not_found")
    }

    pub fn person_not_in_household() -> Self {
        Self::new(StatusCode::NOT_FOUND, "person_not_in_household")
    }
}

/// Lets a handler's error cross a Firestore transaction (see `store::fs_error`).
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.status.as_u16(), self.code)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(json!({ "error": self.code, "detail": self.detail }));
        match self.retry_after {
            Some(seconds) => {
                (self.status, [(RETRY_AFTER, seconds.to_string())], body).into_response()
            }
            None => (self.status, body).into_response(),
        }
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
