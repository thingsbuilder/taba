//! People (spec 3.4, 4.1, 4.11, 4.12, 7, 8.5). Check-in in three
//! ways, edits, late biometrics and the history rules.

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
use crate::children;
use crate::error::ApiError;
use crate::households::{self, HouseholdDoc};
use crate::idempotency;
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
    /// `guardians[].person_id` as a flat list, so a query can find every child of a guardian (the
    /// late biometrics move them to the new id). Kept in step with `guardians`: see `sync_guardian_ids`.
    #[serde(default)]
    pub guardian_ids: Vec<String>,
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

impl PersonData {
    /// Nothing but `child_status` is set.
    fn only_child_status(&self) -> bool {
        self.name.is_none()
            && self.social_name.is_none()
            && self.wristband.is_none()
            && self.birth_year.is_none()
            // `child_status` makes `is_child` true on its own (see `validated`)
            && self.is_child != Some(false)
            && self.needs.is_none()
            && self.vulnerabilities.is_none()
    }
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
        guardian_ids: vec![],
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

/// `X-Taken-Over-From`: the original author of a queue item an admin took over (spec 9.2).
fn taken_over_from(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-taken-over-from")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.len() <= 64)
        .map(str::to_string)
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
    shelter: ShelterDoc,
    event_id: Option<String>,
    legal_basis: LegalBasis,
    entered_at: FirestoreInstant,
    volunteer_id: String,
    device: Option<String>,
    taken_over_from: Option<String>,
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
    tx_replace(db, tx, COLLECTION, &c.person_id, &person)?;
    crate::matching::on_person_registered(db, tx, &person, &c.person_id, Some(&c.shelter)).await?;
    // A child who arrives separated or alone raises the alert for the protection queue (spec 3.4.1).
    if let (true, true, Some(status)) = (
        person.is_child,
        c.data.child_status.is_some(),
        person.child_status,
    ) {
        children::raise_child_separated(db, tx, &c.person_id, &c.shelter_id, &c.shelter, status)
            .await?;
    }
    let mut entry = AuditEntry::new("person.checkin", "person", &c.person_id)
        .by(&c.volunteer_id)
        .in_shelter(&c.shelter_id)
        .taken_over_from(c.taken_over_from.as_deref())
        .basis(c.legal_basis);
    if let Some(device) = &c.device {
        entry = entry.on_device(device);
    }
    audit::add(db, tx, &entry)?;
    Ok((person, is_new))
}

/// A capture made for another shelter or another person.
fn wrong_capture_target() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "capture_wrong_target")
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
    crate::events::ensure_open(&state.db, shelter.event_id.as_deref()).await?;
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
            // The capture was asked for one shelter and cannot check anyone in elsewhere.
            // Nor can another volunteer, even on the same phone, use someone else's capture.
            if capture.shelter_id.as_deref() != Some(shelter_id.as_str())
                || capture.volunteer_id.as_deref() != Some(volunteer.id.as_str())
            {
                return Err(wrong_capture_target());
            }
            let person_id = auth::resolve_person(&state, capture_id, &mut capture).await?;
            (person_id, Level::Verified, Some(capture_id.clone()))
        }
        (None, Some(photo), false) => {
            let route = format!("POST /v1/shelters/{shelter_id}/checkins");
            let id = idempotency::photo_person_id(&state, &headers, &volunteer.id, &route, async {
                let token = state.unico.token_from_image(&photo_bytes(photo)?).await?;
                Ok(state.person_id_for_token(&token))
            })
            .await?;
            (id, Level::Assisted, None)
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
        shelter: shelter.clone(),
        legal_basis: request.legal_basis,
        entered_at,
        volunteer_id: volunteer.id.clone(),
        device: device.ok(),
        taken_over_from: taken_over_from(&headers),
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
/// Whether a child is being attended: an open `child_separated` alert is about them (spec 4.11).
async fn child_alert_open(
    state: &AppState,
    person: &PersonDoc,
    person_id: &str,
) -> Result<bool, ApiError> {
    if !person.is_child {
        return Ok(false);
    }
    let alerts: Vec<AlertDoc> = state
        .db
        .fluent()
        .select()
        .from(alerts::COLLECTION)
        .filter(|q| {
            q.for_all([
                q.field(path!(AlertDoc::target_id)).eq(person_id),
                q.field(path!(AlertDoc::resolved)).eq(false),
            ])
        })
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    Ok(alerts.iter().any(|a| a.kind == AlertKind::ChildSeparated))
}

/// The person and the shelter through which the viewer sees them. Members of a shelter of the
/// person see them. With `attending`, protection and the Civil Defense also see a child while
/// an open `child_separated` alert is about them, whatever the shelter (spec 4.11); the shelter
/// is then the child's current one, and the viewer is not a member of it.
async fn visible_person(
    state: &AppState,
    volunteer: &Volunteer,
    person_id: &str,
    attending: bool,
) -> Result<(PersonDoc, String), ApiError> {
    let person: PersonDoc = store::get(&state.db, COLLECTION, person_id)
        .await?
        .ok_or_else(person_not_found)?;
    if let Some(shelter) = viewer_shelter(&person, volunteer) {
        return Ok((person, shelter));
    }
    if attending
        && can_change_child_status(volunteer)
        && child_alert_open(state, &person, person_id).await?
    {
        let shelter = person
            .current_shelter_id
            .clone()
            .or_else(|| person.stays.last().map(|s| s.shelter_id.clone()))
            .unwrap_or_default();
        return Ok((person, shelter));
    }
    Err(person_not_found())
}

pub async fn get_person(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(person_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (person, shelter_id) = visible_person(&state, &volunteer, &person_id, true).await?;
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
    headers: HeaderMap,
    Path(person_id): Path<String>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let request: PatchRequest = body(request)?;
    let data = PersonData::parse(request.data)?;
    let edited_at: FirestoreInstant = request
        .client_edited_at
        .parse()
        .map_err(|_| invalid("client_edited_at"))?;
    let (current, shelter_id) = visible_person(&state, &volunteer, &person_id, true).await?;
    crate::events::ensure_shelter_open(&state.db, &shelter_id).await?;
    // Someone who sees the child only through the alert changes the status and nothing else.
    if viewer_shelter(&current, &volunteer).is_none() && !data.only_child_status() {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden"));
    }
    if data.child_status.is_some()
        && data.child_status != current.child_status
        && !can_change_child_status(&volunteer)
    {
        return Err(ApiError::protection_role_required());
    }
    let entry = AuditEntry::new("person.update", "person", &person_id)
        .by(&volunteer.id)
        .taken_over_from(taken_over_from(&headers).as_deref())
        .basis(current.legal_basis);
    let person = store::transact(&state.db, |db, tx| {
        let (person_id, data, entry) = (person_id.clone(), data.clone(), entry.clone());
        Box::pin(async move {
            let mut person: PersonDoc = store::get(&db, COLLECTION, &person_id)
                .await?
                .ok_or_else(person_not_found)?;
            // The last edit wins; an older one is ignored and the current state returned (spec 9.4).
            if person.client_edited_at.is_some_and(|last| last > edited_at) {
                return Ok(person);
            }
            let before = person.child_status;
            data.apply(&mut person);
            person.client_edited_at = Some(edited_at);
            person.updated_at = store::now();
            tx_replace(&db, tx, COLLECTION, &person_id, &person)?;
            // A change to separated or unaccompanied raises the alert too (spec 3.4.1, item 4).
            if let (true, true, Some(status), Some(shelter_id)) = (
                person.is_child,
                person.child_status != before,
                person.child_status,
                person.current_shelter_id.as_deref(),
            ) {
                if let Some(shelter) =
                    store::get::<ShelterDoc>(&db, shelters::COLLECTION, shelter_id).await?
                {
                    children::raise_child_separated(
                        &db, tx, &person_id, shelter_id, &shelter, status,
                    )
                    .await?;
                }
            }
            audit::add(&db, tx, &entry)?;
            Ok(person)
        })
    })
    .await?;
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

impl PersonDoc {
    /// Makes `guardian_ids` match `guardians`; call it after any change to the guardians.
    pub fn sync_guardian_ids(&mut self) {
        self.guardian_ids = self.guardians.iter().map(|g| g.person_id.clone()).collect();
    }
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
    taken_over_from: Option<String>,
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
    // Every child who lists the old record as guardian, in any household or none (spec 4.12).
    let children: Vec<PersonDoc> = db
        .fluent()
        .select()
        .from(COLLECTION)
        .filter(|q| {
            q.for_all([q
                .field(path!(PersonDoc::guardian_ids))
                .array_contains(&m.old_id)])
        })
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    for mut child in children {
        let Some(id) = child.id.take() else { continue };
        if id == m.old_id || id == m.new_id {
            continue;
        }
        if repoint_guardians(&mut child.guardians, &m.old_id, &m.new_id) {
            child.sync_guardian_ids();
            tx_replace(db, tx, COLLECTION, &id, &child)?;
        }
    }
    repoint_person_refs(db, tx, &m.old_id, &m.new_id).await?;
    crate::rescues::repoint_person(db, tx, &m.old_id, &m.new_id).await?;

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
    crate::matching::on_person_registered(db, tx, &person, &m.new_id, shelter.as_ref()).await?;
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
        .taken_over_from(m.taken_over_from.as_deref())
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
        .taken_over_from(m.taken_over_from.as_deref())
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
    let (person, shelter_id) = visible_person(&state, &volunteer, &person_id, false).await?;
    crate::events::ensure_shelter_open(&state.db, &shelter_id).await?;
    let device = auth::device_id(&headers);
    let (new_id, level, capture_id) = match (&request.capture_id, &request.photo_base64) {
        (Some(capture_id), None) => {
            let device = device.clone()?;
            let mut capture =
                auth::usable_capture(&state, &device, capture_id, Purpose::Biometrics).await?;
            // ... and a biometrics capture was asked for one person.
            if capture.expected_person_id.as_deref() != Some(person_id.as_str())
                || capture.volunteer_id.as_deref() != Some(volunteer.id.as_str())
            {
                return Err(wrong_capture_target());
            }
            let new_id = auth::resolve_person(&state, capture_id, &mut capture).await?;
            (new_id, Level::Verified, Some(capture_id.clone()))
        }
        (None, Some(photo)) => {
            let route = format!("PUT /v1/people/{person_id}/biometrics");
            let id = idempotency::photo_person_id(&state, &headers, &volunteer.id, &route, async {
                let token = state.unico.token_from_image(&photo_bytes(photo)?).await?;
                Ok(state.person_id_for_token(&token))
            })
            .await?;
            (id, Level::Assisted, None)
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
        taken_over_from: taken_over_from(&headers),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::CaptureDoc;
    use crate::test_support::{Actor, TestApp, audit_actions, new_id, north_of_origin};

    const JPEG: [u8; 6] = [0xFF, 0xD8, 0xFF, 0xE0, 1, 2];

    fn at(text: &str) -> FirestoreInstant {
        text.parse().unwrap()
    }

    fn stay(shelter: &str, entered: &str) -> Stay {
        Stay {
            shelter_id: shelter.into(),
            event_id: None,
            entered_at: at(entered),
            left_at: None,
            exit_reason: None,
            exit_destination: None,
            destination_municipality: None,
            registered_by: "v".into(),
        }
    }

    fn person_in(shelter: &str) -> PersonDoc {
        let mut person = new_person("p", Level::Assisted, LegalBasis::Consent, store::now());
        enter_shelter(
            &mut person,
            shelter,
            None,
            at("2026-10-21T10:00:00Z"),
            "v",
            store::now(),
        );
        person
    }

    // ---- units ----

    #[test]
    fn the_level_never_goes_down() {
        for (from, to, expected) in [
            (Level::NoBiometrics, Level::Assisted, Level::Assisted),
            (Level::Assisted, Level::Verified, Level::Verified),
            (Level::Verified, Level::Assisted, Level::Verified),
            (Level::Verified, Level::NoBiometrics, Level::Verified),
            (Level::Assisted, Level::NoBiometrics, Level::Assisted),
        ] {
            assert_eq!(from.raised_to(to), expected, "{from:?} -> {to:?}");
        }
    }

    #[test]
    fn a_new_stay_closes_the_one_open_elsewhere() {
        let mut person = person_in("a");
        let now = store::now();
        assert!(enter_shelter(&mut person, "b", Some("ev"), now, "v", now));
        assert_eq!(person.current_shelter_id.as_deref(), Some("b"));
        assert_eq!(person.stays.len(), 2);
        assert_eq!(
            person.stays[0].exit_reason,
            Some(ExitReason::CheckinElsewhere)
        );
        assert!(person.stays[0].left_at.is_some());
        assert!(person.stays[1].left_at.is_none());
        assert_eq!(person.event_ids, vec!["ev".to_string()]);
    }

    #[test]
    fn checking_in_again_where_the_person_is_changes_nothing() {
        let mut person = person_in("a");
        assert!(!enter_shelter(
            &mut person,
            "a",
            None,
            store::now(),
            "v",
            store::now()
        ));
        assert_eq!(person.stays.len(), 1);
    }

    #[test]
    fn an_older_offline_check_in_is_recorded_closed_and_keeps_the_current_shelter() {
        let mut person = person_in("a");
        let now = store::now();
        enter_shelter(&mut person, "b", None, at("2026-10-21T09:00:00Z"), "v", now);
        assert_eq!(person.current_shelter_id.as_deref(), Some("a"));
        assert_eq!(person.stays.len(), 2);
        let older = &person.stays[1];
        assert_eq!(older.left_at, Some(at("2026-10-21T10:00:00Z")));
        assert_eq!(older.exit_reason, Some(ExitReason::CheckinElsewhere));
        assert_eq!(
            person.stays.iter().filter(|s| s.left_at.is_none()).count(),
            1
        );
    }

    #[test]
    fn history_follows_role_and_confirmation() {
        let stays = vec![
            stay("here", "2026-10-21T10:00:00Z"),
            stay("there", "2026-10-20T10:00:00Z"),
        ];
        let shelters = |v: &Value| -> Vec<String> {
            v["stays"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s["shelter_id"].as_str().unwrap().to_string())
                .collect()
        };
        let admin = visible_history(&stays, "here", true, true);
        assert_eq!(shelters(&admin), ["here", "there"]);
        assert_eq!(admin["has_other_stays"], true);
        let volunteer = visible_history(&stays, "here", false, true);
        assert_eq!(shelters(&volunteer), ["here"]);
        assert_eq!(volunteer["has_other_stays"], true);
        for admin in [true, false] {
            let unconfirmed = visible_history(&stays, "here", admin, false);
            assert_eq!(shelters(&unconfirmed), ["here"]);
            assert_eq!(unconfirmed["has_other_stays"], false);
        }
        let alone = visible_history(&stays[..1], "here", false, true);
        assert_eq!(alone["has_other_stays"], false);
    }

    #[test]
    fn data_accepts_only_the_four_needs_and_known_vulnerabilities() {
        assert!(PersonData::parse(json!({ "needs": { "reduced_mobility": true } })).is_ok());
        for bad in [
            json!({ "needs": { "diagnosis": "diabetes" } }),
            json!({ "needs": { "reduced_mobility": "needs a wheelchair" } }),
            json!({ "vulnerabilities": { "notes": "x" } }),
            json!({ "vulnerabilities": { "disability": ["flying"] } }),
            json!({ "religion": "x" }),
        ] {
            let error = PersonData::parse(bad.clone()).unwrap_err();
            assert_eq!(
                (error.status, error.code),
                (StatusCode::UNPROCESSABLE_ENTITY, "invalid_person"),
                "{bad}"
            );
        }
    }

    #[test]
    fn data_is_trimmed_and_bounded() {
        let data = PersonData::parse(json!({ "name": "  Ana  ", "wristband": "   " })).unwrap();
        assert_eq!(data.name.as_deref(), Some("Ana"));
        assert_eq!(data.wristband, None);
        assert!(
            !PersonData::parse(json!({ "wristband": " " }))
                .unwrap()
                .identifies()
        );
        for bad in [
            json!({ "name": "x".repeat(101) }),
            json!({ "birth_year": 1850 }),
            json!({ "birth_year": 2999 }),
            json!({ "is_child": false, "child_status": "unaccompanied" }),
        ] {
            assert!(PersonData::parse(bad.clone()).is_err(), "{bad}");
        }
        assert_eq!(
            PersonData::parse(json!({ "child_status": "separated" }))
                .unwrap()
                .is_child,
            Some(true)
        );
    }

    #[test]
    fn filling_a_name_or_a_wristband_drops_the_thumbnail_in_the_same_write() {
        for data in [json!({ "name": "Ana" }), json!({ "wristband": "B-1" })] {
            let mut person = person_in("a");
            person.thumbnail_encrypted = Some(vec![1, 2, 3]);
            PersonData::parse(json!({ "needs": { "reduced_mobility": true } }))
                .unwrap()
                .apply(&mut person);
            assert!(person.thumbnail_encrypted.is_some());
            PersonData::parse(data).unwrap().apply(&mut person);
            assert!(person.thumbnail_encrypted.is_none());
        }
    }

    #[test]
    fn merge_joins_stays_keeps_one_open_and_remembers_the_old_id() {
        let mut old = new_person(
            "old",
            Level::NoBiometrics,
            LegalBasis::Consent,
            store::now(),
        );
        old.name = Some("Ana".into());
        old.stays = vec![stay("a", "2026-10-21T10:00:00Z")];
        old.current_shelter_id = Some("a".into());
        let mut new = new_person("new", Level::Assisted, LegalBasis::Consent, store::now());
        new.stays = vec![stay("b", "2026-10-21T08:00:00Z")];
        new.current_shelter_id = Some("b".into());
        let merged = merge_into(new, old, "old", Level::Verified);
        assert_eq!(merged.level, Level::Verified);
        assert_eq!(merged.former_ids, vec!["old".to_string()]);
        assert_eq!(merged.name.as_deref(), Some("Ana"));
        assert_eq!(merged.stays.len(), 2);
        assert_eq!(
            merged.stays.iter().filter(|s| s.left_at.is_none()).count(),
            1
        );
        assert_eq!(merged.current_shelter_id.as_deref(), Some("a"));
        assert_eq!(
            merged.stays[0].exit_reason,
            Some(ExitReason::CheckinElsewhere)
        );
    }

    #[test]
    fn household_and_guardian_references_move_to_the_new_id() {
        let now = store::now();
        let mut household = HouseholdDoc {
            id: None,
            member_ids: vec!["x".into(), "old".into(), "new".into()],
            head_id: Some("old".into()),
            pets: vec![],
            card_code: None,
            created_by: "v".into(),
            created_shelter_id: "s".into(),
            client_created_at: now,
            created_at: now,
            updated_at: now,
        };
        repoint_household(&mut household, "old", "new");
        assert_eq!(household.member_ids, ["x", "new"]);
        assert_eq!(household.head_id.as_deref(), Some("new"));
        let guardian = |id: &str| Guardian {
            person_id: id.into(),
            link: GuardianLink::Declared,
            checked_by: None,
            checked_at: None,
        };
        let mut guardians = vec![guardian("old"), guardian("z")];
        assert!(repoint_guardians(&mut guardians, "old", "new"));
        assert_eq!(guardians[0].person_id, "new");
        assert!(!repoint_guardians(&mut guardians, "old", "new"));
    }

    // ---- API ----

    struct World {
        app: TestApp,
        admin: Actor,
        volunteer: Actor,
        shelter: String,
    }

    /// A confirmed shelter with its admin and one volunteer.
    async fn world() -> World {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app
            .confirmed_shelter(&admin, "Shelter A", north_of_origin(0.0))
            .await;
        let code = app.invite(&admin, &shelter, "link", "volunteer").await;
        let volunteer = app.join(&code, "Volunteer").await;
        World {
            app,
            admin,
            volunteer,
            shelter,
        }
    }

    async fn call(
        app: &TestApp,
        who: &Actor,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let auth = format!("Bearer {}", who.secret);
        let (status, _, json) = app
            .request(
                method,
                path,
                &[
                    ("authorization", auth.as_str()),
                    ("x-device-id", who.device.as_str()),
                ],
                body,
            )
            .await;
        (status, json)
    }

    fn checkin(extra: Value) -> Value {
        let mut body = json!({
            "id": new_id(),
            "legal_basis": "consent",
            "client_created_at": store::now().to_string(),
            "data": {},
        });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        body
    }

    fn photo() -> Value {
        json!(STANDARD.encode(b"a face"))
    }

    fn finished(token: &str) -> (u16, Value) {
        (
            200,
            json!({ "id": "img", "state": "PROCESS_STATE_FINISHED", "data": { "humanToken": { "value": token } } }),
        )
    }

    /// The next `n` photos sent to Unico come back as `token`.
    fn photos_are(app: &TestApp, token: &str, n: usize) {
        app.unico
            .with(|s| (0..n).for_each(|_| s.post_replies.push_back(finished(token))));
    }

    /// A finished `checkin`/`biometrics` capture of the actor's device for `token`; `target` is
    /// the shelter (check-in) or the person (biometrics) it is asked for.
    async fn capture(
        app: &TestApp,
        who: &Actor,
        purpose: &str,
        token: &str,
        target: &str,
    ) -> String {
        let body = match purpose {
            "checkin" => json!({ "purpose": purpose, "shelter_id": target }),
            _ => json!({ "purpose": purpose, "expected_person_id": target }),
        };
        let auth = format!("Bearer {}", who.secret);
        let (status, _, body) = app
            .request(
                "POST",
                "/v1/captures",
                &[
                    ("x-device-id", who.device.as_str()),
                    ("authorization", auth.as_str()),
                ],
                Some(body),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let id = body["capture_id"].as_str().unwrap().to_string();
        let stored: CaptureDoc = store::get(&app.state.db, CAPTURES, &id)
            .await
            .unwrap()
            .unwrap();
        app.unico.finish_with_token(&stored.process_id, token);
        id
    }

    async fn stored(app: &TestApp, id: &str) -> PersonDoc {
        store::get(&app.state.db, COLLECTION, id)
            .await
            .unwrap()
            .unwrap()
    }

    fn token_id(app: &TestApp, token: &str) -> String {
        app.state.person_id_for_token(token)
    }

    #[tokio::test]
    async fn checkin_with_a_selfie_creates_a_verified_person() {
        let w = world().await;
        let capture_id = capture(&w.app, &w.volunteer, "checkin", "tok-ana", &w.shelter).await;
        let (status, body) = call(
            &w.app,
            &w.volunteer,
            "POST",
            &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(
                json!({ "capture_id": capture_id, "data": { "name": "Ana" } }),
            )),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let id = token_id(&w.app, "tok-ana");
        assert_eq!(
            (
                body["person_id"].as_str(),
                body["level"].as_str(),
                body["is_new"].as_bool()
            ),
            (Some(id.as_str()), Some("verified"), Some(true))
        );
        assert_eq!(body["history"]["has_other_stays"], false);
        let person = stored(&w.app, &id).await;
        assert_eq!(person.name.as_deref(), Some("Ana"));
        assert_eq!(
            person.current_shelter_id.as_deref(),
            Some(w.shelter.as_str())
        );
        assert_eq!(person.stays.len(), 1);
        assert!(
            audit_actions(&w.app.state.db)
                .await
                .contains(&"person.checkin".to_string())
        );
        // the capture is spent
        let again = call(
            &w.app,
            &w.volunteer,
            "POST",
            &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(json!({ "capture_id": capture_id }))),
        )
        .await;
        assert_eq!(
            (again.0, again.1["error"].as_str()),
            (StatusCode::CONFLICT, Some("capture_consumed"))
        );
    }

    #[tokio::test]
    async fn checkin_with_a_photo_is_assisted_and_waits_for_a_selfie() {
        let w = world().await;
        photos_are(&w.app, "tok-bia", 1);
        let (status, body) = call(
            &w.app,
            &w.volunteer,
            "POST",
            &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(json!({ "photo_base64": photo() }))),
        )
        .await;
        assert_eq!(
            (status, body["level"].as_str()),
            (StatusCode::CREATED, Some("assisted")),
            "{body}"
        );
        let (_, list) = call(
            &w.app,
            &w.admin,
            "GET",
            &format!("/v1/shelters/{}/people/needs-selfie", w.shelter),
            None,
        )
        .await;
        assert_eq!(list["people"].as_array().unwrap().len(), 1);
        assert_eq!(list["people"][0]["id"], body["person_id"]);
    }

    #[tokio::test]
    async fn checkin_without_biometrics_uses_the_client_id_and_the_refusal_leaves_the_selfie_list()
    {
        let w = world().await;
        let calls_before = w.app.unico.post_count();
        let (declined, kept) = (new_id(), new_id());
        for (id, refused) in [(&declined, true), (&kept, false)] {
            let (status, body) = call(&w.app, &w.volunteer, "POST", &format!("/v1/shelters/{}/checkins", w.shelter),
                Some(checkin(json!({ "id": id, "no_biometrics": true, "biometrics_declined": refused, "data": { "name": "N" } })))).await;
            assert_eq!(
                (status, body["level"].as_str(), body["person_id"].as_str()),
                (
                    StatusCode::CREATED,
                    Some("no_biometrics"),
                    Some(id.as_str())
                ),
                "{body}"
            );
        }
        assert!(stored(&w.app, &declined).await.biometrics_declined);
        assert_eq!(w.app.unico.post_count(), calls_before);
        let (_, selfies) = call(
            &w.app,
            &w.admin,
            "GET",
            &format!("/v1/shelters/{}/people/needs-selfie", w.shelter),
            None,
        )
        .await;
        let ids: Vec<&str> = selfies["people"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, [kept.as_str()]);
        let (_, all) = call(
            &w.app,
            &w.admin,
            "GET",
            &format!("/v1/shelters/{}/people", w.shelter),
            None,
        )
        .await;
        assert_eq!(all["people"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn exactly_one_way_to_identify_is_required_and_only_members_check_in() {
        let w = world().await;
        let path = format!("/v1/shelters/{}/checkins", w.shelter);
        for extra in [
            json!({}),
            json!({ "no_biometrics": true, "photo_base64": photo() }),
            json!({ "capture_id": "c", "photo_base64": photo() }),
        ] {
            let (status, body) =
                call(&w.app, &w.volunteer, "POST", &path, Some(checkin(extra))).await;
            assert_eq!(
                (status, body["error"].as_str()),
                (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_person")),
                "{body}"
            );
        }
        let stranger = w.app.actor("Stranger").await;
        let (status, _) = call(
            &w.app,
            &stranger,
            "POST",
            &path,
            Some(checkin(json!({ "no_biometrics": true }))),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = call(
            &w.app,
            &stranger,
            "GET",
            &format!("/v1/shelters/{}/people", w.shelter),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = call(
            &w.app,
            &w.volunteer,
            "POST",
            "/v1/shelters/nope/checkins",
            Some(checkin(json!({ "no_biometrics": true }))),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_image_that_is_not_base64_is_refused_before_unico() {
        let w = world().await;
        let calls_before = w.app.unico.post_count();
        let (status, body) = call(
            &w.app,
            &w.volunteer,
            "POST",
            &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(json!({ "photo_base64": "***" }))),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_image"))
        );
        assert_eq!(w.app.unico.post_count(), calls_before);
    }

    // criterion 24
    #[tokio::test]
    async fn a_photo_checkin_then_a_selfie_is_the_same_document_raised_to_verified() {
        let w = world().await;
        let path = format!("/v1/shelters/{}/checkins", w.shelter);
        photos_are(&w.app, "tok-1", 1);
        let (_, first) = call(
            &w.app,
            &w.volunteer,
            "POST",
            &path,
            Some(checkin(
                json!({ "photo_base64": photo(), "data": { "name": "Ana" } }),
            )),
        )
        .await;
        let capture_id = capture(&w.app, &w.volunteer, "checkin", "tok-1", &w.shelter).await;
        let (status, second) = call(
            &w.app,
            &w.volunteer,
            "POST",
            &path,
            Some(checkin(json!({ "capture_id": capture_id }))),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{second}");
        assert_eq!(
            (
                second["person_id"].clone(),
                second["level"].as_str(),
                second["is_new"].as_bool()
            ),
            (first["person_id"].clone(), Some("verified"), Some(false))
        );
        assert_eq!(
            crate::test_support::all_docs(&w.app.state.db, COLLECTION)
                .await
                .len(),
            1
        );
        let person = stored(&w.app, first["person_id"].as_str().unwrap()).await;
        assert_eq!(person.name.as_deref(), Some("Ana"));
        assert_eq!(person.stays.len(), 1);
        // and a later photo does not take it back down
        photos_are(&w.app, "tok-1", 1);
        let (_, third) = call(
            &w.app,
            &w.volunteer,
            "POST",
            &path,
            Some(checkin(json!({ "photo_base64": photo() }))),
        )
        .await;
        assert_eq!(third["level"], "verified");
    }

    async fn second_shelter(w: &World) -> (String, Actor) {
        let other = w.app.actor("Other").await;
        let shelter = w
            .app
            .confirmed_shelter(&other, "Shelter B", north_of_origin(500.0))
            .await;
        (shelter, other)
    }

    // criterion 23
    #[tokio::test]
    async fn the_same_token_in_two_shelters_is_one_document_two_stays_one_open() {
        let w = world().await;
        let (b, other) = second_shelter(&w).await;
        photos_are(&w.app, "tok-1", 2);
        let (_, first) = call(
            &w.app,
            &w.volunteer,
            "POST",
            &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(json!({ "photo_base64": photo() }))),
        )
        .await;
        let (status, second) = call(
            &w.app,
            &other,
            "POST",
            &format!("/v1/shelters/{b}/checkins"),
            Some(checkin(json!({ "photo_base64": photo() }))),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{second}");
        assert_eq!(first["person_id"], second["person_id"]);
        assert_eq!(second["is_new"], false);
        let person = stored(&w.app, first["person_id"].as_str().unwrap()).await;
        assert_eq!(person.stays.len(), 2);
        assert_eq!(
            person.stays.iter().filter(|s| s.left_at.is_none()).count(),
            1
        );
        assert_eq!(
            person.stays[0].exit_reason,
            Some(ExitReason::CheckinElsewhere)
        );
        assert_eq!(person.current_shelter_id.as_deref(), Some(b.as_str()));
        assert_eq!(
            crate::test_support::all_docs(&w.app.state.db, COLLECTION)
                .await
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn concurrent_checkins_of_one_token_leave_one_document_and_one_open_stay() {
        let w = world().await;
        let (b, other) = second_shelter(&w).await;
        photos_are(&w.app, "tok-1", 1);
        call(
            &w.app,
            &w.volunteer,
            "POST",
            &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(json!({ "photo_base64": photo() }))),
        )
        .await;
        let n = 6;
        photos_are(&w.app, "tok-1", n);
        let mut tasks = Vec::new();
        for _ in 0..n {
            let (app, who, shelter) = (w.app.clone(), other.clone(), b.clone());
            tasks.push(tokio::spawn(async move {
                call(
                    &app,
                    &who,
                    "POST",
                    &format!("/v1/shelters/{shelter}/checkins"),
                    Some(checkin(json!({ "photo_base64": photo() }))),
                )
                .await
            }));
        }
        for task in tasks {
            let (status, body) = task.await.unwrap();
            assert_eq!(status, StatusCode::CREATED, "{body}");
        }
        assert_eq!(
            crate::test_support::all_docs(&w.app.state.db, COLLECTION)
                .await
                .len(),
            1
        );
        let person = stored(&w.app, &token_id(&w.app, "tok-1")).await;
        assert_eq!(person.stays.len(), 2, "{:?}", person.stays);
        assert_eq!(
            person.stays.iter().filter(|s| s.left_at.is_none()).count(),
            1
        );
        assert_eq!(person.current_shelter_id.as_deref(), Some(b.as_str()));
    }

    #[tokio::test]
    async fn one_capture_used_by_concurrent_checkins_is_consumed_once() {
        let w = world().await;
        let capture_id = capture(&w.app, &w.volunteer, "checkin", "tok-1", &w.shelter).await;
        let mut tasks = Vec::new();
        for _ in 0..5 {
            let (app, who, shelter, capture_id) = (
                w.app.clone(),
                w.volunteer.clone(),
                w.shelter.clone(),
                capture_id.clone(),
            );
            tasks.push(tokio::spawn(async move {
                call(
                    &app,
                    &who,
                    "POST",
                    &format!("/v1/shelters/{shelter}/checkins"),
                    Some(checkin(json!({ "capture_id": capture_id }))),
                )
                .await
            }));
        }
        let mut statuses = Vec::new();
        for task in tasks {
            let (status, body) = task.await.unwrap();
            statuses.push((status, body["error"].as_str().map(str::to_string)));
        }
        assert_eq!(
            statuses
                .iter()
                .filter(|s| s.0 == StatusCode::CREATED)
                .count(),
            1,
            "{statuses:?}"
        );
        assert!(
            statuses
                .iter()
                .filter(|s| s.0 != StatusCode::CREATED)
                .all(|s| s.1.as_deref() == Some("capture_consumed")),
            "{statuses:?}"
        );
        let capture: CaptureDoc = store::get(&w.app.state.db, CAPTURES, &capture_id)
            .await
            .unwrap()
            .unwrap();
        assert!(capture.consumed);
        assert_eq!(
            stored(&w.app, &token_id(&w.app, "tok-1")).await.stays.len(),
            1
        );
    }

    // criterion 36
    #[tokio::test]
    async fn a_volunteer_sees_only_that_there_are_other_stays_and_the_admin_sees_them() {
        let w = world().await;
        let (b, other) = second_shelter(&w).await;
        photos_are(&w.app, "tok-1", 2);
        call(
            &w.app,
            &other,
            "POST",
            &format!("/v1/shelters/{b}/checkins"),
            Some(checkin(json!({ "photo_base64": photo() }))),
        )
        .await;
        let (_, body) = call(
            &w.app,
            &w.volunteer,
            "POST",
            &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(json!({ "photo_base64": photo() }))),
        )
        .await;
        assert_eq!(body["history"]["has_other_stays"], true);
        assert_eq!(body["history"]["stays"].as_array().unwrap().len(), 1);
        let id = body["person_id"].as_str().unwrap();
        let (_, as_volunteer) = call(
            &w.app,
            &w.volunteer,
            "GET",
            &format!("/v1/people/{id}"),
            None,
        )
        .await;
        assert_eq!(
            as_volunteer["person"]["history"]["stays"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(as_volunteer["person"]["history"]["has_other_stays"], true);
        let (_, as_admin) = call(&w.app, &w.admin, "GET", &format!("/v1/people/{id}"), None).await;
        let stays = as_admin["person"]["history"]["stays"].as_array().unwrap();
        assert_eq!(stays.len(), 2);
        assert!(stays.iter().any(|s| s["shelter_id"] == b.as_str()));
        // the other shelter's admin, no longer the current one, sees their own stay and the other as history
        let (_, as_other) = call(&w.app, &other, "GET", &format!("/v1/people/{id}"), None).await;
        assert_eq!(
            as_other["person"]["history"]["stays"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn an_unconfirmed_shelter_sees_only_its_own_stays() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let confirmed = app
            .confirmed_shelter(&admin, "Confirmed", north_of_origin(0.0))
            .await;
        let other = app.actor("Other").await;
        let unconfirmed = app
            .create_shelter(&other, "Unconfirmed", north_of_origin(500.0))
            .await;
        photos_are(&app, "tok-1", 2);
        call(
            &app,
            &admin,
            "POST",
            &format!("/v1/shelters/{confirmed}/checkins"),
            Some(checkin(json!({ "photo_base64": photo() }))),
        )
        .await;
        let (_, body) = call(
            &app,
            &other,
            "POST",
            &format!("/v1/shelters/{unconfirmed}/checkins"),
            Some(checkin(json!({ "photo_base64": photo() }))),
        )
        .await;
        assert_eq!(body["history"]["has_other_stays"], false);
        assert_eq!(body["history"]["stays"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn people_nobody_of_the_viewer_registered_are_not_found() {
        let w = world().await;
        let id = new_id();
        call(
            &w.app,
            &w.volunteer,
            "POST",
            &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(json!({ "id": id, "no_biometrics": true }))),
        )
        .await;
        let stranger = w.app.actor("Stranger").await;
        for (method, body) in [
            ("GET", None),
            (
                "PATCH",
                Some(json!({ "data": {}, "client_edited_at": store::now().to_string() })),
            ),
        ] {
            let (status, _) =
                call(&w.app, &stranger, method, &format!("/v1/people/{id}"), body).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method}");
        }
        let (status, _) = call(
            &w.app,
            &stranger,
            "PUT",
            &format!("/v1/people/{id}/biometrics"),
            Some(json!({ "photo_base64": photo() })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&w.app, &w.volunteer, "GET", "/v1/people/missing", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // criterion 38
    #[tokio::test]
    async fn needs_take_only_the_four_booleans_on_checkin_and_patch() {
        let w = world().await;
        let path = format!("/v1/shelters/{}/checkins", w.shelter);
        let (status, body) = call(&w.app, &w.volunteer, "POST", &path,
            Some(checkin(json!({ "no_biometrics": true, "data": { "needs": { "continuous_medication": "insulin 2x" } } })))).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_person")),
            "{body}"
        );
        let id = new_id();
        let (status, _) = call(&w.app, &w.volunteer, "POST", &path,
            Some(checkin(json!({ "id": id, "no_biometrics": true, "data": { "needs": { "reduced_mobility": true }, "vulnerabilities": { "pregnant": true, "disability": ["visual"], "migrant_language": "es" } } })))).await;
        assert_eq!(status, StatusCode::CREATED);
        let edited = store::now().to_string();
        let (status, _) = call(&w.app, &w.volunteer, "PATCH", &format!("/v1/people/{id}"),
            Some(json!({ "data": { "needs": { "allergy": "peanuts" } }, "client_edited_at": edited }))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, body) = call(&w.app, &w.volunteer, "PATCH", &format!("/v1/people/{id}"),
            Some(json!({ "data": { "needs": { "dietary_restriction": true } }, "client_edited_at": edited }))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let person = stored(&w.app, &id).await;
        assert!(person.needs.dietary_restriction && !person.needs.reduced_mobility);
        assert!(person.vulnerabilities.pregnant);
        assert_eq!(
            person.vulnerabilities.migrant_language.as_deref(),
            Some("es")
        );
    }

    #[tokio::test]
    async fn the_last_edit_wins_and_an_older_one_is_ignored() {
        let w = world().await;
        let id = new_id();
        call(
            &w.app,
            &w.volunteer,
            "POST",
            &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(json!({ "id": id, "no_biometrics": true }))),
        )
        .await;
        let path = format!("/v1/people/{id}");
        let edit = |name: &str, time: &str| {
            Some(json!({ "data": { "name": name }, "client_edited_at": time }))
        };
        let (status, _) = call(
            &w.app,
            &w.volunteer,
            "PATCH",
            &path,
            edit("Second", "2026-10-21T12:00:00Z"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = call(
            &w.app,
            &w.volunteer,
            "PATCH",
            &path,
            edit("First", "2026-10-21T11:00:00Z"),
        )
        .await;
        assert_eq!(
            (status, body["person"]["name"].as_str()),
            (StatusCode::OK, Some("Second"))
        );
        assert_eq!(stored(&w.app, &id).await.name.as_deref(), Some("Second"));
        let (status, _) = call(
            &w.app,
            &w.volunteer,
            "PATCH",
            &path,
            Some(json!({ "data": {} })),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn child_status_changes_only_by_protection_or_civil_defense() {
        let w = world().await;
        let id = new_id();
        call(&w.app, &w.volunteer, "POST", &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(json!({ "id": id, "no_biometrics": true, "legal_basis": "child_protection", "data": { "is_child": true, "child_status": "unaccompanied" } })))).await;
        let path = format!("/v1/people/{id}");
        let patch = |status: &str| {
            Some(
                json!({ "data": { "child_status": status }, "client_edited_at": store::now().to_string() }),
            )
        };
        for who in [&w.volunteer, &w.admin] {
            let (status, body) = call(&w.app, who, "PATCH", &path, patch("with_guardian")).await;
            assert_eq!(
                (status, body["error"].as_str()),
                (StatusCode::FORBIDDEN, Some("protection_role_required"))
            );
        }
        // the same value is not a change
        let (status, _) = call(&w.app, &w.volunteer, "PATCH", &path, patch("unaccompanied")).await;
        assert_eq!(status, StatusCode::OK);
        w.app
            .grant(&w.admin, crate::volunteers::GlobalRole::Protection)
            .await;
        let (status, body) = call(&w.app, &w.admin, "PATCH", &path, patch("with_guardian")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            stored(&w.app, &id).await.child_status,
            Some(ChildStatus::WithGuardian)
        );
    }

    // criteria 39 and 40, and the storage of the bytes
    #[tokio::test]
    async fn the_thumbnail_is_stored_encrypted_as_bytes_audited_on_read_and_dropped_with_a_name() {
        let w = world().await;
        let id = new_id();
        let upload = STANDARD.encode(JPEG);
        let (status, body) = call(
            &w.app,
            &w.volunteer,
            "POST",
            &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(
                json!({ "id": id, "no_biometrics": true, "thumbnail_base64": upload }),
            )),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let person = stored(&w.app, &id).await;
        let sealed = person.thumbnail_encrypted.clone().unwrap();
        assert!(!sealed.windows(JPEG.len()).any(|w| w == JPEG));
        assert_eq!(
            thumbnail::decrypt(w.app.state.thumbnail_key(), &sealed).unwrap(),
            JPEG
        );

        // Firestore holds a bytes value, not an array of integers
        let db = &w.app.state.db;
        let url = format!(
            "http://{}/v1/{}/documents/people/{id}",
            std::env::var("FIRESTORE_EMULATOR_HOST").unwrap(),
            db.get_database_path()
        );
        let raw: Value = reqwest::Client::new()
            .get(url)
            .header("authorization", "Bearer owner")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(
            raw["fields"]["thumbnail_encrypted"]["bytesValue"].is_string(),
            "{raw}"
        );

        let path = format!("/v1/people/{id}/thumbnail");
        let auth = format!("Bearer {}", w.volunteer.secret);
        let (status, headers, _) = w
            .app
            .request("GET", &path, &[("authorization", auth.as_str())], None)
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["content-type"], "image/jpeg");
        assert!(
            audit_actions(db)
                .await
                .contains(&"thumbnail.view".to_string())
        );
        let (_, listed) = call(
            &w.app,
            &w.volunteer,
            "GET",
            &format!("/v1/people/{id}"),
            None,
        )
        .await;
        assert_eq!(listed["person"]["has_thumbnail"], true);
        assert!(!listed.to_string().contains("thumbnail_encrypted"));

        // a volunteer of another shelter: 403, and nothing is audited for it
        let (_, other) = second_shelter(&w).await;
        let views_before = audit_actions(db)
            .await
            .iter()
            .filter(|a| *a == "thumbnail.view")
            .count();
        let (status, _) = call(&w.app, &other, "GET", &path, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            audit_actions(db)
                .await
                .iter()
                .filter(|a| *a == "thumbnail.view")
                .count(),
            views_before
        );

        // the same write that fills the name removes the thumbnail
        let (status, _) = call(
            &w.app,
            &w.volunteer,
            "PATCH",
            &format!("/v1/people/{id}"),
            Some(
                json!({ "data": { "name": "Ana" }, "client_edited_at": store::now().to_string() }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(stored(&w.app, &id).await.thumbnail_encrypted.is_none());
        let (status, body) = call(&w.app, &w.volunteer, "GET", &path, None).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::NOT_FOUND, Some("thumbnail_not_found"))
        );
    }

    #[tokio::test]
    async fn a_thumbnail_is_not_kept_for_someone_with_a_name_or_wristband_and_must_be_a_jpeg() {
        let w = world().await;
        let path = format!("/v1/shelters/{}/checkins", w.shelter);
        let id = new_id();
        call(&w.app, &w.volunteer, "POST", &path, Some(checkin(json!({ "id": id, "no_biometrics": true, "data": { "wristband": "B-7" }, "thumbnail_base64": STANDARD.encode(JPEG) })))).await;
        assert!(stored(&w.app, &id).await.thumbnail_encrypted.is_none());
        let (status, body) = call(&w.app, &w.volunteer, "POST", &path, Some(checkin(json!({ "no_biometrics": true, "thumbnail_base64": STANDARD.encode(b"not a jpeg") })))).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_thumbnail"))
        );
    }

    // ---- late biometrics (criterion 26) ----

    fn household(members: &[&str], head: Option<&str>) -> HouseholdDoc {
        let now = store::now();
        HouseholdDoc {
            id: None,
            member_ids: members.iter().map(|m| m.to_string()).collect(),
            head_id: head.map(str::to_string),
            pets: vec![],
            card_code: None,
            created_by: "v".into(),
            created_shelter_id: "s".into(),
            client_created_at: now,
            created_at: now,
            updated_at: now,
        }
    }

    async fn put_household(app: &TestApp, id: &str, doc: &HouseholdDoc) {
        store::replace(&app.state.db, households::COLLECTION, id, doc)
            .await
            .unwrap();
        let mut person: PersonDoc = stored(app, doc.member_ids.first().unwrap()).await;
        person.household_id = Some(id.into());
        store::replace(
            &app.state.db,
            COLLECTION,
            doc.member_ids.first().unwrap(),
            &person,
        )
        .await
        .unwrap();
    }

    async fn register_without_biometrics(w: &World, name: &str) -> String {
        let id = new_id();
        let (status, body) = call(
            &w.app,
            &w.volunteer,
            "POST",
            &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(
                json!({ "id": id, "no_biometrics": true, "data": { "name": name } }),
            )),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        id
    }

    #[tokio::test]
    async fn late_biometrics_with_a_new_token_moves_the_record_and_every_reference() {
        let w = world().await;
        let old = register_without_biometrics(&w, "Ana").await;
        let kin = register_without_biometrics(&w, "Bia").await;
        put_household(&w.app, "h1", &household(&[&old, &kin], Some(&old))).await;
        // a child who lists the old record as guardian and documents of later phases
        let mut child = stored(&w.app, &kin).await;
        child.guardians.push(Guardian {
            person_id: old.clone(),
            link: GuardianLink::Declared,
            checked_by: None,
            checked_at: None,
        });
        child.sync_guardian_ids();
        store::replace(&w.app.state.db, COLLECTION, &kin, &child)
            .await
            .unwrap();
        for collection in ["deliveries", "rescues"] {
            store::replace(
                &w.app.state.db,
                collection,
                "d1",
                &PersonRef {
                    id: None,
                    person_id: old.clone(),
                },
            )
            .await
            .unwrap();
            store::replace(
                &w.app.state.db,
                collection,
                "d2",
                &PersonRef {
                    id: None,
                    person_id: kin.clone(),
                },
            )
            .await
            .unwrap();
        }

        let capture_id = capture(&w.app, &w.volunteer, "biometrics", "tok-ana", &old).await;
        let (status, body) = call(
            &w.app,
            &w.volunteer,
            "PUT",
            &format!("/v1/people/{old}/biometrics"),
            Some(json!({ "capture_id": capture_id })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let new = token_id(&w.app, "tok-ana");
        assert_eq!(
            (body["person_id"].as_str(), body["level"].as_str()),
            (Some(new.as_str()), Some("verified"))
        );

        let moved = stored(&w.app, &new).await;
        assert_eq!(moved.name.as_deref(), Some("Ana"));
        assert_eq!(moved.former_ids, vec![old.clone()]);
        assert_eq!(moved.household_id.as_deref(), Some("h1"));
        assert_eq!(moved.stays.len(), 1);
        assert!(
            store::get::<PersonDoc>(&w.app.state.db, COLLECTION, &old)
                .await
                .unwrap()
                .is_none()
        );
        let h: HouseholdDoc = store::get(&w.app.state.db, households::COLLECTION, "h1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(h.member_ids, [new.clone(), kin.clone()]);
        assert_eq!(h.head_id.as_deref(), Some(new.as_str()));
        assert_eq!(stored(&w.app, &kin).await.guardians[0].person_id, new);
        for collection in ["deliveries", "rescues"] {
            let d1: PersonRef = store::get(&w.app.state.db, collection, "d1")
                .await
                .unwrap()
                .unwrap();
            let d2: PersonRef = store::get(&w.app.state.db, collection, "d2")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                (d1.person_id, d2.person_id),
                (new.clone(), kin.clone()),
                "{collection}"
            );
        }
        let capture: CaptureDoc = store::get(&w.app.state.db, CAPTURES, &capture_id)
            .await
            .unwrap()
            .unwrap();
        assert!(capture.consumed);
        assert!(
            audit_actions(&w.app.state.db)
                .await
                .contains(&"person.biometrics".to_string())
        );
        // the old id now leads nowhere
        let (status, _) = call(
            &w.app,
            &w.volunteer,
            "GET",
            &format!("/v1/people/{old}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn late_biometrics_by_photo_makes_the_person_assisted() {
        let w = world().await;
        let old = register_without_biometrics(&w, "Ana").await;
        photos_are(&w.app, "tok-ana", 1);
        let (status, body) = call(
            &w.app,
            &w.volunteer,
            "PUT",
            &format!("/v1/people/{old}/biometrics"),
            Some(json!({ "photo_base64": photo() })),
        )
        .await;
        assert_eq!(
            (status, body["level"].as_str()),
            (StatusCode::OK, Some("assisted")),
            "{body}"
        );
        assert_eq!(
            stored(&w.app, &token_id(&w.app, "tok-ana")).await.level,
            Level::Assisted
        );
    }

    #[tokio::test]
    async fn late_biometrics_of_a_token_that_already_exists_joins_the_stays() {
        let w = world().await;
        let (b, other) = second_shelter(&w).await;
        photos_are(&w.app, "tok-ana", 1);
        let (_, elsewhere) = call(
            &w.app,
            &other,
            "POST",
            &format!("/v1/shelters/{b}/checkins"),
            Some(checkin(
                json!({ "photo_base64": photo(), "client_created_at": "2026-10-20T10:00:00Z" }),
            )),
        )
        .await;
        let existing = elsewhere["person_id"].as_str().unwrap().to_string();
        let old = register_without_biometrics(&w, "Ana").await;
        let capture_id = capture(&w.app, &w.volunteer, "biometrics", "tok-ana", &old).await;
        let (status, body) = call(
            &w.app,
            &w.volunteer,
            "PUT",
            &format!("/v1/people/{old}/biometrics"),
            Some(json!({ "capture_id": capture_id })),
        )
        .await;
        assert_eq!(
            (status, body["person_id"].as_str(), body["level"].as_str()),
            (StatusCode::OK, Some(existing.as_str()), Some("verified")),
            "{body}"
        );
        let person = stored(&w.app, &existing).await;
        assert_eq!(person.stays.len(), 2);
        assert_eq!(
            person.stays.iter().filter(|s| s.left_at.is_none()).count(),
            1
        );
        assert_eq!(
            person.current_shelter_id.as_deref(),
            Some(w.shelter.as_str())
        );
        assert_eq!(person.former_ids, vec![old.clone()]);
        assert_eq!(person.name.as_deref(), Some("Ana"));
        assert_eq!(
            crate::test_support::all_docs(&w.app.state.db, COLLECTION)
                .await
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn two_different_households_keep_the_one_of_the_token_and_raise_a_conflict() {
        let w = world().await;
        photos_are(&w.app, "tok-ana", 1);
        let (_, first) = call(
            &w.app,
            &w.volunteer,
            "POST",
            &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(json!({ "photo_base64": photo() }))),
        )
        .await;
        let existing = first["person_id"].as_str().unwrap().to_string();
        let old = register_without_biometrics(&w, "Ana").await;
        put_household(&w.app, "h-token", &household(&[&existing], Some(&existing))).await;
        put_household(&w.app, "h-old", &household(&[&old], Some(&old))).await;
        let capture_id = capture(&w.app, &w.volunteer, "biometrics", "tok-ana", &old).await;
        let (status, body) = call(
            &w.app,
            &w.volunteer,
            "PUT",
            &format!("/v1/people/{old}/biometrics"),
            Some(json!({ "capture_id": capture_id })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            stored(&w.app, &existing).await.household_id.as_deref(),
            Some("h-token")
        );
        let kept: HouseholdDoc = store::get(&w.app.state.db, households::COLLECTION, "h-token")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(kept.member_ids, [existing.clone()]);
        let left: HouseholdDoc = store::get(&w.app.state.db, households::COLLECTION, "h-old")
            .await
            .unwrap()
            .unwrap();
        assert!(left.member_ids.is_empty());
        let alerts = crate::test_support::all_docs(&w.app.state.db, alerts::COLLECTION).await;
        let conflict: Vec<_> = alerts
            .iter()
            .filter(|a| a["kind"] == "household_conflict")
            .collect();
        assert_eq!(conflict.len(), 1, "{alerts:?}");
        assert_eq!(conflict[0]["shelter_ids"], json!([w.shelter]));
        assert_eq!(conflict[0]["target_id"], existing.as_str());
    }

    /// A request with the actor's session, device and an `Idempotency-Key`.
    async fn keyed(
        app: &TestApp,
        who: &Actor,
        method: &str,
        path: &str,
        key: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let auth = format!("Bearer {}", who.secret);
        let (status, _, json) = app
            .request(
                method,
                path,
                &[
                    ("authorization", auth.as_str()),
                    ("x-device-id", who.device.as_str()),
                    ("idempotency-key", key),
                ],
                Some(body),
            )
            .await;
        (status, json)
    }

    #[tokio::test]
    async fn a_photo_sent_again_after_a_failure_does_not_pay_unico_twice() {
        // Criterion 55 on the photo routes: the call to Unico comes before the transaction, so a
        // request refused after it and sent again with the same key must reuse its answer.
        let w = world().await;
        photos_are(&w.app, "tok-1", 1);
        let (_, first) = call(
            &w.app,
            &w.volunteer,
            "POST",
            &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(json!({ "photo_base64": photo() }))),
        )
        .await;
        let person = first["person_id"].as_str().unwrap();
        let path = format!("/v1/people/{person}/biometrics");
        let paid = w.app.unico.post_count();

        // Someone else's face: refused after the paid call.
        photos_are(&w.app, "tok-2", 1);
        let (status, body) = keyed(
            &w.app,
            &w.volunteer,
            "PUT",
            &path,
            "key-1",
            json!({ "photo_base64": photo() }),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::CONFLICT, Some("person_mismatch"))
        );
        assert_eq!(w.app.unico.post_count(), paid + 1);
        for _ in 0..3 {
            let (status, body) = keyed(
                &w.app,
                &w.volunteer,
                "PUT",
                &path,
                "key-1",
                json!({ "photo_base64": photo() }),
            )
            .await;
            assert_eq!(
                (status, body["error"].as_str()),
                (StatusCode::CONFLICT, Some("person_mismatch"))
            );
        }
        assert_eq!(
            w.app.unico.post_count(),
            paid + 1,
            "the retries made no paid call"
        );

        // Another key is another request, and asks again.
        photos_are(&w.app, "tok-2", 1);
        keyed(
            &w.app,
            &w.volunteer,
            "PUT",
            &path,
            "key-2",
            json!({ "photo_base64": photo() }),
        )
        .await;
        assert_eq!(w.app.unico.post_count(), paid + 2);
        // Without a key every call asks.
        photos_are(&w.app, "tok-2", 2);
        for _ in 0..2 {
            call(
                &w.app,
                &w.volunteer,
                "PUT",
                &path,
                Some(json!({ "photo_base64": photo() })),
            )
            .await;
        }
        assert_eq!(w.app.unico.post_count(), paid + 4);
    }

    #[tokio::test]
    async fn a_photo_unico_refuses_is_a_422_that_is_not_repeated_nor_kept_as_paid() {
        let w = world().await;
        let refuse = |app: &TestApp, n: usize| {
            app.unico.with(|s| {
                (0..n).for_each(|_| {
                    s.post_replies
                        .push_back((400, json!({ "code": 3, "message": "INVALID_REQUEST" })))
                })
            })
        };
        let path = format!("/v1/shelters/{}/checkins", w.shelter);
        let before = w.app.unico.post_count();

        // Check-in by photo: one call to Unico, no retry on the server, a 422 and nothing written.
        refuse(&w.app, 1);
        let (status, body) = keyed(
            &w.app,
            &w.volunteer,
            "POST",
            &path,
            "k-refused",
            checkin(json!({ "photo_base64": photo() })),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("photo_rejected")),
            "{body}"
        );
        assert_eq!(w.app.unico.post_count(), before + 1);
        assert!(
            crate::test_support::all_docs(&w.app.state.db, COLLECTION)
                .await
                .is_empty()
        );
        let kept = crate::test_support::all_docs(&w.app.state.db, "idempotency").await;
        assert!(
            kept.is_empty(),
            "a refusal is not kept as an answer, paid or not: {kept:?}"
        );

        // The same, late biometrics by photo.
        let person = register_without_biometrics(&w, "Ana").await;
        refuse(&w.app, 1);
        let (status, body) = keyed(
            &w.app,
            &w.volunteer,
            "PUT",
            &format!("/v1/people/{person}/biometrics"),
            "k-late",
            json!({ "photo_base64": photo() }),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("photo_rejected")),
            "{body}"
        );
        assert_eq!(stored(&w.app, &person).await.level, Level::NoBiometrics);
        assert!(
            crate::test_support::all_docs(&w.app.state.db, "idempotency")
                .await
                .is_empty()
        );

        // The codes Unico does identify keep their own.
        w.app.unico.with(|s| {
            s.post_replies
                .push_back((400, json!({ "code": 20532, "message": "no face" })))
        });
        let (status, body) = call(
            &w.app,
            &w.volunteer,
            "POST",
            &path,
            Some(checkin(json!({ "photo_base64": photo() }))),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("no_face"))
        );
    }

    #[tokio::test]
    async fn a_kept_answer_is_not_handed_to_another_volunteer_or_route() {
        let w = world().await;
        photos_are(&w.app, "tok-1", 1);
        let path = format!("/v1/shelters/{}/checkins", w.shelter);
        let (status, first) = keyed(
            &w.app,
            &w.volunteer,
            "POST",
            &path,
            "key-x",
            checkin(json!({ "photo_base64": photo() })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{first}");
        // A key reused by someone else gets the middleware's refusal and no kept answer.
        let (status, body) = keyed(
            &w.app,
            &w.admin,
            "POST",
            &path,
            "key-x",
            checkin(json!({ "photo_base64": photo() })),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("idempotency_key_mismatch")
            )
        );
        // The document is kept under its own id, with the id of the person and no token.
        let kept = crate::test_support::all_docs(&w.app.state.db, "idempotency").await;
        let photo_doc = kept
            .iter()
            .find(|d| d["_firestore_id"] == "photo~key-x")
            .unwrap();
        assert_eq!(photo_doc["person_id"], first["person_id"]);
        assert!(!photo_doc.to_string().contains("tok-1"));
        // `~` is not accepted in a key, so a client cannot aim at those documents.
        let (status, body) = keyed(
            &w.app,
            &w.volunteer,
            "POST",
            &path,
            "photo~key-x",
            checkin(json!({ "photo_base64": photo() })),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_idempotency_key"))
        );
    }

    #[tokio::test]
    async fn late_biometrics_needs_one_proof_and_a_token_that_matches_the_record() {
        let w = world().await;
        let old = register_without_biometrics(&w, "Ana").await;
        let path = format!("/v1/people/{old}/biometrics");
        for bad in [
            json!({}),
            json!({ "capture_id": "c", "photo_base64": photo() }),
        ] {
            let (status, _) = call(&w.app, &w.volunteer, "PUT", &path, Some(bad)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        }
        // a person who already has a token cannot be pointed at someone else's
        photos_are(&w.app, "tok-1", 1);
        let (_, first) = call(
            &w.app,
            &w.volunteer,
            "POST",
            &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(json!({ "photo_base64": photo() }))),
        )
        .await;
        photos_are(&w.app, "tok-2", 1);
        let (status, body) = call(
            &w.app,
            &w.volunteer,
            "PUT",
            &format!(
                "/v1/people/{}/biometrics",
                first["person_id"].as_str().unwrap()
            ),
            Some(json!({ "photo_base64": photo() })),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::CONFLICT, Some("person_mismatch"))
        );
        // and a selfie of the same token only raises it
        let capture_id = capture(
            &w.app,
            &w.volunteer,
            "biometrics",
            "tok-1",
            first["person_id"].as_str().unwrap(),
        )
        .await;
        let (status, body) = call(
            &w.app,
            &w.volunteer,
            "PUT",
            &format!(
                "/v1/people/{}/biometrics",
                first["person_id"].as_str().unwrap()
            ),
            Some(json!({ "capture_id": capture_id })),
        )
        .await;
        assert_eq!(
            (status, body["level"].as_str()),
            (StatusCode::OK, Some("verified")),
            "{body}"
        );
    }

    #[tokio::test]
    async fn the_unico_photo_limit_is_enforced_without_a_call() {
        let w = world().await;
        let calls_before = w.app.unico.post_count();
        let big = STANDARD.encode(vec![1u8; 800 * 1024 + 1]);
        let (status, body) = call(
            &w.app,
            &w.volunteer,
            "POST",
            &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(json!({ "photo_base64": big }))),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::PAYLOAD_TOO_LARGE, Some("image_too_large"))
        );
        assert_eq!(w.app.unico.post_count(), calls_before);
    }

    #[tokio::test]
    async fn a_taken_over_item_records_the_original_author_in_the_audit() {
        let w = world().await;
        let id = new_id();
        let auth = format!("Bearer {}", w.admin.secret);
        let headers = [
            ("authorization", auth.as_str()),
            ("x-device-id", "dev-Admin"),
            ("x-taken-over-from", "volunteer-original"),
        ];
        let (status, _, body) = w
            .app
            .request(
                "POST",
                &format!("/v1/shelters/{}/checkins", w.shelter),
                &headers,
                Some(checkin(json!({ "id": id, "no_biometrics": true }))),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let patch =
            json!({ "data": { "name": "Ana" }, "client_edited_at": store::now().to_string() });
        let (status, _, _) = w
            .app
            .request("PATCH", &format!("/v1/people/{id}"), &headers, Some(patch))
            .await;
        assert_eq!(status, StatusCode::OK);
        let entries = crate::test_support::all_docs(&w.app.state.db, "audit_log").await;
        for action in ["person.checkin", "person.update"] {
            let entry = entries.iter().find(|e| e["action"] == action).unwrap();
            assert_eq!(entry["taken_over_from"], "volunteer-original", "{action}");
        }
    }
}
