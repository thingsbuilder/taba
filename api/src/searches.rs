//! Search for missing people (spec 3.7, 7, 8.7): `POST /v1/searches` and the photo pipeline that
//! the family link shares.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use firestore::jiff::SignedDuration;
use firestore::{FirestoreDb, FirestoreInstant, FirestoreTransaction};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Membership, Volunteer};
use crate::error::ApiError;
use crate::idempotency;
use crate::matching;
use crate::shelters::Confirmation;
use crate::store::{self, tx_create, tx_replace};
use crate::volunteers::GlobalRole;

pub const COLLECTION: &str = "searches";

/// Who made the search: the volunteer typing it, or the family through a link the volunteer made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchVia {
    Volunteer,
    FamilyLink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchStatus {
    Active,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissingPerson {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Family {
    pub name: String,
    pub phone: String,
    pub relationship: String,
    /// `pt-BR`, `en` or `es`: the language of the WhatsApp message (spec 11.1).
    pub language: String,
}

/// `searches/{search_id}` (UUID of the client), spec 7. Only the `person_id` derived from each
/// photo is kept, never the photo or the token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    /// The volunteer in whose name the search is: who typed it, or who made the family link.
    pub created_by: String,
    pub via: SearchVia,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_shelter_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default)]
    pub person_ids: Vec<String>,
    pub photos_sent: u32,
    pub usable_photos: u32,
    pub missing_person: MissingPerson,
    pub family: Family,
    pub status: SearchStatus,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub expiry_notified_at: Option<FirestoreInstant>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
}

/// Who a registered search belongs to, for `register_photos`.
#[derive(Debug, Clone)]
pub struct SearchOwner {
    pub volunteer_id: String,
    pub device_id: Option<String>,
    pub origin_shelter_id: Option<String>,
    pub event_id: Option<String>,
    pub via: SearchVia,
    /// What keeps the paid answer of Unico per photo for a retry: the `Idempotency-Key` of
    /// `POST /v1/searches`, or the link code of the family link. The photos are never paid twice.
    pub key: Option<String>,
}

/// What the search needs, already validated by the route: 1 to 5 photos, each decoded and
/// at most 800 KB.
#[derive(Debug, Clone)]
pub struct SearchInput {
    pub id: String,
    pub photos: Vec<Vec<u8>>,
    pub missing_person: MissingPerson,
    pub family: Family,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisterOutcome {
    pub usable_photos: u32,
}

const MAX_PHOTOS: usize = 5;
const MAX_PHOTO_BYTES: usize = 800 * 1024;
const MAX_TEXT_CHARS: usize = 100;
const MAX_NOTES_CHARS: usize = 1000;
const ROUTE: &str = "POST /v1/searches";
/// The quota counters live in the `idempotency` collection, which already has the TTL on `expires_at`.
const QUOTA_COLLECTION: &str = idempotency::COLLECTION;
const QUOTA_TTL: SignedDuration = SignedDuration::from_hours(48);

fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
    error.detail = json!({ "field": field });
    error
}

/// The one answer of a registered search, with or without a match (spec 3.7 item 6).
pub fn answer(outcome: RegisterOutcome) -> (StatusCode, Json<Value>) {
    (
        StatusCode::ACCEPTED,
        Json(json!({ "message": "search_registered", "usable_photos": outcome.usable_photos })),
    )
}

// ---- who may search, and how many ----

/// The membership a search is registered from: a `confirmed` shelter's. `None` with `Ok` for
/// civil defense, `mediator` and `protection` without one; `403` for anyone else (spec 3.7 item 1).
fn registering_membership(volunteer: &Volunteer) -> Result<Option<&Membership>, ApiError> {
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

/// `403` when the volunteer may not register searches; otherwise the id of the shelter the
/// search comes from (`None` for civil defense, `mediator` and `protection` outside a shelter).
#[allow(dead_code)] // called by search_links.rs (owner E)
pub fn registering_shelter(volunteer: &Volunteer) -> Result<Option<String>, ApiError> {
    Ok(registering_membership(volunteer)?.map(|m| m.shelter_id.clone()))
}

#[derive(Debug, Serialize, Deserialize)]
struct DayCount {
    count: u32,
    #[serde(with = "firestore::serialize_as_timestamp")]
    expires_at: FirestoreInstant,
}

/// One counter per volunteer and UTC day.
fn quota_id(volunteer_id: &str) -> String {
    let now = store::now().to_string();
    format!("searchday~{volunteer_id}~{}", &now[..10])
}

/// Takes one of the `limit` daily slots (`SEARCHES_PER_DAY`) of the volunteer inside the caller's
/// transaction: `429 search_limit` when none is left. Searches and family links share it.
pub async fn take_daily_slot(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    volunteer_id: &str,
    limit: u32,
) -> Result<(), ApiError> {
    let id = quota_id(volunteer_id);
    let used = store::get::<DayCount>(db, QUOTA_COLLECTION, &id)
        .await?
        .map_or(0, |c| c.count);
    if used >= limit {
        return Err(ApiError::search_limit());
    }
    let next = DayCount {
        count: used + 1,
        expires_at: store::now() + QUOTA_TTL,
    };
    tx_replace(db, tx, QUOTA_COLLECTION, &id, &next)
}

/// Whether the day is already used up: a cheap look before paying Unico for the photos. The
/// slot itself is taken in the transaction that writes the search.
async fn day_is_full(db: &FirestoreDb, volunteer_id: &str, limit: u32) -> Result<bool, ApiError> {
    let used = store::get::<DayCount>(db, QUOTA_COLLECTION, &quota_id(volunteer_id))
        .await?
        .map_or(0, |c| c.count);
    Ok(used >= limit)
}

// ---- the pipeline ----

/// A photo Unico cannot use (no face, inconclusive, refused) is not an error: it only does not
/// count in `usable_photos`. Anything else (degraded mode, Unico down) stops the search.
fn is_unusable_photo(error: &ApiError) -> bool {
    matches!(
        error.code,
        "no_face" | "capture_inconclusive" | "photo_rejected" | "invalid_image" | "image_too_large"
    )
}

/// One token per photo; only the derived `person_id`s come out, in the order of the photos.
/// Each paid answer is kept under the owner's key, so a retry does not pay it again.
async fn person_ids_of(
    state: &AppState,
    owner: &SearchOwner,
    photos: &[Vec<u8>],
) -> Result<Vec<String>, ApiError> {
    let mut found = Vec::new();
    for (index, photo) in photos.iter().enumerate() {
        let mut headers = HeaderMap::new();
        if let Some(key) = owner.key.as_deref().filter(|k| !k.is_empty()) {
            if let Ok(value) = format!("{key}.{index}").parse() {
                headers.insert("idempotency-key", value);
            }
        }
        let asked =
            idempotency::photo_person_id(state, &headers, &owner.volunteer_id, ROUTE, async {
                let token = state.unico.token_from_image(photo).await?;
                Ok(state.person_id_for_token(&token))
            })
            .await;
        match asked {
            Ok(person_id) => found.push(person_id),
            Err(error) if is_unusable_photo(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(found)
}

/// Writes the search, takes the daily slot when `limit` is given, audits and compares it with the
/// people already registered, in one transaction. An id that already exists for the same owner
/// is a repeat: it answers as the first time and changes nothing.
async fn commit_search(
    db: &FirestoreDb,
    owner: &SearchOwner,
    input: &SearchInput,
    person_ids: &[String],
    usable_photos: u32,
    limit: Option<u32>,
) -> Result<u32, ApiError> {
    store::transact(db, |db, tx| {
        let (owner, input) = (owner.clone(), input.clone());
        let person_ids = person_ids.to_vec();
        Box::pin(async move {
            if let Some(existing) = store::get::<SearchDoc>(&db, COLLECTION, &input.id).await? {
                return if existing.created_by == owner.volunteer_id {
                    Ok(existing.usable_photos)
                } else {
                    Err(search_id_taken())
                };
            }
            if let Some(limit) = limit {
                take_daily_slot(&db, tx, &owner.volunteer_id, limit).await?;
            }
            let search = SearchDoc {
                id: None,
                created_by: owner.volunteer_id.clone(),
                via: owner.via,
                origin_shelter_id: owner.origin_shelter_id.clone(),
                event_id: owner.event_id.clone(),
                person_ids: person_ids.clone(),
                photos_sent: input.photos.len() as u32,
                usable_photos,
                missing_person: input.missing_person.clone(),
                family: input.family.clone(),
                status: SearchStatus::Active,
                expiry_notified_at: None,
                created_at: store::now(),
            };
            tx_create(&db, tx, COLLECTION, &input.id, &search)?;
            let mut entry = AuditEntry::new("search.create", "search", &input.id)
                .by(&owner.volunteer_id)
                .basis(LegalBasis::VitalInterest);
            if let Some(shelter_id) = &owner.origin_shelter_id {
                entry = entry.in_shelter(shelter_id);
            }
            if let Some(device) = &owner.device_id {
                entry = entry.on_device(device);
            }
            audit::add(&db, tx, &entry)?;
            matching::on_search_registered(&db, tx, &input.id, &person_ids).await?;
            Ok(usable_photos)
        })
    })
    .await
}

fn search_id_taken() -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "search_id_taken")
}

async fn register(
    state: &AppState,
    owner: SearchOwner,
    input: SearchInput,
    limit: Option<u32>,
) -> Result<RegisterOutcome, ApiError> {
    // A repeat of a search already written answers from it, without asking Unico again.
    if let Some(existing) = store::get::<SearchDoc>(&state.db, COLLECTION, &input.id).await? {
        if existing.created_by != owner.volunteer_id {
            return Err(search_id_taken());
        }
        return Ok(RegisterOutcome {
            usable_photos: existing.usable_photos,
        });
    }
    if let Some(limit) = limit {
        if day_is_full(&state.db, &owner.volunteer_id, limit).await? {
            return Err(ApiError::search_limit());
        }
    }
    let tokens = person_ids_of(state, &owner, &input.photos).await?;
    let usable_photos = tokens.len() as u32;
    let mut person_ids: Vec<String> = Vec::new();
    for id in tokens {
        if !person_ids.contains(&id) {
            person_ids.push(id);
        }
    }
    let usable_photos =
        commit_search(&state.db, &owner, &input, &person_ids, usable_photos, limit).await?;
    Ok(RegisterOutcome { usable_photos })
}

/// The pipeline both `POST /v1/searches` and the family link's submit run (spec 3.7 items 5 and
/// 11): one token per photo, keep only the derived `person_id`s, write the search, try the match
/// now (`matching`), and answer the same whether or not there was one. `503 unico_degraded` when
/// Unico cannot be used, without writing anything. The daily limit is not taken here: the route
/// takes it, and the family link takes it when the link is made (`take_daily_slot`).
#[allow(dead_code)] // called by search_links.rs (owner E)
pub async fn register_photos(
    state: &AppState,
    owner: SearchOwner,
    input: SearchInput,
) -> Result<RegisterOutcome, ApiError> {
    register(state, owner, input, None).await
}

// ---- POST /v1/searches ----

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchRequest {
    id: String,
    photos_base64: Vec<String>,
    missing_person: MissingPerson,
    family: Family,
}

fn text(value: &str, field: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() || value.chars().count() > MAX_TEXT_CHARS {
        return Err(invalid(field));
    }
    Ok(())
}

fn decode_photo(base64: &str) -> Result<Vec<u8>, ApiError> {
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

/// `POST /v1/searches`: `202 {message: "search_registered", usable_photos}`; `429 search_limit`.
pub async fn post_search(
    State(state): State<AppState>,
    volunteer: Volunteer,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let membership = registering_membership(&volunteer)?;
    let input = parse_input(body)?;
    let owner = SearchOwner {
        volunteer_id: volunteer.id.clone(),
        device_id: crate::auth::device_id(&headers).ok(),
        origin_shelter_id: membership.map(|m| m.shelter_id.clone()),
        event_id: membership.and_then(|m| m.event_id.clone()),
        via: SearchVia::Volunteer,
        key: headers
            .get("idempotency-key")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_string()),
    };
    let limit = state.config.searches_per_day;
    let outcome = register(&state, owner, input, Some(limit)).await?;
    Ok(answer(outcome))
}
