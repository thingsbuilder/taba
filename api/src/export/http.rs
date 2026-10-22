//! The route of the export (spec 8.8, Q22): it checks who asks, builds the CSV with
//! `persist` and audits it.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use jiff::tz::TimeZone;

use super::persist::{MAX_ROWS, build};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::shelters::{self, Confirmation, ShelterDoc};
use crate::store;

/// `GET /v1/shelters/{id}/export.csv`: `403` for anyone but an admin of a `confirmed` shelter.
/// Reading, so `events::ensure_open` does not apply: a closed event can still be exported.
pub async fn export_csv(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
) -> Result<Response, ApiError> {
    let forbidden = || ApiError::new(StatusCode::FORBIDDEN, "forbidden");
    if !volunteer.is_admin_of(&shelter_id) {
        return Err(forbidden());
    }
    let shelter: ShelterDoc = store::get(&state.db, shelters::COLLECTION, &shelter_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found"))?;
    if shelter.confirmation != Confirmation::Confirmed {
        return Err(forbidden());
    }
    let now = store::now();
    let body = build(&state.db, &shelter_id, now, MAX_ROWS).await?;
    let entry = AuditEntry::new("export.shelter", "shelter", &shelter_id)
        .by(&volunteer.id)
        .in_shelter(&shelter_id)
        .basis(LegalBasis::PublicPolicy);
    store::transact(&state.db, |db, tx| {
        let entry = entry.clone();
        Box::pin(async move { audit::add(&db, tx, &entry) })
    })
    .await?;
    let day = now.to_zoned(TimeZone::UTC).date();
    let disposition = format!("attachment; filename=\"taba-shelter-{shelter_id}-{day}.csv\"");
    Ok((
        [
            (CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (CACHE_CONTROL, "no-store".to_string()),
            (CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
        .into_response())
}
