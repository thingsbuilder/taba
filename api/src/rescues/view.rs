//! The JSON a rescue shows (spec 8.7).

use axum::Json;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::people::Level;

pub(super) fn created(rescue_id: &str, person_id: &str, level: Level) -> (StatusCode, Json<Value>) {
    (
        StatusCode::CREATED,
        Json(json!({ "rescue_id": rescue_id, "person_id": person_id, "level": level })),
    )
}
