//! The routes (spec 3.3, 8.4): create, list, cancel, preview and accept an invite.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use super::domain::{LINK_TTL, QR_TTL, new_code};
use super::model::{InviteDoc, InviteKind, InviteStatus, forbidden};
use super::persist::{self, Accept};
use super::view;
use crate::auth::{
    self, AppState, CaptureStatus, Purpose, Volunteer, resolve_person, usable_capture,
};
use crate::error::ApiError;
use crate::members::{self, ShelterRole};
use crate::store;
use crate::volunteers::ProfileInput;

// ---- POST /v1/shelters/{id}/invites ----

#[derive(Debug, Deserialize)]
pub struct CreateInvite {
    kind: InviteKind,
    role: ShelterRole,
}

pub async fn create_invite(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
    Json(request): Json<CreateInvite>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if !volunteer.is_admin_of(&shelter_id) {
        return Err(forbidden());
    }
    crate::events::ensure_shelter_open(&state.db, &shelter_id).await?;
    if request.kind == InviteKind::Link && request.role == ShelterRole::Admin {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "link_volunteer_only",
        ));
    }
    let now = store::now();
    let ttl = match request.kind {
        InviteKind::Qr => QR_TTL,
        InviteKind::Link => LINK_TTL,
    };
    let code = new_code();
    let invite = InviteDoc {
        shelter_id: shelter_id.clone(),
        kind: request.kind,
        role: request.role,
        created_by: volunteer.id.clone(),
        created_at: now,
        expires_at: now + ttl,
        status: InviteStatus::Active,
        used_by: None,
        used_at: None,
    };

    persist::create(&state, &code, &invite, &volunteer.id).await?;
    members::touch_shelter_event(&state, &shelter_id).await;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({ "code": code, "url": view::url(&state, &code), "expires_at": invite.expires_at.to_string() }),
        ),
    ))
}

// ---- GET /v1/shelters/{id}/invites ----

pub async fn list_invites(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !volunteer.is_admin_of(&shelter_id) {
        return Err(forbidden());
    }
    let invites: Vec<Value> = persist::open_in_shelter(&state, &shelter_id)
        .await?
        .iter()
        .map(|l| view::invite_row(&state, l))
        .collect();
    let joined_by_link: Vec<Value> = persist::joined_by_link(&state, &shelter_id)
        .await?
        .iter()
        .map(|(member, name)| view::joined_row(member, name))
        .collect();
    Ok(Json(
        json!({ "invites": invites, "joined_by_link": joined_by_link }),
    ))
}

// ---- DELETE /v1/invites/{code} ----

pub async fn cancel_invite(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(code): Path<String>,
) -> Result<StatusCode, ApiError> {
    let shelter_id = persist::cancel(&state, &volunteer, &code).await?;
    members::touch_shelter_event(&state, &shelter_id).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- GET /v1/invites/{code} ----

pub async fn get_invite(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(view::preview(
        persist::usable(&state.db, &code).await?,
    )))
}

// ---- POST /v1/invites/{code}/accept ----

#[derive(Debug, Deserialize)]
pub struct AcceptInvite {
    capture_id: String,
    #[serde(default)]
    profile: Option<ProfileInput>,
}

pub async fn accept_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
    Json(request): Json<AcceptInvite>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let device = auth::device_id(&headers)?;
    // A dead invite is refused before the capture is looked at, so it costs nothing and
    // leaves the capture unused.
    let invite = persist::live_invite(&state.db, &code).await?;
    crate::events::ensure_shelter_open(&state.db, &invite.shelter_id).await?;
    let mut capture = usable_capture(&state, &device, &request.capture_id, Purpose::Invite).await?;
    let person_id = resolve_person(&state, &request.capture_id, &mut capture).await?;

    let args = Accept {
        code,
        capture_id: request.capture_id,
        device_id: device,
        person_id: person_id.clone(),
        profile: request.profile,
        secret: auth::new_secret(),
        session_days: state.config.session_days,
    };

    let accepted = persist::accept(&state, &args).await;

    let accepted = match accepted {
        Ok(accepted) => accepted,
        // A new volunteer still owes the profile: keep what Unico said, it is not asked again.
        Err(e) if e.code == "profile_required" => {
            capture.status = CaptureStatus::Ok;
            capture.person_id = Some(person_id);
            auth::remember_result(&state, &args.capture_id, &capture).await?;
            return Err(e);
        }
        Err(e) => return Err(e),
    };
    members::touch_shelter_event(&state, &accepted.shelter_id).await;

    Ok((
        StatusCode::CREATED,
        Json(view::accepted(&args.secret, &person_id, &accepted)),
    ))
}
