//! The routes (spec 5.2, 8.1): captures, sessions, and the extractor that turns a bearer secret
//! into a `Volunteer`.

use axum::Json;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::Value;

use super::domain::{CAPTURE_TTL, device_id, new_secret};
use super::model::{AppState, CaptureDoc, CaptureStatus, Purpose, Volunteer, unauthorized};
use super::persist::{self, Login, resolve_person, usable_capture};
use super::view;
use crate::audit::{AuditEntry, LegalBasis};
use crate::error::ApiError;
use crate::volunteers::{GlobalRole, ProfileInput};

// ---- POST /v1/captures ----

#[derive(Debug, Deserialize)]
pub struct CreateCapture {
    purpose: Purpose,
    #[serde(default)]
    shelter_id: Option<String>,
    #[serde(default)]
    expected_person_id: Option<String>,
}

pub async fn post_captures(
    State(state): State<AppState>,
    headers: HeaderMap,
    volunteer: Option<Volunteer>,
    Json(request): Json<CreateCapture>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let device = device_id(&headers)?;
    // `login` and `invite` are public (spec 10.4). The captures of phase 3 need a session and say
    // what they are for, so a capture can only be used for that shelter and that person:
    // `checkin` for a shelter the caller belongs to, `biometrics` for one person, `revalidation`
    // for one person in a shelter.
    let (volunteer_id, shelter_id, expected_person_id) = match request.purpose {
        Purpose::Login | Purpose::Invite => (None, None, None),
        purpose => {
            let volunteer = volunteer.ok_or_else(unauthorized)?;
            let target_required =
                || ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "capture_target_required");
            let needs_shelter = matches!(purpose, Purpose::Checkin | Purpose::Revalidation);
            let needs_person = matches!(purpose, Purpose::Biometrics | Purpose::Revalidation);
            if (needs_shelter && request.shelter_id.is_none())
                || (needs_person && request.expected_person_id.is_none())
            {
                return Err(target_required());
            }
            if let Some(shelter_id) = &request.shelter_id {
                let may = volunteer.role_in(shelter_id).is_some()
                    || (purpose == Purpose::Revalidation
                        && (volunteer.has_global(GlobalRole::CivilDefense)
                            || volunteer.has_global(GlobalRole::Protection)));
                if !may {
                    return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden"));
                }
            }
            (
                Some(volunteer.id),
                request.shelter_id,
                request.expected_person_id,
            )
        }
    };

    let capture_id = uuid::Uuid::new_v4().to_string();
    let callback = format!("https://{}/return?capture={capture_id}", state.domain);
    let journey = state.unico.create_journey(&callback).await?;

    let now = crate::store::now();

    let doc = CaptureDoc {
        process_id: journey.process_id,
        purpose: request.purpose,
        device_id: device.clone(),
        status: CaptureStatus::Created,
        person_id: None,
        volunteer_id,
        shelter_id,
        expected_person_id,
        consumed: false,
        created_at: now,
        expires_at: now + CAPTURE_TTL,
    };

    let entry = AuditEntry::new(super::ACTION_CAPTURE_CREATE, "capture", &capture_id)
        .on_device(&device)
        .basis(LegalBasis::FraudPrevention);
    persist::create_capture(&state, &capture_id, &doc, &entry).await?;

    Ok((
        StatusCode::CREATED,
        Json(view::capture_created(&capture_id, &journey.link, &doc)),
    ))
}

// ---- POST /v1/sessions ----

#[derive(Debug, Deserialize)]
pub struct CreateSession {
    capture_id: String,
    #[serde(default)]
    profile: Option<ProfileInput>,
}

pub async fn post_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateSession>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let device = device_id(&headers)?;
    let mut capture = usable_capture(&state, &device, &request.capture_id, Purpose::Login).await?;
    let person_id = resolve_person(&state, &request.capture_id, &mut capture).await?;

    let secret = new_secret();
    let args = Login {
        capture_id: request.capture_id,
        device_id: device,
        person_id: person_id.clone(),
        profile: request.profile,
        secret: secret.clone(),
        session_days: state.config.session_days,
    };

    let (volunteer, session) =
        persist::open_session(&state, &args, &mut capture, &person_id).await?;

    Ok((
        StatusCode::CREATED,
        Json(view::session_opened(
            &secret, &session, &person_id, &volunteer,
        )),
    ))
}

// ---- DELETE /v1/sessions/current ----

pub async fn delete_current_session(
    State(state): State<AppState>,
    volunteer: Volunteer,
) -> Result<StatusCode, ApiError> {
    let entry = AuditEntry::new(
        super::ACTION_SESSION_DELETE,
        "session",
        &volunteer.session_id,
    )
    .basis(LegalBasis::FraudPrevention)
    .by(&volunteer.id);
    persist::delete_session(&state, &volunteer.session_id, &entry).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- extractor ----

impl axum::extract::OptionalFromRequestParts<AppState> for Volunteer {
    type Rejection = ApiError;

    /// `None` without an `Authorization` header; a bad session is still an error.
    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Option<Self>, ApiError> {
        if !parts.headers.contains_key("authorization") {
            return Ok(None);
        }
        <Self as FromRequestParts<AppState>>::from_request_parts(parts, state)
            .await
            .map(Some)
    }
}

impl FromRequestParts<AppState> for Volunteer {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let secret = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(unauthorized)?;

        persist::authenticate(state, secret).await
    }
}
