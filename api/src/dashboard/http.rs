//! The routes of the panel (spec 3.10, 8.8): they check who reads, then serve the
//! cached answer or build it with `persist`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use jiff::tz::TimeZone;
use serde_json::Value;

use super::model::EventDashboard;
use super::persist::{
    CACHE_TTL, COUNT_LIMITER, DETAIL, EVENT_LIMIT, LIMITS, LIST, LIST_TTL, SHELTER_LIMIT,
    build_event, build_list,
};
use super::view::daily_csv_body;
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::store;

// ---- routes ----

/// Only a validator (read) and the Civil Defense see the panel (spec 2.3).
fn require_reader(volunteer: &Volunteer) -> Result<(), ApiError> {
    if volunteer.is_validator_or_civil_defense() {
        Ok(())
    } else {
        Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden"))
    }
}

async fn dashboard_of(state: &AppState, event_id: &str) -> Result<EventDashboard, ApiError> {
    let key = format!("{}/{event_id}", state.db.get_database_path());
    DETAIL
        .get_or(&key, CACHE_TTL, || async {
            build_event(&state.db, &COUNT_LIMITER, event_id, LIMITS).await
        })
        .await
}

/// `GET /v1/dashboard/events`: `{events: [EventSummary]}`.
pub async fn list_events(
    State(state): State<AppState>,
    volunteer: Volunteer,
) -> Result<Json<Value>, ApiError> {
    require_reader(&volunteer)?;
    let list = LIST
        .get_or(state.db.get_database_path(), LIST_TTL, || {
            build_list(&state.db, EVENT_LIMIT, SHELTER_LIMIT)
        })
        .await?;
    Ok(Json(list))
}

/// `GET /v1/dashboard/events/{id}`: an `EventDashboard`.
pub async fn get_event(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(event_id): Path<String>,
) -> Result<Json<EventDashboard>, ApiError> {
    require_reader(&volunteer)?;
    Ok(Json(dashboard_of(&state, &event_id).await?))
}

/// `GET /v1/dashboard/events/{id}/daily.csv`: one line per shelter, no name of a person, no `person_id`.
pub async fn daily_csv(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(event_id): Path<String>,
) -> Result<Response, ApiError> {
    require_reader(&volunteer)?;
    let dashboard = dashboard_of(&state, &event_id).await?;
    let date = store::now().to_zoned(TimeZone::UTC).date().to_string();
    let body = daily_csv_body(&dashboard, &date);
    Ok((
        StatusCode::OK,
        [
            (CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (
                CONTENT_DISPOSITION,
                format!("attachment; filename=\"taba-{event_id}-{date}.csv\""),
            ),
            (CACHE_CONTROL, "no-store".to_string()),
        ],
        body,
    )
        .into_response())
}
