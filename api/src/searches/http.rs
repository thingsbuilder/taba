//! The route of the searches module: `POST /v1/searches`.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use serde_json::Value;

use super::domain::{parse_input, registering_membership};
use super::model::{SearchOwner, SearchVia};
use super::persist::register;
use super::view::answer;
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;

/// `POST /v1/searches`: `202 {message: "search_registered", usable_photos}`; `429 search_limit`.
pub async fn post_search(
    State(state): State<AppState>,
    volunteer: Volunteer,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let membership = registering_membership(&volunteer)?;
    crate::events::ensure_open(&state.db, membership.and_then(|m| m.event_id.as_deref())).await?;
    let input = parse_input(body)?;
    let owner = SearchOwner {
        volunteer_id: volunteer.id.clone(),
        device_id: crate::auth::device_id(&headers).ok(),
        origin_shelter_id: membership.map(|m| m.shelter_id.clone()),
        event_id: membership.and_then(|m| m.event_id.clone()),
        via: SearchVia::Volunteer,
        key: headers
            .get("idempotency-key")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_string()),
    };
    let limit = state.config.searches_per_day;
    let event_id = owner.event_id.clone();
    let outcome = register(&state, owner, input, Some(limit)).await?;
    crate::events::touch(&state, event_id.as_deref()).await;
    Ok(answer(outcome))
}
