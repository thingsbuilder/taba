//! Search for missing people (spec 3.7, 7, 8.7): `POST /v1/searches` and the photo pipeline that
//! the family link shares.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use firestore::jiff::SignedDuration;
use firestore::jiff::tz::TimeZone;
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

#[derive(Debug, Serialize, Deserialize)]
struct DayCount {
    count: u32,
    #[serde(with = "firestore::serialize_as_timestamp")]
    expires_at: FirestoreInstant,
}

/// The calendar day of `at` in the pilot's time zone (`TZ_PILOT`), as `YYYY-MM-DD`: the day the
/// daily search quota follows, so it turns over at midnight there and not at 21:00 in Brazil.
pub fn quota_day(at: FirestoreInstant, tz: &str) -> String {
    let zone = TimeZone::get(tz).unwrap_or(TimeZone::UTC);
    at.to_zoned(zone).date().to_string()
}

/// One counter per volunteer and day of the pilot's time zone.
fn quota_id(volunteer_id: &str, tz: &str) -> String {
    format!("searchday~{volunteer_id}~{}", quota_day(store::now(), tz))
}

/// Takes one of the `limit` daily slots (`SEARCHES_PER_DAY`) of the volunteer inside the caller's
/// transaction: `429 search_limit` when none is left. Searches and family links share it.
pub async fn take_daily_slot(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    volunteer_id: &str,
    limit: u32,
    tz: &str,
) -> Result<(), ApiError> {
    let id = quota_id(volunteer_id, tz);
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
async fn day_is_full(
    db: &FirestoreDb,
    volunteer_id: &str,
    limit: u32,
    tz: &str,
) -> Result<bool, ApiError> {
    let used = store::get::<DayCount>(db, QUOTA_COLLECTION, &quota_id(volunteer_id, tz))
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
    tz: &str,
) -> Result<u32, ApiError> {
    store::transact(db, |db, tx| {
        let (owner, input, tz) = (owner.clone(), input.clone(), tz.to_string());
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
                take_daily_slot(&db, tx, &owner.volunteer_id, limit, &tz).await?;
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
        if day_is_full(
            &state.db,
            &owner.volunteer_id,
            limit,
            &state.config.tz_pilot,
        )
        .await?
        {
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
    // A family link whose photos all fail makes no search: the family is asked for other photos.
    if usable_photos == 0 && owner.via == SearchVia::FamilyLink {
        return Ok(RegisterOutcome { usable_photos: 0 });
    }
    let usable_photos = commit_search(
        &state.db,
        &owner,
        &input,
        &person_ids,
        usable_photos,
        limit,
        &state.config.tz_pilot,
    )
    .await?;
    Ok(RegisterOutcome { usable_photos })
}

/// The pipeline both `POST /v1/searches` and the family link's submit run (spec 3.7 items 5 and
/// 11): one token per photo, keep only the derived `person_id`s, write the search, try the match
/// now (`matching`), and answer the same whether or not there was one. `503 unico_degraded` when
/// Unico cannot be used, without writing anything. The daily limit is not taken here: the route
/// takes it, and the family link takes it when the link is made (`take_daily_slot`).
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
    crate::events::ensure_open(&state.db, membership.and_then(|m| m.event_id.as_deref())).await?;
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
    let event_id = owner.event_id.clone();
    let outcome = register(&state, owner, input, Some(limit)).await?;
    crate::events::touch(&state, event_id.as_deref()).await;
    Ok(answer(outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matches::{self, MatchDoc, MatchStatus, MediatorKind};
    use crate::people::{self, PersonDoc};
    use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, audit_actions, new_id};
    use std::sync::Arc;

    fn photo() -> Value {
        json!(STANDARD.encode(b"a face"))
    }

    fn finished(token: &str) -> (u16, Value) {
        (
            200,
            json!({ "id": "img", "state": "PROCESS_STATE_FINISHED", "data": { "humanToken": { "value": token } } }),
        )
    }

    /// The next photos sent to Unico come back as these tokens, in order.
    fn tokens(app: &TestApp, tokens: &[&str]) {
        app.unico.with(|s| {
            tokens
                .iter()
                .for_each(|t| s.post_replies.push_back(finished(t)))
        });
    }

    fn refuse_next(app: &TestApp) {
        app.unico.with(|s| {
            s.post_replies
                .push_back((400, json!({ "code": 3, "message": "INVALID_REQUEST" })))
        });
    }

    fn body(id: &str, photos: usize) -> Value {
        json!({
            "id": id,
            "photos_base64": (0..photos).map(|_| photo()).collect::<Vec<_>>(),
            "missing_person": { "name": "Joao", "nickname": "Jo", "notes": "camisa azul" },
            "family": { "name": "Maria", "phone": "+5551999999999", "relationship": "mae", "language": "pt-BR" },
        })
    }

    async fn search(app: &TestApp, who: &Actor, body: Value) -> (StatusCode, Value) {
        app.send("POST", "/v1/searches", &who.secret, Some(body))
            .await
    }

    fn person_id(app: &TestApp, token: &str) -> String {
        app.state.person_id_for_token(token)
    }

    /// A check-in by photo of whoever `token` is, as `who` in `shelter`.
    async fn checkin(app: &TestApp, who: &Actor, shelter: &str, token: &str, extra: Value) {
        tokens(app, &[token]);
        let mut request = json!({
            "id": new_id(),
            "legal_basis": "consent",
            "client_created_at": store::now().to_string(),
            "data": {},
            "photo_base64": photo(),
        });
        for (key, value) in extra.as_object().unwrap() {
            request[key] = value.clone();
        }
        let (status, answer) = app
            .send(
                "POST",
                &format!("/v1/shelters/{shelter}/checkins"),
                &who.secret,
                Some(request),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
    }

    async fn matches_of(app: &TestApp) -> Vec<MatchDoc> {
        app.state
            .db
            .fluent()
            .select()
            .from(matches::COLLECTION)
            .obj()
            .query()
            .await
            .unwrap()
    }

    async fn stored_search(app: &TestApp, id: &str) -> SearchDoc {
        store::get(&app.state.db, COLLECTION, id)
            .await
            .unwrap()
            .unwrap()
    }

    /// Ana, admin of a confirmed shelter.
    async fn world() -> (TestApp, Actor, String) {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let shelter = app.confirmed_shelter(&ana, "Ginasio", ORIGIN).await;
        (app, ana, shelter)
    }

    // ---- who may search (spec 3.7 item 1) ----

    #[tokio::test]
    async fn who_may_register_a_search() {
        let (app, ana, _) = world().await;
        // A volunteer of a confirmed shelter.
        tokens(&app, &["t1"]);
        let (status, answer) = search(&app, &ana, body(&new_id(), 1)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{answer}");

        // An unconfirmed shelter's admin, a volunteer with no shelter and a validator alone: 403.
        let bia = app.actor("Bia").await;
        app.create_shelter(&bia, "Escola", (ORIGIN.0 + 0.5, ORIGIN.1))
            .await;
        let caio = app.actor("Caio").await;
        let validator = app.actor("Val").await;
        app.grant(&validator, GlobalRole::Validator).await;
        for who in [&bia, &caio, &validator] {
            let (status, answer) = search(&app, who, body(&new_id(), 1)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
            assert_eq!(answer["error"], "forbidden");
        }

        // Civil defense, mediator and protection outside any shelter.
        for (name, role) in [
            ("Dora", GlobalRole::CivilDefense),
            ("Eli", GlobalRole::Mediator),
            ("Fabi", GlobalRole::Protection),
        ] {
            let who = app.actor(name).await;
            app.grant(&who, role).await;
            tokens(&app, &["t-role"]);
            let id = new_id();
            let (status, answer) = search(&app, &who, body(&id, 1)).await;
            assert_eq!(status, StatusCode::ACCEPTED, "{answer}");
            assert_eq!(stored_search(&app, &id).await.origin_shelter_id, None);
        }

        // No session.
        let (status, _, _) = app
            .request("POST", "/v1/searches", &[], Some(body(&new_id(), 1)))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_bad_body_is_refused_before_unico_is_asked() {
        let (app, ana, _) = world().await;
        let before = app.unico.post_count();
        let mut cases = vec![
            ("no photos", body(&new_id(), 0)),
            ("six photos", body(&new_id(), 6)),
        ];
        let mut not_base64 = body(&new_id(), 1);
        not_base64["photos_base64"] = json!(["***"]);
        cases.push(("not base64", not_base64));
        let mut blank_name = body(&new_id(), 1);
        blank_name["missing_person"]["name"] = json!("  ");
        cases.push(("blank name", blank_name));
        let mut language = body(&new_id(), 1);
        language["family"]["language"] = json!("fr");
        cases.push(("language", language));
        let mut not_uuid = body("x", 1);
        not_uuid["id"] = json!("x");
        cases.push(("id", not_uuid));
        for (what, case) in cases {
            let (status, answer) = search(&app, &ana, case).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{what}: {answer}");
        }
        let mut big = body(&new_id(), 1);
        big["photos_base64"] = json!([STANDARD.encode(vec![0u8; MAX_PHOTO_BYTES + 1])]);
        let (status, answer) = search(&app, &ana, big).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{answer}");
        assert_eq!(app.unico.post_count(), before);
        assert!(all_docs(&app.state.db, COLLECTION).await.is_empty());
    }

    // ---- what is kept ----

    #[tokio::test]
    async fn only_the_derived_person_ids_are_kept_and_usable_photos_counts_the_tokens() {
        let (app, ana, shelter) = world().await;
        // Three photos: a token, a refusal, the same person again. Two photos gave a token.
        tokens(&app, &["tok-joao"]);
        refuse_next(&app);
        tokens(&app, &["tok-joao"]);
        let id = new_id();
        let (status, answer) = search(&app, &ana, body(&id, 3)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{answer}");
        assert_eq!(
            answer,
            json!({ "message": "search_registered", "usable_photos": 2 })
        );
        let doc = stored_search(&app, &id).await;
        assert_eq!(doc.person_ids, vec![person_id(&app, "tok-joao")]);
        assert_eq!((doc.photos_sent, doc.usable_photos), (3, 2));
        assert_eq!(
            (doc.created_by.as_str(), doc.via, doc.status),
            (ana.id.as_str(), SearchVia::Volunteer, SearchStatus::Active)
        );
        assert_eq!(doc.origin_shelter_id.as_deref(), Some(shelter.as_str()));
        let raw = all_docs(&app.state.db, COLLECTION).await;
        let raw = raw[0].to_string();
        assert!(!raw.contains("tok-joao") && !raw.contains(&STANDARD.encode(b"a face")));

        // Who searched is audited, under vital interest.
        let audits = all_docs(&app.state.db, "audit_log").await;
        let entry = audits
            .iter()
            .find(|e| e["action"] == "search.create")
            .unwrap();
        assert_eq!(entry["volunteer_id"], json!(ana.id));
        assert_eq!(entry["legal_basis"], "vital_interest");
        assert_eq!(entry["target_id"], json!(id));
    }

    // ---- the answer never tells (criterion 47) ----

    #[tokio::test]
    async fn the_answer_is_the_same_with_and_without_a_match() {
        let (app, ana, shelter) = world().await;
        checkin(&app, &ana, &shelter, "tok-found", json!({})).await;
        let (without, with) = (new_id(), new_id());
        tokens(&app, &["tok-nobody"]);
        let (status_a, a) = search(&app, &ana, body(&without, 1)).await;
        tokens(&app, &["tok-found"]);
        let (status_b, b) = search(&app, &ana, body(&with, 1)).await;
        assert_eq!(
            (status_a, status_b),
            (StatusCode::ACCEPTED, StatusCode::ACCEPTED)
        );
        assert_eq!(a.to_string(), b.to_string());
        // The match exists, and the answer says nothing about it.
        let all = matches_of(&app).await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].search_id, with);
        assert!(!b.to_string().contains("match"));
    }

    // ---- matching (criteria 51, 50, 46) ----

    #[tokio::test]
    async fn a_later_check_in_creates_one_match_and_repeating_it_creates_no_other() {
        let (app, ana, shelter) = world().await;
        let (search_id, found) = (new_id(), person_id(&app, "tok-found"));
        tokens(&app, &["tok-found"]);
        search(&app, &ana, body(&search_id, 1)).await;
        assert!(matches_of(&app).await.is_empty());

        checkin(&app, &ana, &shelter, "tok-found", json!({})).await;
        let all = matches_of(&app).await;
        assert_eq!(all.len(), 1);
        let m = &all[0];
        assert_eq!(
            m.id.as_deref(),
            Some(matching::match_id(&search_id, &found).as_str())
        );
        assert_eq!(
            (m.search_id.as_str(), m.person_id.as_str()),
            (search_id.as_str(), found.as_str())
        );
        assert_eq!(
            (
                m.mediator_kind,
                m.mediator_shelter_id.as_deref(),
                m.status,
                m.is_child
            ),
            (
                MediatorKind::Shelter,
                Some(shelter.as_str()),
                MatchStatus::Open,
                false
            )
        );

        checkin(&app, &ana, &shelter, "tok-found", json!({})).await;
        assert_eq!(matches_of(&app).await.len(), 1);
    }

    #[tokio::test]
    async fn a_search_is_compared_with_the_people_already_registered() {
        let (app, ana, shelter) = world().await;
        checkin(&app, &ana, &shelter, "tok-found", json!({})).await;
        tokens(&app, &["tok-other", "tok-found"]);
        let id = new_id();
        search(&app, &ana, body(&id, 2)).await;
        let all = matches_of(&app).await;
        assert_eq!(all.len(), 1, "a match in any photo counts");
        assert_eq!(all[0].person_id, person_id(&app, "tok-found"));
        // Searching again with the same id changes nothing.
        let (status, _) = search(&app, &ana, body(&id, 2)).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(matches_of(&app).await.len(), 1);
    }

    #[tokio::test]
    async fn a_closed_pair_never_gets_a_new_match() {
        let (app, ana, shelter) = world().await;
        tokens(&app, &["tok-found"]);
        search(&app, &ana, body(&new_id(), 1)).await;
        checkin(&app, &ana, &shelter, "tok-found", json!({})).await;
        for status in [MatchStatus::NotThePerson, MatchStatus::Declined] {
            let mut m = matches_of(&app).await.remove(0);
            let id = m.id.take().unwrap();
            m.status = status;
            store::replace(&app.state.db, matches::COLLECTION, &id, &m)
                .await
                .unwrap();
            checkin(&app, &ana, &shelter, "tok-found", json!({})).await;
            let all = matches_of(&app).await;
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].status, status, "the closed match was left alone");
        }
    }

    #[tokio::test]
    async fn the_queue_follows_the_person_found() {
        let (app, ana, confirmed) = world().await;
        let bia = app.actor("Bia").await;
        let unconfirmed = app
            .create_shelter(&bia, "Escola", (ORIGIN.0 + 0.5, ORIGIN.1))
            .await;
        for t in ["tok-child", "tok-unconfirmed", "tok-confirmed"] {
            tokens(&app, &[t]);
            search(&app, &ana, body(&new_id(), 1)).await;
        }
        checkin(
            &app,
            &ana,
            &confirmed,
            "tok-child",
            json!({ "legal_basis": "child_protection", "data": { "is_child": true, "child_status": "unaccompanied" } }),
        )
        .await;
        checkin(&app, &bia, &unconfirmed, "tok-unconfirmed", json!({})).await;
        checkin(&app, &ana, &confirmed, "tok-confirmed", json!({})).await;
        let all = matches_of(&app).await;
        assert_eq!(all.len(), 3);
        let by_person = |token: &str| {
            let id = person_id(&app, token);
            all.iter().find(|m| m.person_id == id).unwrap().clone()
        };
        let child = by_person("tok-child");
        assert_eq!(
            (
                child.mediator_kind,
                child.mediator_shelter_id,
                child.is_child
            ),
            (MediatorKind::Protection, None, true)
        );
        let other = by_person("tok-unconfirmed");
        assert_eq!(
            (other.mediator_kind, other.mediator_shelter_id),
            (MediatorKind::Central, None)
        );
        let here = by_person("tok-confirmed");
        assert_eq!(
            (here.mediator_kind, here.mediator_shelter_id),
            (MediatorKind::Shelter, Some(confirmed))
        );
    }

    #[tokio::test]
    async fn a_closed_search_is_not_compared() {
        let (app, ana, shelter) = world().await;
        let id = new_id();
        tokens(&app, &["tok-found"]);
        search(&app, &ana, body(&id, 1)).await;
        let mut doc = stored_search(&app, &id).await;
        doc.status = SearchStatus::Closed;
        store::replace(&app.state.db, COLLECTION, &id, &doc)
            .await
            .unwrap();
        checkin(&app, &ana, &shelter, "tok-found", json!({})).await;
        assert!(matches_of(&app).await.is_empty());
    }

    // ---- the daily limit (criterion 47) ----

    #[tokio::test]
    async fn the_daily_limit_answers_429_before_unico_is_asked_and_repeats_do_not_count() {
        let (mut app, ana, _) = world().await;
        Arc::make_mut(&mut app.state.config).searches_per_day = 3;
        let first = new_id();
        for id in [first.clone(), new_id(), new_id()] {
            tokens(&app, &["t"]);
            let (status, answer) = search(&app, &ana, body(&id, 1)).await;
            assert_eq!(status, StatusCode::ACCEPTED, "{answer}");
        }
        // A repeat of one already written is not a new search.
        let paid = app.unico.post_count();
        let (status, _) = search(&app, &ana, body(&first, 1)).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(app.unico.post_count(), paid);

        let (status, answer) = search(&app, &ana, body(&new_id(), 1)).await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::TOO_MANY_REQUESTS, Some("search_limit"))
        );
        assert_eq!(app.unico.post_count(), paid, "Unico was not asked");
        assert_eq!(all_docs(&app.state.db, COLLECTION).await.len(), 3);

        // The limit is per volunteer.
        let bia = app.actor("Bia").await;
        app.grant(&bia, GlobalRole::Mediator).await;
        tokens(&app, &["t"]);
        let (status, _) = search(&app, &bia, body(&new_id(), 1)).await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[test]
    fn the_quota_day_follows_the_pilot_time_zone_not_utc() {
        let at = |text: &str| text.parse::<FirestoreInstant>().unwrap();
        let brt = "America/Sao_Paulo";
        // 21:00 in Brazil is already the next day in UTC, and the quota does not restart there.
        assert_eq!(quota_day(at("2026-10-22T00:00:00Z"), brt), "2026-10-21");
        assert_eq!(quota_day(at("2026-10-22T02:59:59Z"), brt), "2026-10-21");
        // Midnight in Brazil it does.
        assert_eq!(quota_day(at("2026-10-22T03:00:00Z"), brt), "2026-10-22");
        assert_eq!(quota_day(at("2026-10-22T00:00:00Z"), "UTC"), "2026-10-22");
        assert_ne!(quota_id("v", brt), quota_id("w", brt));
    }

    async fn take(app: &TestApp, volunteer_id: &str, limit: u32) -> Result<(), ApiError> {
        let id = volunteer_id.to_string();
        store::transact(&app.state.db, |db, tx| {
            let id = id.clone();
            Box::pin(async move { take_daily_slot(&db, tx, &id, limit, "America/Sao_Paulo").await })
        })
        .await
    }

    #[tokio::test]
    async fn the_slot_is_taken_in_the_callers_transaction_and_concurrent_takers_cannot_pass_the_limit()
     {
        let (app, ana, _) = world().await;
        assert!(take(&app, &ana.id, 2).await.is_ok());
        assert!(take(&app, &ana.id, 2).await.is_ok());
        assert_eq!(
            take(&app, &ana.id, 2).await.unwrap_err().code,
            "search_limit"
        );
        // Eleven concurrent takers of ten slots: exactly ten win.
        let bia = app.actor("Bia").await;
        let mut handles = Vec::new();
        for _ in 0..11 {
            let (app, id) = (app.clone(), bia.id.clone());
            handles.push(tokio::spawn(async move { take(&app, &id, 10).await }));
        }
        let mut won = 0;
        for handle in handles {
            match handle.await.unwrap() {
                Ok(()) => won += 1,
                Err(e) => assert_eq!(e.code, "search_limit"),
            }
        }
        assert_eq!(won, 10);
    }

    // ---- degraded mode ----

    #[tokio::test]
    async fn in_degraded_mode_nothing_is_written_and_no_slot_is_taken() {
        let (app, ana, _) = world().await;
        crate::degraded::enter_limit(&app.state.db).await.unwrap();
        let (status, answer) = search(&app, &ana, body(&new_id(), 2)).await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, Some("unico_degraded"))
        );
        assert!(all_docs(&app.state.db, COLLECTION).await.is_empty());
        let quota = all_docs(&app.state.db, QUOTA_COLLECTION).await;
        assert!(quota.iter().all(|d| d.get("count").is_none()));
    }

    #[tokio::test]
    async fn a_retry_with_the_same_key_does_not_pay_the_photos_twice() {
        let (app, ana, _) = world().await;
        // The first photo is paid; the second fails for good (Unico down), so nothing is written.
        tokens(&app, &["t1"]);
        app.unico
            .with(|s| s.post_replies.extend((0..5).map(|_| (500, json!({})))));
        let (id, key) = (new_id(), new_id());
        let (status, _) = app
            .send_keyed(
                "POST",
                "/v1/searches",
                &ana.secret,
                &key,
                Some(body(&id, 2)),
            )
            .await;
        assert!(!status.is_success());
        assert!(all_docs(&app.state.db, COLLECTION).await.is_empty());
        let paid = app.unico.post_count();
        app.unico.with(|s| s.post_replies.clear());
        tokens(&app, &["t2"]);
        let (status, answer) = app
            .send_keyed(
                "POST",
                "/v1/searches",
                &ana.secret,
                &key,
                Some(body(&id, 2)),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{answer}");
        assert_eq!(
            app.unico.post_count(),
            paid + 1,
            "only the second photo was paid again"
        );
        assert_eq!(stored_search(&app, &id).await.usable_photos, 2);
    }

    // ---- concurrency ----

    /// What `checkin_in` does for a new person, without the photo: write the person and look
    /// for searches in one transaction.
    async fn register_person(app: &TestApp, template: &PersonDoc, id: &str, shelter: &str) {
        let shelter: crate::shelters::ShelterDoc =
            store::get(&app.state.db, crate::shelters::COLLECTION, shelter)
                .await
                .unwrap()
                .unwrap();
        store::transact(&app.state.db, |db, tx| {
            let (person, id, shelter) = (template.clone(), id.to_string(), shelter.clone());
            Box::pin(async move {
                store::tx_replace(&db, tx, people::COLLECTION, &id, &person)?;
                matching::on_person_registered(&db, tx, &person, &id, Some(&shelter)).await
            })
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn concurrent_searches_and_registrations_of_one_person_make_one_match_per_search() {
        let (app, ana, shelter) = world().await;
        checkin(&app, &ana, &shelter, "tok-template", json!({})).await;
        let template: PersonDoc = store::get(
            &app.state.db,
            people::COLLECTION,
            &person_id(&app, "tok-template"),
        )
        .await
        .unwrap()
        .unwrap();
        let owner = SearchOwner {
            volunteer_id: ana.id.clone(),
            device_id: None,
            origin_shelter_id: None,
            event_id: None,
            via: SearchVia::Volunteer,
            key: None,
        };
        let target = person_id(&app, "tok-target");
        let search_ids: Vec<String> = (0..3).map(|_| new_id()).collect();
        let mut handles = Vec::new();
        for search_id in &search_ids {
            let (db, owner, target) = (app.state.db.clone(), owner.clone(), target.clone());
            let input = SearchInput {
                id: search_id.clone(),
                photos: vec![],
                missing_person: MissingPerson {
                    name: "Joao".into(),
                    nickname: None,
                    notes: String::new(),
                },
                family: Family {
                    name: "Maria".into(),
                    phone: "1".into(),
                    relationship: "mae".into(),
                    language: "en".into(),
                },
            };
            handles.push(tokio::spawn(async move {
                commit_search(&db, &owner, &input, &[target], 1, None, "UTC")
                    .await
                    .unwrap();
            }));
        }
        for _ in 0..6 {
            let (app, template, target, shelter) = (
                app.clone(),
                template.clone(),
                target.clone(),
                shelter.clone(),
            );
            handles.push(tokio::spawn(async move {
                register_person(&app, &template, &target, &shelter).await
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        let mut ids: Vec<String> = matches_of(&app)
            .await
            .iter()
            .filter_map(|m| m.id.clone())
            .collect();
        ids.sort();
        let mut expected: Vec<String> = search_ids
            .iter()
            .map(|s| matching::match_id(s, &target))
            .collect();
        expected.sort();
        assert_eq!(ids, expected, "one match for each search, none twice");
        let created = audit_actions(&app.state.db)
            .await
            .iter()
            .filter(|a| *a == "match.create")
            .count();
        assert_eq!(created, 3);
    }
}
