//! People (spec 3.4, 4.1, 4.11, 4.12, 7, 8.5). Owner A. Stage 0: data types and the routes
//! registered as `501 not_implemented`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use firestore::{FirestoreDb, FirestoreInstant, FirestoreTransaction, path};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::alerts::{self, AlertDoc, AlertKind};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{self, AppState, CaptureDoc, Purpose, Volunteer};
use crate::error::ApiError;
use crate::households::{self, HouseholdDoc};
use crate::members::ShelterRole;
use crate::shelters::{self, Confirmation, ShelterDoc};
use crate::store::{self, fs_error, tx_delete, tx_replace};
use crate::thumbnail;
use crate::volunteers::GlobalRole;

const CAPTURES: &str = "captures";
const MAX_TEXT_CHARS: usize = 100;
const MIN_BIRTH_YEAR: u16 = 1900;
/// A page of one shelter's people.
// ponytail: one page, no cursor; add pagination if a shelter ever passes a thousand people.
const LIST_LIMIT: u32 = 1000;

pub const COLLECTION: &str = "people";

/// Never goes down (spec 4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Verified,
    Assisted,
    NoBiometrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildStatus {
    WithGuardian,
    Separated,
    Unaccompanied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardianLink {
    Declared,
    DocumentChecked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guardian {
    pub person_id: String,
    pub link: GuardianLink,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_by: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub checked_at: Option<FirestoreInstant>,
}

/// Only these four booleans exist; anything else is a `422` (criterion 38).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Needs {
    #[serde(default)]
    pub continuous_medication: bool,
    #[serde(default)]
    pub dietary_restriction: bool,
    #[serde(default)]
    pub reduced_mobility: bool,
    #[serde(default)]
    pub health_follow_up: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disability {
    Mobility,
    Visual,
    Hearing,
    Intellectual,
    Psychosocial,
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vulnerabilities {
    #[serde(default)]
    pub pregnant: bool,
    #[serde(default)]
    pub lactating: bool,
    #[serde(default)]
    pub disability: Vec<Disability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indigenous_people: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrant_language: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    CheckinElsewhere,
    Exit,
    ExceptionalExit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitDestination {
    OwnHome,
    RelativesOrFriends,
    RentalAid,
    OtherShelter,
    Other,
    Unknown,
}

/// One passage through a shelter (spec 7); open while `left_at` is empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stay {
    pub shelter_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub entered_at: FirestoreInstant,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub left_at: Option<FirestoreInstant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_reason: Option<ExitReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_destination: Option<ExitDestination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_municipality: Option<String>,
    pub registered_by: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Checkin,
    Rescue,
}

/// `people/{person_id}`: the HMAC of the token, or the client's UUID while `no_biometrics`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    pub level: Level,
    #[serde(default)]
    pub biometrics_declined: bool,
    #[serde(default)]
    pub former_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub social_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wristband: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub birth_year: Option<u16>,
    #[serde(default)]
    pub is_child: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_status: Option<ChildStatus>,
    #[serde(default)]
    pub guardians: Vec<Guardian>,
    #[serde(default)]
    pub needs: Needs,
    #[serde(default)]
    pub vulnerabilities: Vulnerabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub household_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_shelter_id: Option<String>,
    #[serde(default)]
    pub stays: Vec<Stay>,
    #[serde(default)]
    pub event_ids: Vec<String>,
    #[serde(default)]
    pub rescue_ids: Vec<String>,
    /// AES-256-GCM, nonce first (spec 4.6); stored as a Firestore bytes value.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::thumbnail::optional_bytes"
    )]
    pub thumbnail_encrypted: Option<Vec<u8>>,
    pub origin: Origin,
    pub legal_basis: LegalBasis,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub updated_at: FirestoreInstant,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub client_edited_at: Option<FirestoreInstant>,
}

// ---- levels, stays, data ----

impl Level {
    fn rank(self) -> u8 {
        match self {
            Level::NoBiometrics => 0,
            Level::Assisted => 1,
            Level::Verified => 2,
        }
    }

    /// The level never goes down (spec 4.1).
    pub fn raised_to(self, other: Level) -> Level {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

pub fn person_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "person_not_found")
}

fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_person");
    error.detail = json!({ "field": field });
    error
}

/// The `data` of a check-in or of a `PATCH` (spec 3.4). Unknown fields are refused.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonData {
    name: Option<String>,
    social_name: Option<String>,
    wristband: Option<String>,
    birth_year: Option<u16>,
    is_child: Option<bool>,
    child_status: Option<ChildStatus>,
    needs: Option<Needs>,
    vulnerabilities: Option<Vulnerabilities>,
}

fn text(value: Option<String>, field: &str) -> Result<Option<String>, ApiError> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim();
    if value.chars().count() > MAX_TEXT_CHARS {
        return Err(invalid(field));
    }
    Ok((!value.is_empty()).then(|| value.to_string()))
}

fn current_year() -> u16 {
    u16::try_from(store::now().as_second() / 31_556_952 + 1970).unwrap_or(u16::MAX)
}

impl PersonData {
    /// Parses the `data` object; free text in `needs` or `vulnerabilities` is a `422` (criterion 38).
    fn parse(value: Value) -> Result<Self, ApiError> {
        let value = if value.is_null() { json!({}) } else { value };
        let data: PersonData = serde_json::from_value(value).map_err(|err| {
            let mut error = invalid("data");
            error.detail = json!({ "field": "data", "message": err.to_string() });
            error
        })?;
        data.validated()
    }

    fn validated(mut self) -> Result<Self, ApiError> {
        self.name = text(self.name, "name")?;
        self.social_name = text(self.social_name, "social_name")?;
        self.wristband = text(self.wristband, "wristband")?;
        if let Some(year) = self.birth_year {
            if !(MIN_BIRTH_YEAR..=current_year()).contains(&year) {
                return Err(invalid("birth_year"));
            }
        }
        if self.child_status.is_some() {
            if self.is_child == Some(false) {
                return Err(invalid("child_status"));
            }
            self.is_child = Some(true);
        }
        if let Some(v) = &mut self.vulnerabilities {
            v.indigenous_people = text(v.indigenous_people.take(), "indigenous_people")?;
            v.migrant_language = text(v.migrant_language.take(), "migrant_language")?;
        }
        Ok(self)
    }

    fn identifies(&self) -> bool {
        self.name.is_some() || self.wristband.is_some()
    }

    /// Copies what was sent onto the person; the thumbnail goes in the same write that gives
    /// the person a name or a wristband (spec 4.6, criterion 39).
    fn apply(&self, person: &mut PersonDoc) {
        if self.name.is_some() {
            person.name = self.name.clone();
        }
        if self.social_name.is_some() {
            person.social_name = self.social_name.clone();
        }
        if self.wristband.is_some() {
            person.wristband = self.wristband.clone();
        }
        if self.birth_year.is_some() {
            person.birth_year = self.birth_year;
        }
        if let Some(is_child) = self.is_child {
            person.is_child = is_child;
        }
        if self.child_status.is_some() {
            person.child_status = self.child_status;
        }
        if let Some(needs) = &self.needs {
            person.needs = needs.clone();
        }
        if let Some(vulnerabilities) = &self.vulnerabilities {
            person.vulnerabilities = vulnerabilities.clone();
        }
        drop_thumbnail_if_identified(person);
    }
}

pub fn drop_thumbnail_if_identified(person: &mut PersonDoc) {
    if person.name.is_some() || person.wristband.is_some() {
        person.thumbnail_encrypted = None;
    }
}

fn open_stay_index(stays: &[Stay]) -> Option<usize> {
    stays.iter().rposition(|s| s.left_at.is_none())
}

/// Opens a stay in `shelter_id` (spec 9.4). An open stay elsewhere is closed with
/// `checkin_elsewhere`; if that one is more recent than `entered_at` (a check-in that was
/// offline), the new stay is recorded already closed and the current shelter does not change.
/// Returns whether the stays changed: a second check-in in the shelter the person is already
/// in changes nothing.
pub fn enter_shelter(
    person: &mut PersonDoc,
    shelter_id: &str,
    event_id: Option<&str>,
    entered_at: FirestoreInstant,
    registered_by: &str,
    now: FirestoreInstant,
) -> bool {
    let entered_at = entered_at.min(now);
    let mut stay = Stay {
        shelter_id: shelter_id.into(),
        event_id: event_id.map(str::to_string),
        entered_at,
        left_at: None,
        exit_reason: None,
        exit_destination: None,
        destination_municipality: None,
        registered_by: registered_by.into(),
    };
    if let Some(event_id) = event_id {
        if !person.event_ids.iter().any(|e| e == event_id) {
            person.event_ids.push(event_id.into());
        }
    }
    match open_stay_index(&person.stays) {
        Some(i) if person.stays[i].shelter_id == shelter_id => return false,
        Some(i) if person.stays[i].entered_at > entered_at => {
            stay.left_at = Some(person.stays[i].entered_at);
            stay.exit_reason = Some(ExitReason::CheckinElsewhere);
            person.stays.push(stay);
            return true;
        }
        Some(i) => {
            person.stays[i].left_at = Some(now);
            person.stays[i].exit_reason = Some(ExitReason::CheckinElsewhere);
        }
        None => {}
    }
    person.stays.push(stay);
    person.current_shelter_id = Some(shelter_id.into());
    true
}

// ---- history visibility (spec 4.11) ----

/// What a volunteer of `shelter_id` sees of the stays: the shelter's own always; the stays in
/// other shelters only for the admin of a confirmed shelter, who gets shelter and dates;
/// everyone else in a confirmed shelter only learns whether there are any; an unconfirmed
/// shelter sees nothing of the others.
pub fn visible_history(stays: &[Stay], shelter_id: &str, admin: bool, confirmed: bool) -> Value {
    let (own, other): (Vec<&Stay>, Vec<&Stay>) =
        stays.iter().partition(|s| s.shelter_id == shelter_id);
    let mut shown: Vec<Value> = own.iter().map(|s| stay_view(s, true)).collect();
    if confirmed && admin {
        shown.extend(other.iter().map(|s| stay_view(s, false)));
    }
    json!({ "stays": shown, "has_other_stays": confirmed && !other.is_empty() })
}

fn stay_view(stay: &Stay, own: bool) -> Value {
    let mut view = json!({
        "shelter_id": stay.shelter_id,
        "entered_at": stay.entered_at.to_string(),
        "left_at": stay.left_at.map(|t| t.to_string()),
        "exit_reason": stay.exit_reason,
    });
    if own {
        view["exit_destination"] = json!(stay.exit_destination);
        view["destination_municipality"] = json!(stay.destination_municipality);
    }
    view
}

fn history_for(person: &PersonDoc, viewer: &Volunteer, shelter_id: &str) -> Value {
    match viewer
        .memberships
        .iter()
        .find(|m| m.shelter_id == shelter_id)
    {
        Some(m) => visible_history(
            &person.stays,
            shelter_id,
            m.role == ShelterRole::Admin,
            m.confirmation == Confirmation::Confirmed,
        ),
        None => json!({ "stays": [], "has_other_stays": false }),
    }
}

/// The shelter through which `viewer` is allowed to see the person: the person's current one,
/// else the latest shelter of a stay the viewer belongs to (a registration of their own).
fn viewer_shelter(person: &PersonDoc, viewer: &Volunteer) -> Option<String> {
    let current = person
        .current_shelter_id
        .iter()
        .find(|s| viewer.role_in(s).is_some());
    let past = person
        .stays
        .iter()
        .rev()
        .map(|s| &s.shelter_id)
        .find(|s| viewer.role_in(s).is_some());
    current.or(past).cloned()
}

pub fn person_view(id: &str, person: &PersonDoc, history: Value) -> Value {
    json!({
        "id": id,
        "level": person.level,
        "biometrics_declined": person.biometrics_declined,
        "former_ids": person.former_ids,
        "name": person.name,
        "social_name": person.social_name,
        "wristband": person.wristband,
        "birth_year": person.birth_year,
        "is_child": person.is_child,
        "child_status": person.child_status,
        "guardians": person.guardians.iter().map(|g| json!({
            "person_id": g.person_id,
            "link": g.link,
            "checked_by": g.checked_by,
            "checked_at": g.checked_at.map(|t| t.to_string()),
        })).collect::<Vec<_>>(),
        "needs": person.needs,
        "vulnerabilities": person.vulnerabilities,
        "household_id": person.household_id,
        "current_shelter_id": person.current_shelter_id,
        "history": history,
        "has_thumbnail": person.thumbnail_encrypted.is_some(),
        "origin": person.origin,
        "created_at": person.created_at.to_string(),
        "updated_at": person.updated_at.to_string(),
        "client_edited_at": person.client_edited_at.map(|t| t.to_string()),
    })
}

fn new_person(id: &str, level: Level, basis: LegalBasis, now: FirestoreInstant) -> PersonDoc {
    PersonDoc {
        id: Some(id.into()),
        level,
        biometrics_declined: false,
        former_ids: vec![],
        name: None,
        social_name: None,
        wristband: None,
        birth_year: None,
        is_child: false,
        child_status: None,
        guardians: vec![],
        needs: Needs::default(),
        vulnerabilities: Vulnerabilities::default(),
        household_id: None,
        current_shelter_id: None,
        stays: vec![],
        event_ids: vec![],
        rescue_ids: vec![],
        thumbnail_encrypted: None,
        origin: Origin::Checkin,
        legal_basis: basis,
        created_at: now,
        updated_at: now,
        client_edited_at: None,
    }
}

// ---- request plumbing ----

fn body<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ApiError> {
    serde_json::from_value(value).map_err(|err| {
        let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
        error.detail = json!({ "message": err.to_string() });
        error
    })
}

async fn load_shelter(db: &FirestoreDb, id: &str) -> Result<ShelterDoc, ApiError> {
    store::get(db, shelters::COLLECTION, id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found"))
}

fn require_member(volunteer: &Volunteer, shelter_id: &str) -> Result<(), ApiError> {
    volunteer
        .role_in(shelter_id)
        .map(|_| ())
        .ok_or_else(forbidden)
}

fn photo_bytes(base64: &str) -> Result<Vec<u8>, ApiError> {
    STANDARD
        .decode(base64.trim())
        .map_err(|_| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_image"))
}

// ---- POST /v1/shelters/{id}/checkins ----

#[derive(Debug, Deserialize)]
struct CheckinRequest {
    id: String,
    capture_id: Option<String>,
    photo_base64: Option<String>,
    #[serde(default)]
    no_biometrics: bool,
    #[serde(default)]
    biometrics_declined: bool,
    #[serde(default)]
    data: Value,
    thumbnail_base64: Option<String>,
    legal_basis: LegalBasis,
    client_created_at: String,
}

#[derive(Clone)]
struct Checkin {
    person_id: String,
    level: Level,
    declined: bool,
    data: PersonData,
    thumbnail: Option<Vec<u8>>,
    capture_id: Option<String>,
    shelter_id: String,
    event_id: Option<String>,
    legal_basis: LegalBasis,
    entered_at: FirestoreInstant,
    volunteer_id: String,
    device: Option<String>,
}

/// One transaction on `people/{person_id}`: consume the capture, create the person or raise
/// them, move the stay, write the audit entry. Returns the person and whether it is new.
async fn checkin_in(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    c: &Checkin,
) -> Result<(PersonDoc, bool), ApiError> {
    let existing: Option<PersonDoc> = store::get(db, COLLECTION, &c.person_id).await?;
    if let Some(capture_id) = &c.capture_id {
        let capture: CaptureDoc = store::get(db, CAPTURES, capture_id)
            .await?
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "capture_not_found"))?;
        if capture.consumed {
            return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
        }
        tx_replace(
            db,
            tx,
            CAPTURES,
            capture_id,
            &auth::consumed(capture, &c.person_id),
        )?;
    }
    let now = store::now();
    let is_new = existing.is_none();
    let mut person =
        existing.unwrap_or_else(|| new_person(&c.person_id, c.level, c.legal_basis, now));
    person.level = person.level.raised_to(c.level);
    person.biometrics_declined |= c.declined;
    c.data.apply(&mut person);
    if let Some(sealed) = &c.thumbnail {
        if person.name.is_none() && person.wristband.is_none() {
            person.thumbnail_encrypted = Some(sealed.clone());
        }
    }
    enter_shelter(
        &mut person,
        &c.shelter_id,
        c.event_id.as_deref(),
        c.entered_at,
        &c.volunteer_id,
        now,
    );
    person.updated_at = now;
    // child_separated hook (C): raise the alert here when `child_status` is separated or unaccompanied.
    tx_replace(db, tx, COLLECTION, &c.person_id, &person)?;
    let mut entry = AuditEntry::new("person.checkin", "person", &c.person_id)
        .by(&c.volunteer_id)
        .in_shelter(&c.shelter_id)
        .basis(c.legal_basis);
    if let Some(device) = &c.device {
        entry = entry.on_device(device);
    }
    audit::add(db, tx, &entry)?;
    Ok((person, is_new))
}

pub async fn post_checkin(
    State(state): State<AppState>,
    volunteer: Volunteer,
    headers: HeaderMap,
    Path(shelter_id): Path<String>,
    Json(request): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let request: CheckinRequest = body(request)?;
    require_member(&volunteer, &shelter_id)?;
    let shelter = load_shelter(&state.db, &shelter_id).await?;
    let data = PersonData::parse(request.data)?;
    let entered_at: FirestoreInstant = request
        .client_created_at
        .parse()
        .map_err(|_| invalid("client_created_at"))?;
    let thumbnail = match (&request.thumbnail_base64, data.identifies()) {
        (Some(upload), false) => {
            let jpeg = thumbnail::decode_upload(upload)?;
            Some(thumbnail::encrypt(state.thumbnail_key(), &jpeg)?)
        }
        _ => None,
    };

    let device = auth::device_id(&headers);
    let (person_id, level, capture_id) = match (
        &request.capture_id,
        &request.photo_base64,
        request.no_biometrics,
    ) {
        (Some(capture_id), None, false) => {
            let device = device.clone()?;
            let mut capture =
                auth::usable_capture(&state, &device, capture_id, Purpose::Checkin).await?;
            let person_id = auth::resolve_person(&state, capture_id, &mut capture).await?;
            (person_id, Level::Verified, Some(capture_id.clone()))
        }
        (None, Some(photo), false) => {
            let token = state.unico.token_from_image(&photo_bytes(photo)?).await?;
            (state.person_id_for_token(&token), Level::Assisted, None)
        }
        (None, None, true) => {
            uuid::Uuid::parse_str(&request.id).map_err(|_| invalid("id"))?;
            (request.id.clone(), Level::NoBiometrics, None)
        }
        _ => return Err(invalid("biometrics")),
    };
    let checkin = Checkin {
        person_id: person_id.clone(),
        level,
        declined: request.biometrics_declined,
        data,
        thumbnail,
        capture_id,
        shelter_id: shelter_id.clone(),
        event_id: shelter.event_id.clone(),
        legal_basis: request.legal_basis,
        entered_at,
        volunteer_id: volunteer.id.clone(),
        device: device.ok(),
    };
    let (person, is_new) = store::transact(&state.db, |db, tx| {
        let checkin = checkin.clone();
        Box::pin(async move { checkin_in(&db, tx, &checkin).await })
    })
    .await?;
    crate::events::touch(&state, shelter.event_id.as_deref()).await;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "person_id": person_id,
            "level": person.level,
            "is_new": is_new,
            "history": history_for(&person, &volunteer, &shelter_id),
        })),
    ))
}

// ---- GET /v1/shelters/{id}/people[/needs-selfie] ----

async fn people_of(db: &FirestoreDb, shelter_id: &str) -> Result<Vec<PersonDoc>, ApiError> {
    db.fluent()
        .select()
        .from(COLLECTION)
        .filter(|q| q.for_all([q.field(path!(PersonDoc::current_shelter_id)).eq(shelter_id)]))
        .order_by([(
            path!(PersonDoc::updated_at),
            firestore::FirestoreQueryDirection::Descending,
        )])
        .limit(LIST_LIMIT)
        .obj()
        .query()
        .await
        .map_err(fs_error)
}

fn list_view(people: &[PersonDoc], volunteer: &Volunteer, shelter_id: &str) -> Json<Value> {
    let list: Vec<Value> = people
        .iter()
        .map(|p| {
            person_view(
                p.id.as_deref().unwrap_or_default(),
                p,
                history_for(p, volunteer, shelter_id),
            )
        })
        .collect();
    Json(json!({ "people": list }))
}

pub async fn list_people(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_member(&volunteer, &shelter_id)?;
    let people = people_of(&state.db, &shelter_id).await?;
    Ok(list_view(&people, &volunteer, &shelter_id))
}

/// `assisted` and `no_biometrics` who did not refuse the selfie (spec 8.5).
pub async fn needs_selfie(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_member(&volunteer, &shelter_id)?;
    let mut people = people_of(&state.db, &shelter_id).await?;
    people.retain(|p| match p.level {
        Level::Verified => false,
        Level::Assisted => true,
        Level::NoBiometrics => !p.biometrics_declined,
    });
    Ok(list_view(&people, &volunteer, &shelter_id))
}

// ---- GET, PATCH /v1/people/{id} ----

/// A person someone is not allowed to see is "not found": the route does not confirm who exists.
async fn visible_person(
    state: &AppState,
    volunteer: &Volunteer,
    person_id: &str,
) -> Result<(PersonDoc, String), ApiError> {
    let person: PersonDoc = store::get(&state.db, COLLECTION, person_id)
        .await?
        .ok_or_else(person_not_found)?;
    let shelter = viewer_shelter(&person, volunteer).ok_or_else(person_not_found)?;
    Ok((person, shelter))
}

pub async fn get_person(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(person_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (person, shelter_id) = visible_person(&state, &volunteer, &person_id).await?;
    let history = history_for(&person, &volunteer, &shelter_id);
    Ok(Json(
        json!({ "person": person_view(&person_id, &person, history) }),
    ))
}

#[derive(Debug, Deserialize)]
struct PatchRequest {
    #[serde(default)]
    data: Value,
    client_edited_at: String,
}

fn can_change_child_status(volunteer: &Volunteer) -> bool {
    volunteer.has_global(GlobalRole::Protection) || volunteer.has_global(GlobalRole::CivilDefense)
}

pub async fn patch_person(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(person_id): Path<String>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let request: PatchRequest = body(request)?;
    let data = PersonData::parse(request.data)?;
    let edited_at: FirestoreInstant = request
        .client_edited_at
        .parse()
        .map_err(|_| invalid("client_edited_at"))?;
    let (current, _) = visible_person(&state, &volunteer, &person_id).await?;
    if data.child_status.is_some()
        && data.child_status != current.child_status
        && !can_change_child_status(&volunteer)
    {
        return Err(ApiError::protection_role_required());
    }
    let person = store::transact(&state.db, |db, tx| {
        let (person_id, data) = (person_id.clone(), data.clone());
        Box::pin(async move {
            let mut person: PersonDoc = store::get(&db, COLLECTION, &person_id)
                .await?
                .ok_or_else(person_not_found)?;
            // The last edit wins; an older one is ignored and the current state returned (spec 9.4).
            if person.client_edited_at.is_some_and(|last| last > edited_at) {
                return Ok(person);
            }
            data.apply(&mut person);
            person.client_edited_at = Some(edited_at);
            person.updated_at = store::now();
            // child_separated hook (C): raise the alert when `child_status` changed to separated or unaccompanied.
            tx_replace(&db, tx, COLLECTION, &person_id, &person)?;
            Ok(person)
        })
    })
    .await?;
    let shelter_id = viewer_shelter(&person, &volunteer).ok_or_else(person_not_found)?;
    let history = history_for(&person, &volunteer, &shelter_id);
    Ok(Json(
        json!({ "person": person_view(&person_id, &person, history) }),
    ))
}

// ---- PUT /v1/people/{id}/biometrics (spec 4.12) ----

#[derive(Debug, Deserialize)]
struct BiometricsRequest {
    capture_id: Option<String>,
    photo_base64: Option<String>,
}

/// Documents of later phases that point at a person; both keep the person in `person_id`.
const PERSON_REFERENCES: [&str; 2] = ["deliveries", "rescues"];

#[derive(Debug, Serialize, Deserialize)]
struct PersonRef {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    id: Option<String>,
    person_id: String,
}

/// Points every `deliveries.person_id` and `rescues.person_id` that holds `old` at `new`.
/// The collections come to exist in phases 4 and 5; until then the queries find nothing.
pub async fn repoint_person_refs(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    old: &str,
    new: &str,
) -> Result<(), ApiError> {
    for collection in PERSON_REFERENCES {
        let found: Vec<PersonRef> = db
            .fluent()
            .select()
            .from(collection)
            .filter(|q| q.for_all([q.field("person_id").eq(old)]))
            .obj()
            .query()
            .await
            .map_err(fs_error)?;
        for reference in found {
            let Some(id) = reference.id else { continue };
            db.fluent()
                .update()
                .fields(["person_id"])
                .in_col(collection)
                .document_id(&id)
                .object(&PersonRef {
                    id: None,
                    person_id: new.into(),
                })
                .add_to_transaction(tx)
                .map_err(fs_error)?;
        }
    }
    Ok(())
}

/// `old` replaced by `new` in the guardians list; returns whether anything changed.
fn repoint_guardians(guardians: &mut [Guardian], old: &str, new: &str) -> bool {
    let mut changed = false;
    for guardian in guardians.iter_mut().filter(|g| g.person_id == old) {
        guardian.person_id = new.into();
        changed = true;
    }
    changed
}

/// `old` replaced by `new` in the members and the head; `new` is not repeated.
fn repoint_household(household: &mut HouseholdDoc, old: &str, new: &str) {
    let mut members = Vec::new();
    for member in household.member_ids.drain(..) {
        let member = if member == old {
            new.to_string()
        } else {
            member
        };
        if !members.contains(&member) {
            members.push(member);
        }
    }
    household.member_ids = members;
    if household.head_id.as_deref() == Some(old) {
        household.head_id = Some(new.into());
    }
}

fn without_member(household: &mut HouseholdDoc, id: &str) {
    household.member_ids.retain(|m| m != id);
    if household.head_id.as_deref() == Some(id) {
        household.head_id = None;
    }
}

fn union<T: PartialEq>(into: &mut Vec<T>, from: Vec<T>) {
    for item in from {
        if !into.contains(&item) {
            into.push(item);
        }
    }
}

/// Folds the record `old` (a UUID, `no_biometrics`) into the person `new` that already existed
/// under the token: stays are joined and sorted, only the most recent open one stays open, and
/// what `new` lacks is filled from `old`. The household is decided by the caller.
fn merge_into(mut new: PersonDoc, old: PersonDoc, old_id: &str, level: Level) -> PersonDoc {
    new.level = new.level.raised_to(level);
    union(&mut new.former_ids, old.former_ids);
    union(&mut new.former_ids, vec![old_id.to_string()]);
    new.name = new.name.or(old.name);
    new.social_name = new.social_name.or(old.social_name);
    new.wristband = new.wristband.or(old.wristband);
    new.birth_year = new.birth_year.or(old.birth_year);
    new.is_child |= old.is_child;
    new.child_status = new.child_status.or(old.child_status);
    if new.needs == Needs::default() {
        new.needs = old.needs;
    }
    if new.vulnerabilities == Vulnerabilities::default() {
        new.vulnerabilities = old.vulnerabilities;
    }
    let mut guardians = old.guardians;
    for guardian in new.guardians.drain(..) {
        guardians.retain(|g| g.person_id != guardian.person_id);
        guardians.push(guardian);
    }
    new.guardians = guardians;
    union(&mut new.event_ids, old.event_ids);
    union(&mut new.rescue_ids, old.rescue_ids);
    new.stays.extend(old.stays);
    new.stays.sort_by_key(|s| s.entered_at);
    let last_open = open_stay_index(&new.stays);
    for (i, stay) in new.stays.iter_mut().enumerate() {
        if stay.left_at.is_none() && Some(i) != last_open {
            stay.left_at = Some(store::now());
            stay.exit_reason = Some(ExitReason::CheckinElsewhere);
        }
    }
    new.current_shelter_id = last_open.map(|i| new.stays[i].shelter_id.clone());
    new.thumbnail_encrypted = new.thumbnail_encrypted.or(old.thumbnail_encrypted);
    drop_thumbnail_if_identified(&mut new);
    new.biometrics_declined = false;
    new.created_at = new.created_at.min(old.created_at);
    new
}

#[derive(Clone)]
struct Migration {
    old_id: String,
    new_id: String,
    level: Level,
    capture_id: Option<String>,
    volunteer_id: String,
    device: Option<String>,
    shelter_id: String,
}

/// One transaction (spec 4.12): the record under the token is created or merged, every
/// reference moves to it, the old record goes away, and the capture is consumed.
async fn migrate_in(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    m: &Migration,
) -> Result<PersonDoc, ApiError> {
    let old: PersonDoc = store::get(db, COLLECTION, &m.old_id)
        .await?
        .ok_or_else(person_not_found)?;
    let existing: Option<PersonDoc> = store::get(db, COLLECTION, &m.new_id).await?;
    let old_household: Option<HouseholdDoc> = match &old.household_id {
        Some(id) => store::get(db, households::COLLECTION, id).await?,
        None => None,
    };
    let new_household: Option<HouseholdDoc> =
        match existing.as_ref().and_then(|p| p.household_id.as_ref()) {
            Some(id) if Some(id) != old.household_id.as_ref() => {
                store::get(db, households::COLLECTION, id).await?
            }
            _ => None,
        };
    let capture = match &m.capture_id {
        Some(capture_id) => {
            let capture: CaptureDoc = store::get(db, CAPTURES, capture_id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "capture_not_found"))?;
            if capture.consumed {
                return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
            }
            Some((capture_id, capture))
        }
        None => None,
    };
    let shelter: Option<ShelterDoc> = store::get(db, shelters::COLLECTION, &m.shelter_id).await?;
    let now = store::now();

    // The household of the old record moves to the new id; when the token's record already has
    // another household, that one stays and the admins hear about it.
    let mut conflict = None;
    let mut person = match existing {
        None => {
            let mut person = old.clone();
            person.id = Some(m.new_id.clone());
            person.level = m.level;
            person.biometrics_declined = false;
            union(&mut person.former_ids, vec![m.old_id.clone()]);
            person
        }
        Some(existing) => {
            let same_household = existing.household_id == old.household_id;
            let mut merged = merge_into(existing.clone(), old.clone(), &m.old_id, m.level);
            if existing.household_id.is_none() {
                merged.household_id = old.household_id.clone();
            } else if !same_household && old.household_id.is_some() {
                conflict = Some((old.household_id.clone(), existing.household_id.clone()));
            }
            merged
        }
    };
    person.updated_at = now;
    person.id = None;

    if let Some(mut household) = old_household {
        if conflict.is_some() {
            without_member(&mut household, &m.old_id);
        } else {
            repoint_household(&mut household, &m.old_id, &m.new_id);
        }
        household.updated_at = now;
        let id = old.household_id.as_deref().unwrap_or_default();
        tx_replace(db, tx, households::COLLECTION, id, &household)?;
    }
    if let (Some(mut household), Some(id)) = (new_household, person.household_id.as_ref()) {
        // Both records were members of different households: the token's one keeps its place.
        if !household.member_ids.contains(&m.new_id) {
            household.member_ids.push(m.new_id.clone());
            household.updated_at = now;
            tx_replace(db, tx, households::COLLECTION, id, &household)?;
        }
    }
    // A child whose guardian was the old record, among the people of the same household.
    if let Some(household_id) = &old.household_id {
        if let Some(household) =
            store::get::<HouseholdDoc>(db, households::COLLECTION, household_id).await?
        {
            for member in household
                .member_ids
                .iter()
                .filter(|id| **id != m.old_id && **id != m.new_id)
            {
                if let Some(mut other) = store::get::<PersonDoc>(db, COLLECTION, member).await? {
                    if repoint_guardians(&mut other.guardians, &m.old_id, &m.new_id) {
                        tx_replace(db, tx, COLLECTION, member, &other)?;
                    }
                }
            }
        }
    }
    repoint_person_refs(db, tx, &m.old_id, &m.new_id).await?;

    if let Some((capture_id, capture)) = capture {
        tx_replace(
            db,
            tx,
            CAPTURES,
            capture_id,
            &auth::consumed(capture, &m.new_id),
        )?;
    }
    tx_delete(db, tx, COLLECTION, &m.old_id)?;
    tx_replace(db, tx, COLLECTION, &m.new_id, &person)?;
    if let (Some((old_household_id, new_household_id)), Some(shelter)) = (conflict, &shelter) {
        let mut alert = AlertDoc::new(
            AlertKind::HouseholdConflict,
            "person",
            &m.new_id,
            json!({ "old_household_id": old_household_id, "household_id": new_household_id }),
        )
        .for_shelters(vec![m.shelter_id.clone()]);
        if let Some(event_id) = &shelter.event_id {
            alert = alert.in_event(event_id);
        }
        alerts::add(db, tx, &alert)?;
    }
    let mut entry = AuditEntry::new("person.biometrics", "person", &m.new_id)
        .by(&m.volunteer_id)
        .in_shelter(&m.shelter_id)
        .basis(person.legal_basis);
    if let Some(device) = &m.device {
        entry = entry.on_device(device);
    }
    audit::add(db, tx, &entry)?;
    Ok(person)
}

/// The record already lives under the token: only the level can rise.
async fn raise_in(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    m: &Migration,
) -> Result<PersonDoc, ApiError> {
    let mut person: PersonDoc = store::get(db, COLLECTION, &m.new_id)
        .await?
        .ok_or_else(person_not_found)?;
    if let Some(capture_id) = &m.capture_id {
        let capture: CaptureDoc = store::get(db, CAPTURES, capture_id)
            .await?
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "capture_not_found"))?;
        if capture.consumed {
            return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
        }
        tx_replace(
            db,
            tx,
            CAPTURES,
            capture_id,
            &auth::consumed(capture, &m.new_id),
        )?;
    }
    person.level = person.level.raised_to(m.level);
    person.updated_at = store::now();
    tx_replace(db, tx, COLLECTION, &m.new_id, &person)?;
    let entry = AuditEntry::new("person.biometrics", "person", &m.new_id)
        .by(&m.volunteer_id)
        .in_shelter(&m.shelter_id)
        .basis(person.legal_basis);
    audit::add(db, tx, &entry)?;
    Ok(person)
}

pub async fn put_biometrics(
    State(state): State<AppState>,
    volunteer: Volunteer,
    headers: HeaderMap,
    Path(person_id): Path<String>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let request: BiometricsRequest = body(request)?;
    let (person, shelter_id) = visible_person(&state, &volunteer, &person_id).await?;
    let device = auth::device_id(&headers);
    let (new_id, level, capture_id) = match (&request.capture_id, &request.photo_base64) {
        (Some(capture_id), None) => {
            let device = device.clone()?;
            let mut capture =
                auth::usable_capture(&state, &device, capture_id, Purpose::Biometrics).await?;
            let new_id = auth::resolve_person(&state, capture_id, &mut capture).await?;
            (new_id, Level::Verified, Some(capture_id.clone()))
        }
        (None, Some(photo)) => {
            let token = state.unico.token_from_image(&photo_bytes(photo)?).await?;
            (state.person_id_for_token(&token), Level::Assisted, None)
        }
        _ => return Err(invalid("biometrics")),
    };
    // A record that already has a token can only be raised by its own token.
    if person.level != Level::NoBiometrics && new_id != person_id {
        return Err(ApiError::new(StatusCode::CONFLICT, "person_mismatch"));
    }
    let migration = Migration {
        old_id: person_id,
        new_id: new_id.clone(),
        level,
        capture_id,
        volunteer_id: volunteer.id.clone(),
        device: device.ok(),
        shelter_id,
    };
    let person = store::transact(&state.db, |db, tx| {
        let m = migration.clone();
        Box::pin(async move {
            if m.old_id == m.new_id {
                raise_in(&db, tx, &m).await
            } else {
                migrate_in(&db, tx, &m).await
            }
        })
    })
    .await?;
    Ok(Json(json!({ "person_id": new_id, "level": person.level })))
}
