//! The routes (spec 3.9, 8.3): the admin of a new shelter makes a code, an admin of a confirmed
//! shelter reads it.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

use super::domain::CODE_TTL;
use super::model::{Endorse, EndorsementCodeDoc, Position, forbidden};
use super::persist;
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::shelters::{self, valid_position};
use crate::store;

// ---- POST /v1/shelters/{id}/endorsements/in-person/code ----

pub async fn create_code(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
    Json(position): Json<Position>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if !volunteer.is_admin_of(&shelter_id) {
        return Err(forbidden());
    }
    crate::events::ensure_shelter_open(&state.db, &shelter_id).await?;
    valid_position(position.lat, position.lon, position.accuracy)?;
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes);
    let code = URL_SAFE_NO_PAD.encode(bytes);
    let doc = EndorsementCodeDoc {
        shelter_id: shelter_id.clone(),
        created_by: volunteer.id.clone(),
        lat: position.lat,
        lon: position.lon,
        accuracy_m: position.accuracy,
        expires_at: store::now() + CODE_TTL,
        used: false,
    };

    persist::create(&state, &shelter_id, &code, &doc, &volunteer.id).await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "code": code,
            "url": format!("https://{}/endorse/{code}", state.domain()),
            "expires_at": doc.expires_at.to_string(),
        })),
    ))
}

// ---- POST /v1/endorsements/in-person ----

pub async fn endorse(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Json(request): Json<Endorse>,
) -> Result<Json<Value>, ApiError> {
    let radius_m = f64::from(state.config.shelter_radius_m);

    let (shelter_id, shelter) = persist::endorse(&state, &request, &volunteer, radius_m).await?;

    crate::events::touch(&state, shelter.event_id.as_deref()).await;
    Ok(Json(
        json!({ "shelter": shelters::view_for(&state.db, &volunteer, &shelter_id, &shelter).await? }),
    ))
}
