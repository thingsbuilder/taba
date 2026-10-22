//! The rules of a search (spec 3.7, 8.7): who may register one, the day of the quota, which photos
//! count and how the body of the request is validated. Everything here is pure.

use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use firestore::FirestoreInstant;
use firestore::jiff::tz::TimeZone;
use serde::Deserialize;
use serde_json::{Value, json};

use super::model::{Family, MissingPerson, SearchInput, forbidden, invalid};
use crate::auth::{Membership, Volunteer};
use crate::error::ApiError;
use crate::shelters::Confirmation;
use crate::store;
use crate::volunteers::GlobalRole;

pub(super) const MAX_PHOTOS: usize = 5;
pub(super) const MAX_PHOTO_BYTES: usize = 800 * 1024;
pub(super) const MAX_TEXT_CHARS: usize = 100;
pub(super) const MAX_NOTES_CHARS: usize = 1000;
/// The membership a search is registered from: a `confirmed` shelter's. `None` with `Ok` for
/// civil defense, `mediator` and `protection` without one; `403` for anyone else (spec 3.7 item 1).
pub(super) fn registering_membership(
    volunteer: &Volunteer,
) -> Result<Option<&Membership>, ApiError> {
    let from_shelter = volunteer
        .memberships
        .iter()
        .filter(|m| m.confirmation == Confirmation::Confirmed)
        .min_by(|a, b| a.shelter_id.cmp(&b.shelter_id));
    let by_role = [
        GlobalRole::CivilDefense,
        GlobalRole::Mediator,
        GlobalRole::Protection,
    ]
    .into_iter()
    .any(|role| volunteer.has_global(role));
    if from_shelter.is_some() || by_role {
        Ok(from_shelter)
    } else {
        Err(forbidden())
    }
}
/// The calendar day of `at` in the pilot's time zone (`TZ_PILOT`), as `YYYY-MM-DD`: the day the
/// daily search quota follows, so it turns over at midnight there and not at 21:00 in Brazil.
pub fn quota_day(at: FirestoreInstant, tz: &str) -> String {
    let zone = TimeZone::get(tz).unwrap_or(TimeZone::UTC);
    at.to_zoned(zone).date().to_string()
}

/// One counter per volunteer and day of the pilot's time zone.
pub(super) fn quota_id(volunteer_id: &str, tz: &str) -> String {
    format!("searchday~{volunteer_id}~{}", quota_day(store::now(), tz))
}
/// A photo Unico cannot use (no face, inconclusive, refused) is not an error: it only does not
/// count in `usable_photos`. Anything else (degraded mode, Unico down) stops the search.
pub(super) fn is_unusable_photo(error: &ApiError) -> bool {
    matches!(
        error.code,
        "no_face" | "capture_inconclusive" | "photo_rejected" | "invalid_image" | "image_too_large"
    )
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SearchRequest {
    id: String,
    photos_base64: Vec<String>,
    missing_person: MissingPerson,
    family: Family,
}

pub(super) fn text(value: &str, field: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() || value.chars().count() > MAX_TEXT_CHARS {
        return Err(invalid(field));
    }
    Ok(())
}

pub(super) fn decode_photo(base64: &str) -> Result<Vec<u8>, ApiError> {
    let bytes = STANDARD
        .decode(base64.trim())
        .map_err(|_| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_image"))?;
    if bytes.len() > MAX_PHOTO_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "image_too_large",
        ));
    }
    Ok(bytes)
}

/// The body of a search, validated: 1 to 5 photos, each decoded and at most 800 KB (spec 8.7).
pub fn parse_input(body: Value) -> Result<SearchInput, ApiError> {
    let request: SearchRequest = serde_json::from_value(body).map_err(|err| {
        let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
        error.detail = json!({ "message": err.to_string() });
        error
    })?;
    uuid::Uuid::parse_str(&request.id).map_err(|_| invalid("id"))?;
    if request.photos_base64.is_empty() || request.photos_base64.len() > MAX_PHOTOS {
        return Err(invalid("photos_base64"));
    }
    let (missing, family) = (&request.missing_person, &request.family);
    text(&missing.name, "missing_person.name")?;
    if missing
        .nickname
        .as_ref()
        .is_some_and(|n| n.chars().count() > MAX_TEXT_CHARS)
    {
        return Err(invalid("missing_person.nickname"));
    }
    if missing.notes.chars().count() > MAX_NOTES_CHARS {
        return Err(invalid("missing_person.notes"));
    }
    text(&family.name, "family.name")?;
    text(&family.phone, "family.phone")?;
    text(&family.relationship, "family.relationship")?;
    if !["pt-BR", "en", "es"].contains(&family.language.as_str()) {
        return Err(invalid("family.language"));
    }
    let photos = request
        .photos_base64
        .iter()
        .map(|p| decode_photo(p))
        .collect::<Result<_, _>>()?;
    Ok(SearchInput {
        id: request.id,
        photos,
        missing_person: request.missing_person,
        family: request.family,
    })
}
