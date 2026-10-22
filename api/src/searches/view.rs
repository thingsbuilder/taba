//! The JSON a registered search is answered with.

use axum::Json;
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::model::RegisterOutcome;

/// The one answer of a registered search, with or without a match (spec 3.7 item 6).
pub fn answer(outcome: RegisterOutcome) -> (StatusCode, Json<Value>) {
    (
        StatusCode::ACCEPTED,
        Json(json!({ "message": "search_registered", "usable_photos": outcome.usable_photos })),
    )
}
