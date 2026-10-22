//! The routes (spec 8.1): `GET` and `PATCH /v1/me`.

use axum::Json;
use axum::extract::State;
use serde_json::Value;

use super::domain::ProfilePatch;
use super::persist::save_profile;
use super::view::me_body;
use crate::audit::{AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;

pub async fn get_me(volunteer: Volunteer) -> Json<Value> {
    Json(me_body(&volunteer))
}

pub async fn patch_me(
    State(state): State<AppState>,
    mut volunteer: Volunteer,
    Json(patch): Json<ProfilePatch>,
) -> Result<Json<Value>, ApiError> {
    let mut doc = volunteer.doc.clone();
    patch.apply(&mut doc)?;

    let entry = AuditEntry::new(super::ACTION_UPDATE, "volunteer", &volunteer.id)
        .basis(LegalBasis::FraudPrevention)
        .by(&volunteer.id);
    save_profile(&state, &volunteer.id, &doc, &entry).await?;
    volunteer.doc = doc;
    Ok(Json(me_body(&volunteer)))
}
