//! The routes of the search links: the volunteer's (create, cancel) and the family's, public
//! (check, submit).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use super::domain::{MAX_PER_DEVICE_HOUR, MAX_PER_IP_HOUR, allow, new_code, search_id_of};
use super::model::{COLLECTION, LinkStatus, SearchLinkDoc, TTL, forbidden, invalid_shelter};
use super::persist::run_submit;
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::searches;
use crate::shelters::{self, Confirmation, ShelterDoc};
use crate::store::{self, tx_create, tx_replace};
use crate::volunteers::GlobalRole;

#[derive(Debug, Deserialize)]
pub struct CreateLink {
    shelter_id: String,
}

/// `POST /v1/search-links`: `201 {code, url, expires_at}`; counts in `SEARCHES_PER_DAY` (`429 search_limit`).
/// Who may: a member of a `confirmed` shelter, or `civil_defense`, `mediator`, `protection`.
pub async fn post_search_link(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Json(request): Json<CreateLink>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let shelter: ShelterDoc = store::get(&state.db, shelters::COLLECTION, &request.shelter_id)
        .await?
        .ok_or_else(invalid_shelter)?;
    let global = [
        GlobalRole::CivilDefense,
        GlobalRole::Mediator,
        GlobalRole::Protection,
    ]
    .into_iter()
    .any(|role| volunteer.has_global(role));
    let member_of_confirmed = volunteer.role_in(&request.shelter_id).is_some()
        && shelter.confirmation == Confirmation::Confirmed;
    // The origin: a member's own confirmed shelter, or, for the Civil Defense, a mediator and
    // protection, any confirmed shelter they name (the app asks them which).
    if !member_of_confirmed {
        if !global {
            return Err(forbidden());
        }
        if shelter.confirmation != Confirmation::Confirmed {
            return Err(invalid_shelter());
        }
    }
    crate::events::ensure_open(&state.db, shelter.event_id.as_deref()).await?;

    let now = store::now();
    let code = new_code();
    let link = SearchLinkDoc {
        created_by: volunteer.id.clone(),
        shelter_id: request.shelter_id.clone(),
        event_id: shelter.event_id.clone(),
        status: LinkStatus::Active,
        created_at: now,
        expires_at: now + TTL,
        used_at: None,
        search_id: None,
        attempts: 0,
        processing_until: None,
    };
    let limit = state.config.searches_per_day;
    store::transact(&state.db, |db, tx| {
        let (code, link, by, tz) = (
            code.clone(),
            link.clone(),
            volunteer.id.clone(),
            state.config.tz_pilot.clone(),
        );
        Box::pin(async move {
            searches::take_daily_slot(&db, tx, &by, limit, &tz).await?;
            tx_create(&db, tx, COLLECTION, &code, &link)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new("search_link.create", "search_link", &code)
                    .basis(LegalBasis::VitalInterest)
                    .by(&by)
                    .in_shelter(&link.shelter_id),
            )
        })
    })
    .await?;
    crate::events::touch(&state, shelter.event_id.as_deref()).await;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "code": code,
            "url": format!("https://{}/f/{code}", state.domain()),
            "expires_at": link.expires_at.to_string(),
        })),
    ))
}
// ---- DELETE /v1/search-links/{code} ----

/// `DELETE /v1/search-links/{code}`: cancels it; only who made it or the Civil Defense.
pub async fn delete_search_link(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(code): Path<String>,
) -> Result<StatusCode, ApiError> {
    let event_id = store::transact(&state.db, |db, tx| {
        let (code, volunteer) = (code.clone(), volunteer.clone());
        Box::pin(async move {
            let mut link = store::get::<SearchLinkDoc>(&db, COLLECTION, &code)
                .await?
                .filter(|l| l.status == LinkStatus::Active && store::now() < l.expires_at)
                .ok_or_else(ApiError::search_link_invalid)?;
            if link.created_by != volunteer.id && !volunteer.has_global(GlobalRole::CivilDefense) {
                return Err(forbidden());
            }
            link.status = LinkStatus::Canceled;
            link.processing_until = None;
            tx_replace(&db, tx, COLLECTION, &code, &link)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new("search_link.cancel", "search_link", &code)
                    .basis(LegalBasis::VitalInterest)
                    .by(&volunteer.id)
                    .in_shelter(&link.shelter_id),
            )?;
            Ok(link.event_id.clone())
        })
    })
    .await?;
    crate::events::touch(&state, event_id.as_deref()).await;
    Ok(StatusCode::NO_CONTENT)
}
// ---- GET /v1/search-links/{code} ----

/// `GET /v1/search-links/{code}`, public: `{valid: true}`, or `410 search_link_invalid` for a
/// link that is expired, used, canceled, out of attempts or unknown. Nothing about who made it.
pub async fn get_search_link(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<Value>, ApiError> {
    store::get::<SearchLinkDoc>(&state.db, COLLECTION, &code)
        .await?
        .filter(|l| l.is_valid(store::now()))
        .ok_or_else(ApiError::search_link_invalid)?;
    Ok(Json(json!({ "valid": true })))
}
/// The device header and the IP the proxy saw: the last `X-Forwarded-For` entry is the one the
/// proxy appended, the earlier ones the client may have made up.
fn rate_limit(headers: &HeaderMap) -> Result<(), ApiError> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let ip = header("x-forwarded-for")
        .and_then(|v| v.rsplit(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("unknown");
    let device = header("x-device-id")
        .map(str::trim)
        .filter(|v| v.len() <= 64)
        .unwrap_or("none");
    if allow(&format!("ip~{ip}"), MAX_PER_IP_HOUR)
        && allow(&format!("device~{device}~{ip}"), MAX_PER_DEVICE_HOUR)
    {
        Ok(())
    } else {
        let mut error = ApiError::new(StatusCode::TOO_MANY_REQUESTS, "too_many_requests");
        error.retry_after = Some(60);
        Err(error)
    }
}

/// `POST /v1/search-links/{code}/submit`, public: the same pipeline as `POST /v1/searches`
/// (`searches::register_photos`) and the same answer; `410 search_link_invalid`,
/// `503 unico_degraded` keeps the link.
pub async fn submit_search_link(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
    Json(mut request): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    rate_limit(&headers)?;
    // The family sends no id: it is derived from the link, so a retry is the same search.
    if let Some(body) = request.as_object_mut() {
        body.insert("id".into(), search_id_of(&code).into());
    }
    let input = searches::parse_input(request)?;
    let device = headers
        .get("x-device-id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.len() <= 64)
        .map(str::to_string);
    run_submit(&state, &code, device, input, |owner, input| {
        searches::register_photos(&state, owner, input)
    })
    .await
}
