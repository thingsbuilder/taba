//! Shelters (spec 3.2, 3.9, 4.2, 7, 8.2, 8.3): creation under the 100 m rule, edits, closing,
//! and confirmation by a validator.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use firestore::{FirestoreDb, FirestoreInstant};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::alerts::{self, AlertDoc, AlertKind};
use crate::audit::{self, AuditEntry};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::events::{self, EventDoc};
use crate::exits;
use crate::geo;
use crate::members::{self, MembershipDoc, ShelterRole};
use crate::store::{self, tx_create, tx_replace};
use crate::volunteers::{self, GlobalRole, VolunteerDoc};

pub const COLLECTION: &str = "shelters";
/// Largest `accuracy` accepted when creating a shelter or endorsing one (spec 4.2).
pub const MAX_ACCURACY_M: f64 = 100.0;
const MAX_NAME_CHARS: usize = 100;
const MAX_CAPACITY: u32 = 100_000;
const MAX_REASON_CHARS: usize = 500;
const MAX_ADDRESS_TEXT_CHARS: usize = 500;
const MAX_ADDRESS_PART_CHARS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShelterStatus {
    Open,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confirmation {
    Unconfirmed,
    Confirmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationType {
    Validator,
    InPerson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndorsementKind {
    InPerson,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// True until Nominatim answers (spec 6.1.6).
    #[serde(default)]
    pub pending: bool,
}

/// The address as the app sends it; blank parts count as absent.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AddressInput {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_shelter");
    error.detail = json!({ "field": field });
    error
}

fn part(value: &Option<String>, max: usize) -> Result<Option<String>, ApiError> {
    let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > max {
        return Err(invalid("address"));
    }
    Ok(Some(value.into()))
}

impl Address {
    /// What a shelter has before its address is known.
    fn pending() -> Self {
        Self {
            pending: true,
            ..Self::default()
        }
    }

    /// `None` when the input holds no address at all.
    fn from_input(input: &AddressInput) -> Result<Option<Self>, ApiError> {
        let address = Self {
            text: part(&input.text, MAX_ADDRESS_TEXT_CHARS)?,
            city: part(&input.city, MAX_ADDRESS_PART_CHARS)?,
            state: part(&input.state, MAX_ADDRESS_PART_CHARS)?,
            pending: false,
        };
        let empty = address.text.is_none() && address.city.is_none() && address.state.is_none();
        Ok((!empty).then_some(address))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Endorsement {
    pub kind: EndorsementKind,
    pub volunteer_id: String,
    pub endorser_shelter_id: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub at: FirestoreInstant,
    pub distance_m: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Revocation {
    pub by: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub at: FirestoreInstant,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShelterDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: f64,
    pub geohash: String,
    pub address: Address,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<u32>,
    pub created_by: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub client_created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    pub status: ShelterStatus,
    pub confirmation: Confirmation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation_type: Option<ConfirmationType>,
    pub can_endorse: bool,
    #[serde(default)]
    pub endorsements: Vec<Endorsement>,
    #[serde(default)]
    pub revocations: Vec<Revocation>,
    #[serde(default)]
    pub flagged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_site_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflicts_with: Option<String>,
    /// What the shelter gives and the rule of each item (spec 3.5.2); set by `PUT /v1/shelters/{id}/items`.
    #[serde(default, skip_serializing_if = "crate::deliveries::Items::is_empty")]
    pub items: crate::deliveries::Items,
}

impl ShelterDoc {
    /// Confirms the shelter and lets it endorse others (spec 2.2).
    pub fn confirm(&mut self, kind: ConfirmationType) {
        self.confirmation = Confirmation::Confirmed;
        self.confirmation_type = Some(kind);
        self.can_endorse = true;
    }
}

// ---- views ----

/// The shelter as members, validators and the Civil Defense see it.
pub fn full_view(
    id: &str,
    shelter: &ShelterDoc,
    event: Option<(&str, &EventDoc)>,
    role: Option<ShelterRole>,
    occupancy: u32,
) -> Value {
    json!({
        "id": id,
        "name": shelter.name,
        "lat": shelter.lat,
        "lon": shelter.lon,
        "accuracy_m": shelter.accuracy_m,
        "address": shelter.address,
        "event_id": shelter.event_id,
        "event": event.map(|(event_id, event)| event.view(event_id)),
        "capacity": shelter.capacity,
        "occupancy": occupancy,
        "vacancies": exits::vacancies(shelter.capacity, occupancy),
        "status": shelter.status,
        "confirmation": shelter.confirmation,
        "confirmation_type": shelter.confirmation_type,
        "can_endorse": shelter.can_endorse,
        "flagged": shelter.flagged,
        "created_at": shelter.created_at.to_string(),
        "role": role,
        "items": shelter.items,
    })
}

/// What any other volunteer sees: name, confirmation and vacancies (spec 8.2).
fn public_view(id: &str, shelter: &ShelterDoc, occupancy: u32) -> Value {
    json!({
        "id": id,
        "name": shelter.name,
        "confirmation": shelter.confirmation,
        "capacity": shelter.capacity,
        "occupancy": occupancy,
        "vacancies": exits::vacancies(shelter.capacity, occupancy),
    })
}

async fn event_of(
    db: &FirestoreDb,
    shelter: &ShelterDoc,
) -> Result<Option<(String, EventDoc)>, ApiError> {
    let Some(event_id) = &shelter.event_id else {
        return Ok(None);
    };
    Ok(store::get::<EventDoc>(db, events::COLLECTION, event_id)
        .await?
        .map(|event| (event_id.clone(), event)))
}

/// The view that fits the caller (spec 8.2): full for members, validators and Civil Defense.
pub async fn view_for(
    db: &FirestoreDb,
    volunteer: &Volunteer,
    id: &str,
    shelter: &ShelterDoc,
) -> Result<Value, ApiError> {
    let role = volunteer.role_in(id);
    let occupancy = exits::occupancy(db, id).await?;
    if role.is_none() && !volunteer.is_validator_or_civil_defense() {
        return Ok(public_view(id, shelter, occupancy));
    }
    let event = event_of(db, shelter).await?;
    Ok(full_view(
        id,
        shelter,
        event
            .as_ref()
            .map(|(event_id, event)| (event_id.as_str(), event)),
        role,
        occupancy,
    ))
}

// ---- validation ----

#[derive(Debug, Deserialize)]
pub struct CreateShelter {
    id: String,
    name: String,
    lat: f64,
    lon: f64,
    accuracy: f64,
    #[serde(default)]
    address: Option<AddressInput>,
    #[serde(default)]
    capacity: Option<u32>,
    #[serde(default)]
    other_site_reason: Option<String>,
    client_created_at: String,
}

#[derive(Debug, Clone)]
struct NewShelter {
    id: String,
    name: String,
    lat: f64,
    lon: f64,
    accuracy_m: f64,
    address: Address,
    capacity: Option<u32>,
    other_site_reason: Option<String>,
    client_created_at: FirestoreInstant,
}

fn valid_name(name: &str) -> Result<String, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(invalid("name"));
    }
    Ok(name.into())
}

fn valid_capacity(capacity: u32) -> Result<u32, ApiError> {
    if capacity == 0 || capacity > MAX_CAPACITY {
        return Err(invalid("capacity"));
    }
    Ok(capacity)
}

pub fn valid_position(lat: f64, lon: f64, accuracy: f64) -> Result<(), ApiError> {
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(invalid("position"));
    }
    if !accuracy.is_finite() || accuracy < 0.0 {
        return Err(invalid("accuracy"));
    }
    if accuracy > MAX_ACCURACY_M {
        return Err(insufficient_accuracy(accuracy));
    }
    Ok(())
}

pub fn insufficient_accuracy(accuracy: f64) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "insufficient_accuracy");
    error.detail = json!({ "accuracy": accuracy, "max": MAX_ACCURACY_M });
    error
}

fn validate(request: CreateShelter) -> Result<NewShelter, ApiError> {
    uuid::Uuid::parse_str(&request.id).map_err(|_| invalid("id"))?;
    let name = valid_name(&request.name)?;
    let capacity = request.capacity.map(valid_capacity).transpose()?;
    let client_created_at = request
        .client_created_at
        .parse()
        .map_err(|_| invalid("client_created_at"))?;
    let other_site_reason = match request.other_site_reason.as_deref().map(str::trim) {
        None => None,
        Some(reason) if reason.is_empty() || reason.chars().count() > MAX_REASON_CHARS => {
            return Err(invalid("other_site_reason"));
        }
        Some(reason) => Some(reason.to_string()),
    };
    let address = match &request.address {
        Some(input) => Address::from_input(input)?.unwrap_or_else(Address::pending),
        None => Address::pending(),
    };
    valid_position(request.lat, request.lon, request.accuracy)?;
    Ok(NewShelter {
        id: request.id,
        name,
        lat: request.lat,
        lon: request.lon,
        accuracy_m: request.accuracy,
        address,
        capacity,
        other_site_reason,
        client_created_at,
    })
}

// ---- POST /v1/shelters ----

struct Created {
    shelter: ShelterDoc,
    event_id: String,
    event: EventDoc,
}

/// The `409 shelter_nearby` body: who made the nearby shelter, so the two can talk (spec 3.2.7).
async fn conflict(
    db: &FirestoreDb,
    nearby_id: &str,
    nearby: &ShelterDoc,
    distance_m: f64,
) -> Result<ApiError, ApiError> {
    let creator: Option<VolunteerDoc> =
        store::get(db, volunteers::COLLECTION, &nearby.created_by).await?;
    let first_name = creator
        .as_ref()
        .and_then(|c| c.name.split_whitespace().next())
        .unwrap_or_default();
    let mut contact = json!({ "first_name": first_name });
    if let Some(creator) = creator.as_ref().filter(|c| c.show_phone) {
        contact["phone"] = json!(creator.phone);
    }
    let mut error = ApiError::new(StatusCode::CONFLICT, "shelter_nearby");
    error.detail = json!({
        "shelter_id": nearby_id,
        "name": nearby.name,
        "distance_m": distance_m.round(),
        "contact": contact,
    });
    Ok(error)
}

/// Everything creation writes, in one transaction that also does the 100 m search (spec 3.2.7).
async fn create_in(
    db: &FirestoreDb,
    tx: &mut firestore::FirestoreTransaction<'_>,
    creator_id: &str,
    new: &NewShelter,
    radius_m: f64,
    event_radius_km: u32,
) -> Result<Created, ApiError> {
    // The same request again (a lost response, a retry) gets the same answer.
    if let Some(existing) = store::get::<ShelterDoc>(db, COLLECTION, &new.id).await? {
        if existing.created_by != creator_id {
            return Err(ApiError::new(StatusCode::CONFLICT, "shelter_exists"));
        }
        let (event_id, event) = event_of(db, &existing)
            .await?
            .ok_or_else(ApiError::internal)?;
        return Ok(Created {
            shelter: existing,
            event_id,
            event,
        });
    }

    let near = geo::open_shelters_within(db, new.lat, new.lon, radius_m).await?;
    let (mut flagged, mut conflicts_with, mut other_site_reason) = (false, None, None);
    if let Some(nearest) = near.first() {
        let nearest_id = nearest.shelter.id.clone().unwrap_or_default();
        let Some(reason) = &new.other_site_reason else {
            return Err(conflict(db, &nearest_id, &nearest.shelter, nearest.distance_m).await?);
        };
        (flagged, conflicts_with, other_site_reason) =
            (true, Some(nearest_id), Some(reason.clone()));
    }

    let now = store::now();
    let assignment =
        events::assign(db, new.lat, new.lon, &new.address, event_radius_km, now).await?;
    let shelter = ShelterDoc {
        id: None,
        name: new.name.clone(),
        lat: new.lat,
        lon: new.lon,
        accuracy_m: new.accuracy_m,
        geohash: geo::encode(new.lat, new.lon, geo::STORED_PRECISION),
        address: new.address.clone(),
        event_id: Some(assignment.event_id.clone()),
        capacity: new.capacity,
        created_by: creator_id.into(),
        client_created_at: new.client_created_at,
        created_at: now,
        status: ShelterStatus::Open,
        confirmation: Confirmation::Unconfirmed,
        confirmation_type: None,
        can_endorse: false,
        endorsements: vec![],
        revocations: vec![],
        flagged,
        other_site_reason,
        conflicts_with: conflicts_with.clone(),
        items: Default::default(),
    };

    tx_create(db, tx, COLLECTION, &new.id, &shelter)?;
    tx_create(
        db,
        tx,
        members::COLLECTION,
        &members::membership_id(&new.id, creator_id),
        &MembershipDoc::creator(&new.id, creator_id, now),
    )?;
    let event = match assignment.new_event {
        Some(event) => {
            tx_create(db, tx, events::COLLECTION, &assignment.event_id, &event)?;
            event
        }
        None => {
            let mut event: EventDoc = store::get(db, events::COLLECTION, &assignment.event_id)
                .await?
                .ok_or_else(ApiError::internal)?;
            // The event was named before any shelter had an address; this one has it.
            if let Some(name) = event.renamed_for(&new.address) {
                event.name = name;
                tx_replace(db, tx, events::COLLECTION, &assignment.event_id, &event)?;
            }
            event
        }
    };
    if flagged {
        alerts::add(
            db,
            tx,
            &AlertDoc::new(
                AlertKind::ShelterFlagged,
                "shelter",
                &new.id,
                json!({ "conflicts_with": conflicts_with, "other_site_reason": shelter.other_site_reason }),
            )
            .in_event(&assignment.event_id),
        )?;
    }
    if assignment.in_range.len() > 1 {
        alerts::add(
            db,
            tx,
            &AlertDoc::new(
                AlertKind::EventOverlap,
                "shelter",
                &new.id,
                json!({ "event_ids": assignment.in_range }),
            )
            .in_event(&assignment.event_id),
        )?;
    }
    audit::add(
        db,
        tx,
        &AuditEntry::new("shelter.create", "shelter", &new.id)
            .by(creator_id)
            .in_shelter(&new.id),
    )?;
    Ok(Created {
        shelter,
        event_id: assignment.event_id,
        event,
    })
}

pub async fn post_shelters(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Json(request): Json<CreateShelter>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let new = validate(request)?;
    let radius_m = f64::from(state.config.shelter_radius_m);
    let event_radius_km = state.config.event_radius_km;
    let created = {
        // ponytail: global lock; fine with the single e2-micro instance. With more than one
        // instance, switch to a transaction on a per-geohash-cell lock document. The
        // transaction below already reads the neighbours itself, which is what a second
        // instance would still need.
        let _one_at_a_time = state.shelter_lock.lock().await;
        store::transact(&state.db, |db, tx| {
            let (creator_id, new) = (volunteer.id.clone(), new.clone());
            Box::pin(async move {
                create_in(&db, tx, &creator_id, &new, radius_m, event_radius_km).await
            })
        })
        .await?
    };
    events::touch(&state, Some(&created.event_id)).await;
    let body = full_view(
        &new.id,
        &created.shelter,
        Some((&created.event_id, &created.event)),
        Some(ShelterRole::Admin),
        0,
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({ "shelter": body, "event": created.event.view(&created.event_id) })),
    ))
}

// ---- GET, PATCH, close ----

fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found")
}

async fn load(db: &FirestoreDb, id: &str) -> Result<ShelterDoc, ApiError> {
    store::get(db, COLLECTION, id).await?.ok_or_else(not_found)
}

pub async fn get_shelter(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let shelter = load(&state.db, &id).await?;
    Ok(Json(
        json!({ "shelter": view_for(&state.db, &volunteer, &id, &shelter).await? }),
    ))
}

#[derive(Debug, Clone, Deserialize)]
pub struct ShelterPatch {
    name: Option<String>,
    address: Option<AddressInput>,
    /// `null` clears the capacity.
    #[serde(default, deserialize_with = "volunteers::present")]
    capacity: Option<Option<u32>>,
}

pub async fn patch_shelter(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(id): Path<String>,
    Json(patch): Json<ShelterPatch>,
) -> Result<Json<Value>, ApiError> {
    // Name and address: the admin. Capacity: the admin or the Civil Defense (matrix 2.3).
    let edits_details = patch.name.is_some() || patch.address.is_some();
    let is_admin = volunteer.is_admin_of(&id);
    let civil_defense = volunteer.has_global(GlobalRole::CivilDefense);
    if !is_admin && (edits_details || !civil_defense) {
        return Err(forbidden());
    }
    events::ensure_shelter_open(&state.db, &id).await?;
    let name = patch.name.as_deref().map(valid_name).transpose()?;
    let address = match &patch.address {
        Some(input) => Some(Address::from_input(input)?.ok_or_else(|| invalid("address"))?),
        None => None,
    };
    let capacity = match patch.capacity {
        Some(Some(value)) => Some(Some(valid_capacity(value)?)),
        other => other,
    };

    let (shelter, event) = store::transact(&state.db, |db, tx| {
        let (id, name, address, volunteer) =
            (id.clone(), name.clone(), address.clone(), volunteer.clone());
        Box::pin(async move {
            let mut shelter = load(&db, &id).await?;
            let mut event = event_of(&db, &shelter).await?;
            if let Some(name) = name {
                shelter.name = name;
            }
            if let Some(capacity) = capacity {
                shelter.capacity = capacity;
            }
            if let Some(address) = address {
                shelter.address = address;
                // The first address a shelter of the event gets can name the event (spec 4.3.1).
                if let Some((event_id, event)) = event.as_mut() {
                    if let Some(name) = event.renamed_for(&shelter.address) {
                        event.name = name;
                        tx_replace(&db, tx, events::COLLECTION, event_id, event)?;
                    }
                }
            }
            tx_replace(&db, tx, COLLECTION, &id, &shelter)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new("shelter.update", "shelter", &id)
                    .by(&volunteer.id)
                    .in_shelter(&id),
            )?;
            Ok((shelter, event))
        })
    })
    .await?;
    events::touch(&state, shelter.event_id.as_deref()).await;
    Ok(Json(
        json!({ "shelter": respond(&state.db, &id, &shelter, event.as_ref(), &volunteer).await? }),
    ))
}

/// The full view for a caller who is allowed to change the shelter.
async fn respond(
    db: &FirestoreDb,
    id: &str,
    shelter: &ShelterDoc,
    event: Option<&(String, EventDoc)>,
    volunteer: &Volunteer,
) -> Result<Value, ApiError> {
    Ok(full_view(
        id,
        shelter,
        event.map(|(event_id, event)| (event_id.as_str(), event)),
        volunteer.role_in(id),
        exits::occupancy(db, id).await?,
    ))
}

/// What `update` does to a shelter.
#[derive(Clone)]
enum Change {
    Close,
    Confirm,
    Revoke { by: String, reason: String },
}

impl Change {
    fn action(&self) -> &'static str {
        match self {
            Self::Close => "shelter.close",
            Self::Confirm => "shelter.confirm",
            Self::Revoke { .. } => "shelter.revoke",
        }
    }

    /// Edits the shelter; `false` means it was already in that state, so nothing is written.
    async fn apply(
        &self,
        db: &FirestoreDb,
        tx: &mut firestore::FirestoreTransaction<'_>,
        id: &str,
        shelter: &mut ShelterDoc,
    ) -> Result<bool, ApiError> {
        match self {
            Self::Close => {
                let changed = shelter.status != ShelterStatus::Closed;
                shelter.status = ShelterStatus::Closed;
                Ok(changed)
            }
            Self::Confirm => {
                if shelter.confirmation == Confirmation::Confirmed {
                    return Ok(false);
                }
                shelter.confirm(ConfirmationType::Validator);
                Ok(true)
            }
            Self::Revoke { by, reason } => {
                if shelter.confirmation == Confirmation::Unconfirmed {
                    return Ok(false);
                }
                let since = shelter.revocations.last().map(|r| r.at);
                for endorsement in flag_endorsers(shelter, since) {
                    flag_endorser(db, tx, id, reason, endorsement).await?;
                }
                shelter.confirmation = Confirmation::Unconfirmed;
                shelter.confirmation_type = None;
                shelter.can_endorse = false;
                shelter.revocations.push(Revocation {
                    by: by.clone(),
                    at: store::now(),
                    reason: reason.clone(),
                });
                Ok(true)
            }
        }
    }
}

/// Reads the shelter, applies the change and writes it back with an audit entry.
async fn update(
    state: &AppState,
    volunteer: &Volunteer,
    id: &str,
    change: Change,
) -> Result<Json<Value>, ApiError> {
    let (shelter, event) = store::transact(&state.db, |db, tx| {
        let (id, by, change) = (id.to_string(), volunteer.id.clone(), change.clone());
        Box::pin(async move {
            let mut shelter = load(&db, &id).await?;
            if change.apply(&db, tx, &id, &mut shelter).await? {
                tx_replace(&db, tx, COLLECTION, &id, &shelter)?;
                audit::add(
                    &db,
                    tx,
                    &AuditEntry::new(change.action(), "shelter", &id)
                        .by(&by)
                        .in_shelter(&id),
                )?;
            }
            let event = event_of(&db, &shelter).await?;
            Ok((shelter, event))
        })
    })
    .await?;
    events::touch(state, shelter.event_id.as_deref()).await;
    Ok(Json(
        json!({ "shelter": respond(&state.db, id, &shelter, event.as_ref(), volunteer).await? }),
    ))
}

pub async fn close_shelter(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !volunteer.is_admin_of(&id) && !volunteer.has_global(GlobalRole::CivilDefense) {
        return Err(forbidden());
    }
    update(&state, &volunteer, &id, Change::Close).await
}

// ---- confirmation by a validator (spec 3.9) ----

pub async fn confirm_shelter(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !volunteer.is_validator_or_civil_defense() {
        return Err(forbidden());
    }
    update(&state, &volunteer, &id, Change::Confirm).await
}

#[derive(Debug, Deserialize)]
pub struct Revoke {
    reason: String,
}

/// The endorsements the revoked confirmation rested on, one per volunteer. Older ones were
/// flagged by an earlier revocation.
fn flag_endorsers(shelter: &ShelterDoc, since: Option<FirestoreInstant>) -> Vec<&Endorsement> {
    let mut seen = Vec::new();
    shelter
        .endorsements
        .iter()
        .filter(|e| since.is_none_or(|since| e.at > since))
        .filter(|e| {
            let first = !seen.contains(&&e.volunteer_id);
            seen.push(&e.volunteer_id);
            first
        })
        .collect()
}

/// Flags the volunteer who endorsed the revoked shelter, and alerts their admins and the
/// Civil Defense (spec 3.9).
async fn flag_endorser(
    db: &FirestoreDb,
    tx: &mut firestore::FirestoreTransaction<'_>,
    revoked_shelter_id: &str,
    reason: &str,
    endorsement: &Endorsement,
) -> Result<(), ApiError> {
    let endorser = &endorsement.volunteer_id;
    if let Some(mut doc) = store::get::<VolunteerDoc>(db, volunteers::COLLECTION, endorser).await? {
        doc.flagged = true;
        doc.flag_reasons
            .push(format!("endorsed_revoked_shelter:{revoked_shelter_id}"));
        tx_replace(db, tx, volunteers::COLLECTION, endorser, &doc)?;
    }
    alerts::add(
        db,
        tx,
        &AlertDoc::new(
            AlertKind::EndorserFlagged,
            "volunteer",
            endorser,
            json!({
                "revoked_shelter_id": revoked_shelter_id,
                "endorser_shelter_id": endorsement.endorser_shelter_id,
                "reason": reason,
            }),
        )
        .about_volunteers(vec![endorser.clone()]),
    )
}

pub async fn revoke_shelter(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(id): Path<String>,
    Json(request): Json<Revoke>,
) -> Result<Json<Value>, ApiError> {
    if !volunteer.is_validator_or_civil_defense() {
        return Err(forbidden());
    }
    let reason = request.reason.trim().to_string();
    if reason.is_empty() || reason.chars().count() > MAX_REASON_CHARS {
        return Err(invalid("reason"));
    }
    update(
        &state,
        &volunteer,
        &id,
        Change::Revoke {
            by: volunteer.id.clone(),
            reason,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        ORIGIN, TestApp, all_docs, audit_actions, new_id, north_of_origin, shelter_body,
    };

    async fn post(
        app: &TestApp,
        who: &crate::test_support::Actor,
        body: Value,
    ) -> (StatusCode, Value) {
        app.send("POST", "/v1/shelters", &who.secret, Some(body))
            .await
    }

    fn error_of(body: &Value) -> Option<&str> {
        body["error"].as_str()
    }

    async fn shelters_in_db(app: &TestApp) -> usize {
        all_docs(&app.state.db, COLLECTION).await.len()
    }

    // ---- validation ----

    fn request(edit: impl FnOnce(&mut Value)) -> Result<NewShelter, ApiError> {
        let mut body = shelter_body(&new_id(), "Ginásio", ORIGIN);
        edit(&mut body);
        validate(serde_json::from_value(body).unwrap())
    }

    fn field_of(error: &ApiError) -> Option<&str> {
        error.detail["field"].as_str()
    }

    #[test]
    fn a_valid_request_is_trimmed_and_defaults_the_address_to_pending() {
        let shelter = request(|b| b["name"] = json!("  Ginásio  ")).unwrap();
        assert_eq!(shelter.name, "Ginásio");
        assert!(shelter.address.pending && shelter.capacity.is_none());
        let with_address = request(|b| {
            b["address"] = json!({ "text": " Rua A, 1 ", "city": "Canoas", "state": "RS" });
            b["capacity"] = json!(200);
        })
        .unwrap();
        assert_eq!(with_address.address.city.as_deref(), Some("Canoas"));
        assert!(!with_address.address.pending);
        assert_eq!(with_address.capacity, Some(200));
        // An address with nothing in it is still pending.
        assert!(
            request(|b| b["address"] = json!({ "text": "  " }))
                .unwrap()
                .address
                .pending
        );
    }

    #[test]
    fn invalid_requests_name_the_field() {
        type Edit = Box<dyn Fn(&mut Value)>;
        let cases: Vec<(&str, Edit)> = vec![
            ("id", Box::new(|b| b["id"] = json!("not-a-uuid"))),
            ("name", Box::new(|b| b["name"] = json!("   "))),
            ("name", Box::new(|b| b["name"] = json!("x".repeat(101)))),
            ("position", Box::new(|b| b["lat"] = json!(91.0))),
            ("position", Box::new(|b| b["lon"] = json!(-181.0))),
            ("accuracy", Box::new(|b| b["accuracy"] = json!(-1.0))),
            ("capacity", Box::new(|b| b["capacity"] = json!(0))),
            ("capacity", Box::new(|b| b["capacity"] = json!(100_001))),
            (
                "client_created_at",
                Box::new(|b| b["client_created_at"] = json!("yesterday")),
            ),
            (
                "other_site_reason",
                Box::new(|b| b["other_site_reason"] = json!("  ")),
            ),
            (
                "address",
                Box::new(|b| b["address"] = json!({ "city": "x".repeat(101) })),
            ),
        ];
        for (field, edit) in cases {
            let error = request(edit).unwrap_err();
            assert_eq!(
                (error.status, error.code, field_of(&error)),
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_shelter",
                    Some(field)
                )
            );
        }
    }

    #[test]
    fn accuracy_over_100_metres_is_refused_and_100_is_accepted() {
        assert!(request(|b| b["accuracy"] = json!(100.0)).is_ok());
        let error = request(|b| b["accuracy"] = json!(101.0)).unwrap_err();
        assert_eq!(
            (error.status, error.code),
            (StatusCode::UNPROCESSABLE_ENTITY, "insufficient_accuracy")
        );
        assert_eq!(error.detail["max"], 100.0);
    }

    // ---- creation ----

    #[tokio::test]
    async fn creating_a_shelter_makes_the_creator_its_admin_with_an_event_and_an_audit_entry() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let id = new_id();
        let mut body = shelter_body(&id, "Ginásio", ORIGIN);
        body["capacity"] = json!(150);
        let (status, created) = post(&app, &ana, body).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");

        let shelter = &created["shelter"];
        assert_eq!(
            (
                &shelter["id"],
                &shelter["name"],
                &shelter["status"],
                &shelter["confirmation"],
                &shelter["can_endorse"],
                &shelter["flagged"],
                &shelter["role"],
                &shelter["capacity"],
                &shelter["vacancies"]
            ),
            (
                &json!(id),
                &json!("Ginásio"),
                &json!("open"),
                &json!("unconfirmed"),
                &json!(false),
                &json!(false),
                &json!("admin"),
                &json!(150),
                &json!(150)
            )
        );
        assert_eq!(shelter["address"], json!({ "pending": true }));
        assert_eq!(created["event"]["id"], shelter["event_id"]);
        assert!(
            created["event"]["name"]
                .as_str()
                .unwrap()
                .starts_with("Evento – ")
        );

        // Stored as the spec lists it: nine-character geohash, creator, membership as admin.
        let stored: ShelterDoc = store::get(&app.state.db, COLLECTION, &id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.geohash, geo::encode(ORIGIN.0, ORIGIN.1, 9));
        assert_eq!(
            (stored.created_by.as_str(), stored.accuracy_m),
            (ana.id.as_str(), 10.0)
        );
        let (_, me) = app.get_authed("/v1/me", &ana.secret).await;
        assert_eq!(
            me["memberships"],
            json!([{ "shelter_id": id, "name": "Ginásio", "role": "admin", "confirmation": "unconfirmed" }])
        );
        assert!(
            audit_actions(&app.state.db)
                .await
                .contains(&"shelter.create".to_string())
        );
    }

    #[tokio::test]
    async fn creating_needs_a_session() {
        let app = TestApp::new().await;
        let (status, _, _) = app
            .request(
                "POST",
                "/v1/shelters",
                &[],
                Some(shelter_body(&new_id(), "X", ORIGIN)),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn accuracy_101_is_refused_with_nothing_written() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let mut body = shelter_body(&new_id(), "Ginásio", ORIGIN);
        body["accuracy"] = json!(101.0);
        let (status, answer) = post(&app, &ana, body).await;
        assert_eq!(
            (status, error_of(&answer)),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("insufficient_accuracy")
            )
        );
        assert_eq!(shelters_in_db(&app).await, 0);
        let mut ok = shelter_body(&new_id(), "Ginásio", ORIGIN);
        ok["accuracy"] = json!(100.0);
        assert_eq!(post(&app, &ana, ok).await.0, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn a_shelter_80_metres_away_is_refused_naming_the_creator_but_not_the_phone() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana Souza").await;
        let first = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let bia = app.actor("Bia").await;
        let (status, answer) = post(
            &app,
            &bia,
            shelter_body(&new_id(), "Escola", north_of_origin(80.0)),
        )
        .await;
        assert_eq!(
            (status, error_of(&answer)),
            (StatusCode::CONFLICT, Some("shelter_nearby"))
        );
        let detail = &answer["detail"];
        assert_eq!(
            (
                &detail["shelter_id"],
                &detail["name"],
                &detail["contact"]["first_name"]
            ),
            (&json!(first), &json!("Ginásio"), &json!("Ana"))
        );
        assert!((detail["distance_m"].as_f64().unwrap() - 80.0).abs() <= 1.0);
        assert!(detail["contact"].get("phone").is_none());
        assert_eq!(shelters_in_db(&app).await, 1);
    }

    #[tokio::test]
    async fn the_phone_is_shown_only_when_the_creator_allowed_it() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        app.send(
            "PATCH",
            "/v1/me",
            &ana.secret,
            Some(json!({ "phone": "+55 51 99999-0000", "show_phone": true })),
        )
        .await;
        app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let bia = app.actor("Bia").await;
        let (_, answer) = post(
            &app,
            &bia,
            shelter_body(&new_id(), "Escola", north_of_origin(80.0)),
        )
        .await;
        assert_eq!(
            answer["detail"]["contact"],
            json!({ "first_name": "Ana", "phone": "+55 51 99999-0000" })
        );

        // Hidden again once she changes her mind.
        app.send(
            "PATCH",
            "/v1/me",
            &ana.secret,
            Some(json!({ "show_phone": false })),
        )
        .await;
        let (_, answer) = post(
            &app,
            &bia,
            shelter_body(&new_id(), "Escola", north_of_origin(80.0)),
        )
        .await;
        assert_eq!(answer["detail"]["contact"], json!({ "first_name": "Ana" }));
    }

    #[tokio::test]
    async fn the_radius_comes_from_the_configuration_and_a_far_shelter_is_not_a_neighbour() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let bia = app.actor("Bia").await;
        assert_eq!(
            post(
                &app,
                &bia,
                shelter_body(&new_id(), "Escola", north_of_origin(101.0))
            )
            .await
            .0,
            StatusCode::CREATED
        );
        assert_eq!(
            post(
                &app,
                &bia,
                shelter_body(&new_id(), "Igreja", north_of_origin(99.0))
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn an_explanation_creates_the_shelter_flagged_and_alerts_the_civil_defense() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let first = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let bia = app.actor("Bia").await;
        let id = new_id();
        let mut body = shelter_body(&id, "Escola", north_of_origin(80.0));
        body["other_site_reason"] = json!("Outro prédio, do outro lado da rua");
        let (status, created) = post(&app, &bia, body).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["shelter"]["flagged"], true);

        let stored: ShelterDoc = store::get(&app.state.db, COLLECTION, &id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.conflicts_with.as_deref(), Some(first.as_str()));
        assert_eq!(
            stored.other_site_reason.as_deref(),
            Some("Outro prédio, do outro lado da rua")
        );

        let alerts = all_docs(&app.state.db, "alerts").await;
        assert_eq!(alerts.len(), 1);
        assert_eq!(
            (
                &alerts[0]["kind"],
                &alerts[0]["target_id"],
                &alerts[0]["for_civil_defense"],
                &alerts[0]["informational"]
            ),
            (
                &json!("shelter_flagged"),
                &json!(id),
                &json!(true),
                &json!(false)
            )
        );
        assert_eq!(alerts[0]["detail"]["conflicts_with"], json!(first));
        // The count the dashboard will make (phase 6) is a query on `flagged`.
        let flagged: Vec<ShelterDoc> = app
            .state
            .db
            .fluent()
            .select()
            .from(COLLECTION)
            .filter(|q| q.for_all([q.field(firestore::path!(ShelterDoc::flagged)).eq(true)]))
            .obj()
            .query()
            .await
            .unwrap();
        assert_eq!(flagged.len(), 1);
    }

    #[tokio::test]
    async fn an_explanation_with_no_neighbour_creates_an_ordinary_shelter() {
        // Owner's decision: the reason only applies to a conflict, so with nothing nearby the
        // shelter is normal, not flagged, and raises no alert.
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let mut body = shelter_body(&new_id(), "Ginásio", ORIGIN);
        body["other_site_reason"] = json!("só por garantia");
        let (_, created) = post(&app, &ana, body).await;
        assert_eq!(created["shelter"]["flagged"], false);
        assert!(all_docs(&app.state.db, "alerts").await.is_empty());
    }

    #[tokio::test]
    async fn a_closed_shelter_is_not_a_neighbour() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let first = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let (status, _) = app
            .send(
                "POST",
                &format!("/v1/shelters/{first}/close"),
                &ana.secret,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let bia = app.actor("Bia").await;
        assert_eq!(
            post(
                &app,
                &bia,
                shelter_body(&new_id(), "Escola", north_of_origin(50.0))
            )
            .await
            .0,
            StatusCode::CREATED
        );
    }

    #[tokio::test]
    async fn sending_the_same_creation_again_gives_the_same_answer_and_writes_nothing() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let id = new_id();
        let body = shelter_body(&id, "Ginásio", ORIGIN);
        let (status, first) = post(&app, &ana, body.clone()).await;
        assert_eq!(status, StatusCode::CREATED);
        let audits = audit_actions(&app.state.db).await.len();
        let (status, again) = post(&app, &ana, body.clone()).await;
        assert_eq!((status, &again), (StatusCode::CREATED, &first));
        assert_eq!(shelters_in_db(&app).await, 1);
        assert_eq!(audit_actions(&app.state.db).await.len(), audits);

        // Someone else cannot take over an id.
        let bia = app.actor("Bia").await;
        let (status, answer) = post(&app, &bia, body).await;
        assert_eq!(
            (status, error_of(&answer)),
            (StatusCode::CONFLICT, Some("shelter_exists"))
        );
    }

    // ---- 12: concurrent creations ----

    #[tokio::test]
    async fn two_simultaneous_creations_50_metres_apart_make_only_one_shelter() {
        let app = TestApp::new().await;
        let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
        let (a, b) = tokio::join!(
            post(&app, &ana, shelter_body(&new_id(), "Ginásio", ORIGIN)),
            post(
                &app,
                &bia,
                shelter_body(&new_id(), "Escola", north_of_origin(50.0))
            ),
        );
        let mut statuses = [a.0, b.0];
        statuses.sort();
        assert_eq!(statuses, [StatusCode::CREATED, StatusCode::CONFLICT]);
        assert_eq!(shelters_in_db(&app).await, 1);
        assert_eq!(all_docs(&app.state.db, "memberships").await.len(), 1);
    }

    #[tokio::test]
    async fn many_simultaneous_creations_within_the_radius_make_exactly_one() {
        let app = TestApp::new().await;
        let mut actors = Vec::new();
        for n in 0..8 {
            actors.push(app.actor(&format!("V{n}")).await);
        }
        let handles: Vec<_> = actors
            .iter()
            .enumerate()
            .map(|(n, who)| {
                let (app, who) = (app.clone(), who.clone());
                let body = shelter_body(
                    &new_id(),
                    &format!("Abrigo {n}"),
                    north_of_origin(10.0 * n as f64),
                );
                tokio::spawn(async move { post(&app, &who, body).await })
            })
            .collect();
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.unwrap());
        }
        let created = results
            .iter()
            .filter(|r| r.0 == StatusCode::CREATED)
            .count();
        let refused = results
            .iter()
            .filter(|r| r.0 == StatusCode::CONFLICT)
            .count();
        assert_eq!((created, refused), (1, 7), "{results:?}");
        assert_eq!(shelters_in_db(&app).await, 1);
    }

    #[tokio::test]
    async fn the_transaction_alone_also_refuses_a_second_shelter_next_to_the_first() {
        // Without the process lock, as with more than one instance: the neighbours are read
        // inside the transaction, so a creation that raced loses and runs again.
        let app = TestApp::new().await;
        let db = app.state.db.clone();
        let mut handles = Vec::new();
        for n in 0..6 {
            let db = db.clone();
            handles.push(tokio::spawn(async move {
                let new = request_for(&format!("Abrigo {n}"), north_of_origin(10.0 * f64::from(n)));
                let creator = format!("creator-{n}");
                store::transact(&db, |db, tx| {
                    let (creator, new) = (creator.clone(), new.clone());
                    Box::pin(async move {
                        create_in(&db, tx, &creator, &new, 100.0, 50)
                            .await
                            .map(|c| c.event_id)
                    })
                })
                .await
            }));
        }
        let mut created = 0;
        for handle in handles {
            match handle.await.unwrap() {
                Ok(_) => created += 1,
                Err(e) => assert_eq!(e.code, "shelter_nearby", "{e}"),
            }
        }
        assert_eq!(created, 1);
    }

    fn request_for(name: &str, at: (f64, f64)) -> NewShelter {
        validate(serde_json::from_value(shelter_body(&new_id(), name, at)).unwrap()).unwrap()
    }

    // ---- 10: events ----

    async fn event_of_shelter(app: &TestApp, id: &str) -> String {
        let stored: ShelterDoc = store::get(&app.state.db, COLLECTION, id)
            .await
            .unwrap()
            .unwrap();
        stored.event_id.unwrap()
    }

    #[tokio::test]
    async fn a_shelter_40_km_away_joins_the_event() {
        let app = TestApp::new().await;
        let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
        let first = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let near = app
            .create_shelter(&bia, "Escola", north_of_origin(40_000.0))
            .await;
        assert_eq!(
            event_of_shelter(&app, &near).await,
            event_of_shelter(&app, &first).await
        );
        assert_eq!(all_docs(&app.state.db, "events").await.len(), 1);
        assert!(all_docs(&app.state.db, "alerts").await.is_empty());
    }

    #[tokio::test]
    async fn a_shelter_60_km_away_starts_another_event() {
        let app = TestApp::new().await;
        let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
        let first = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let far = app
            .create_shelter(&bia, "Igreja", north_of_origin(60_000.0))
            .await;
        assert_ne!(
            event_of_shelter(&app, &far).await,
            event_of_shelter(&app, &first).await
        );
        assert_eq!(all_docs(&app.state.db, "events").await.len(), 2);
        assert!(all_docs(&app.state.db, "alerts").await.is_empty());
    }

    #[tokio::test]
    async fn reaching_two_events_joins_the_nearest_shelters_event_and_raises_an_overlap() {
        let app = TestApp::new().await;
        let (ana, bia, cai) = (
            app.actor("Ana").await,
            app.actor("Bia").await,
            app.actor("Cai").await,
        );
        let a = app.create_shelter(&ana, "A", ORIGIN).await;
        let b = app
            .create_shelter(&bia, "B", north_of_origin(70_000.0))
            .await;
        let (event_a, event_b) = (
            event_of_shelter(&app, &a).await,
            event_of_shelter(&app, &b).await,
        );
        assert_ne!(event_a, event_b);

        // 30 km from A and 40 km from B: both events are in range, A's shelter is nearer.
        let c = app
            .create_shelter(&cai, "C", north_of_origin(30_000.0))
            .await;
        assert_eq!(event_of_shelter(&app, &c).await, event_a);
        let alerts = all_docs(&app.state.db, "alerts").await;
        assert_eq!(alerts.len(), 1);
        assert_eq!(
            (
                &alerts[0]["kind"],
                &alerts[0]["target_id"],
                &alerts[0]["event_id"],
                &alerts[0]["for_civil_defense"]
            ),
            (
                &json!("event_overlap"),
                &json!(c),
                &json!(event_a),
                &json!(true)
            )
        );
        let mut in_range: Vec<String> =
            serde_json::from_value(alerts[0]["detail"]["event_ids"].clone()).unwrap();
        in_range.sort();
        let mut expected = vec![event_a.clone(), event_b.clone()];
        expected.sort();
        assert_eq!(in_range, expected);
        // Nothing is merged: both events stay active and untouched.
        let events = all_docs(&app.state.db, "events").await;
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e["status"] == "active"));
    }

    #[tokio::test]
    async fn a_closed_shelter_does_not_bring_its_event_into_range() {
        let app = TestApp::new().await;
        let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
        let a = app.create_shelter(&ana, "A", ORIGIN).await;
        app.send(
            "POST",
            &format!("/v1/shelters/{a}/close"),
            &ana.secret,
            None,
        )
        .await;
        let b = app
            .create_shelter(&bia, "B", north_of_origin(10_000.0))
            .await;
        assert_ne!(
            event_of_shelter(&app, &a).await,
            event_of_shelter(&app, &b).await
        );
    }

    #[tokio::test]
    async fn the_first_address_names_the_event_and_later_ones_do_not() {
        let app = TestApp::new().await;
        let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
        let a = app.create_shelter(&ana, "A", ORIGIN).await;
        let b = app
            .create_shelter(&bia, "B", north_of_origin(1_000.0))
            .await;
        let event_id = event_of_shelter(&app, &a).await;
        let name_of = |app: &TestApp| {
            let (db, event_id) = (app.state.db.clone(), event_id.clone());
            async move {
                store::get::<EventDoc>(&db, events::COLLECTION, &event_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .name
            }
        };
        assert!(name_of(&app).await.starts_with("Evento – "));

        let address = json!({ "address": { "text": "Rua A, 1", "city": "Canoas", "state": "RS" } });
        let (status, body) = app
            .send(
                "PATCH",
                &format!("/v1/shelters/{a}"),
                &ana.secret,
                Some(address),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["shelter"]["address"],
            json!({ "text": "Rua A, 1", "city": "Canoas", "state": "RS", "pending": false })
        );
        assert!(name_of(&app).await.starts_with("Canoas/RS – "));
        assert_eq!(body["shelter"]["event"]["name"], json!(name_of(&app).await));

        let other = json!({ "address": { "city": "Esteio", "state": "RS" } });
        app.send(
            "PATCH",
            &format!("/v1/shelters/{b}"),
            &bia.secret,
            Some(other),
        )
        .await;
        assert!(
            name_of(&app).await.starts_with("Canoas/RS – "),
            "already named after a city"
        );
    }

    #[tokio::test]
    async fn an_event_renamed_by_a_person_keeps_its_name_when_an_address_arrives() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let a = app.create_shelter(&ana, "A", ORIGIN).await;
        let event_id = event_of_shelter(&app, &a).await;
        let (status, _) = app
            .send(
                "PATCH",
                &format!("/v1/events/{event_id}"),
                &ana.secret,
                Some(json!({ "name": "Cheia de outubro" })),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        app.send(
            "PATCH",
            &format!("/v1/shelters/{a}"),
            &ana.secret,
            Some(json!({ "address": { "city": "Canoas", "state": "RS" } })),
        )
        .await;
        let event: EventDoc = store::get(&app.state.db, events::COLLECTION, &event_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (event.name.as_str(), event.auto_named),
            ("Cheia de outubro", false)
        );
    }

    #[tokio::test]
    async fn writes_to_a_shelter_record_the_event_activity() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let a = app.create_shelter(&ana, "A", ORIGIN).await;
        let event_id = event_of_shelter(&app, &a).await;
        // Age the stored time and forget the cache, as an hour later would.
        let mut event: EventDoc = store::get(&app.state.db, events::COLLECTION, &event_id)
            .await
            .unwrap()
            .unwrap();
        let old = event.last_activity_at - firestore::jiff::SignedDuration::from_hours(48);
        event.last_activity_at = old;
        store::replace(&app.state.db, events::COLLECTION, &event_id, &event)
            .await
            .unwrap();
        app.state.event_activity.lock().unwrap().clear();

        app.send(
            "PATCH",
            &format!("/v1/shelters/{a}"),
            &ana.secret,
            Some(json!({ "capacity": 50 })),
        )
        .await;
        let after: EventDoc = store::get(&app.state.db, events::COLLECTION, &event_id)
            .await
            .unwrap()
            .unwrap();
        assert!(after.last_activity_at > old + firestore::jiff::SignedDuration::from_hours(47));
        // The name was not touched by the activity write.
        assert_eq!(after.name, event.name);

        // Within the hour a second write leaves it alone.
        let marker = after.last_activity_at;
        app.send(
            "PATCH",
            &format!("/v1/shelters/{a}"),
            &ana.secret,
            Some(json!({ "capacity": 60 })),
        )
        .await;
        let again: EventDoc = store::get(&app.state.db, events::COLLECTION, &event_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again.last_activity_at, marker);
    }

    // ---- who sees and changes what ----

    #[tokio::test]
    async fn members_validators_and_the_civil_defense_see_all_and_others_see_the_basics() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let id = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let stranger = app.actor("Ze").await;
        let validator = app.actor("Val").await;
        app.grant(&validator, crate::volunteers::GlobalRole::Validator)
            .await;

        let (status, own) = app
            .send("GET", &format!("/v1/shelters/{id}"), &ana.secret, None)
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            (&own["shelter"]["role"], &own["shelter"]["lat"]),
            (&json!("admin"), &json!(ORIGIN.0))
        );
        let (_, seen_by_validator) = app
            .send(
                "GET",
                &format!("/v1/shelters/{id}"),
                &validator.secret,
                None,
            )
            .await;
        assert_eq!(seen_by_validator["shelter"]["role"], Value::Null);
        assert_eq!(seen_by_validator["shelter"]["lat"], json!(ORIGIN.0));

        let (_, basics) = app
            .send("GET", &format!("/v1/shelters/{id}"), &stranger.secret, None)
            .await;
        assert_eq!(
            basics["shelter"],
            json!({ "id": id, "name": "Ginásio", "confirmation": "unconfirmed", "capacity": null, "occupancy": 0, "vacancies": null })
        );
        let (status, missing) = app
            .send(
                "GET",
                &format!("/v1/shelters/{}", new_id()),
                &ana.secret,
                None,
            )
            .await;
        assert_eq!(
            (status, error_of(&missing)),
            (StatusCode::NOT_FOUND, Some("shelter_not_found"))
        );
        assert_eq!(
            app.request("GET", &format!("/v1/shelters/{id}"), &[], None)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn members_see_the_items_and_strangers_do_not() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let stranger = app.actor("Ze").await;
        let id = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let path = format!("/v1/shelters/{id}");

        let (_, empty) = app.send("GET", &path, &ana.secret, None).await;
        assert_eq!(empty["shelter"]["items"], json!({}));

        let mut shelter: ShelterDoc = store::get(&app.state.db, COLLECTION, &id)
            .await
            .unwrap()
            .unwrap();
        shelter.items.insert(
            "cesta".into(),
            crate::deliveries::ItemSpec {
                scope: crate::deliveries::ItemScope::PerHousehold,
                period_days: Some(7),
                once_per_event: false,
                high_value: false,
            },
        );
        store::replace(&app.state.db, COLLECTION, &id, &shelter)
            .await
            .unwrap();

        let (_, own) = app.send("GET", &path, &ana.secret, None).await;
        assert_eq!(
            own["shelter"]["items"],
            json!({ "cesta": { "scope": "per_household", "period_days": 7, "once_per_event": false, "high_value": false } })
        );
        let (_, basics) = app.send("GET", &path, &stranger.secret, None).await;
        assert!(basics["shelter"].get("items").is_none());
    }

    #[tokio::test]
    async fn patch_follows_the_permission_matrix() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let id = app.create_shelter(&admin, "Ginásio", ORIGIN).await;
        let volunteer = app
            .join(&app.invite(&admin, &id, "qr", "volunteer").await, "Vol")
            .await;
        let civil_defense = app.actor("DC").await;
        app.grant(&civil_defense, crate::volunteers::GlobalRole::CivilDefense)
            .await;
        let validator = app.actor("Val").await;
        app.grant(&validator, crate::volunteers::GlobalRole::Validator)
            .await;
        let stranger = app.actor("Ze").await;
        let path = format!("/v1/shelters/{id}");

        let rename = json!({ "name": "Novo nome" });
        let capacity = json!({ "capacity": 80 });
        let expect = |who: &crate::test_support::Actor, body: &Value, status: StatusCode| {
            let (app, path, who, body) = (&app, path.clone(), who.clone(), body.clone());
            async move {
                let (got, answer) = app
                    .send("PATCH", &path, &who.secret, Some(body.clone()))
                    .await;
                assert_eq!(got, status, "{} {body}: {answer}", who.id);
            }
        };
        // Name and address: the admin only. Capacity: the admin and the Civil Defense.
        for who in [&volunteer, &civil_defense, &validator, &stranger] {
            expect(who, &rename, StatusCode::FORBIDDEN).await;
        }
        for who in [&volunteer, &validator, &stranger] {
            expect(who, &capacity, StatusCode::FORBIDDEN).await;
        }
        expect(&civil_defense, &capacity, StatusCode::OK).await;
        expect(
            &civil_defense,
            &json!({ "capacity": 90, "name": "x" }),
            StatusCode::FORBIDDEN,
        )
        .await;
        expect(&admin, &rename, StatusCode::OK).await;
        expect(&admin, &json!({ "capacity": 70 }), StatusCode::OK).await;
        let stored: ShelterDoc = store::get(&app.state.db, COLLECTION, &id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (stored.name.as_str(), stored.capacity),
            ("Novo nome", Some(70))
        );
    }

    #[tokio::test]
    async fn capacity_can_be_cleared_and_must_be_sensible() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let id = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let path = format!("/v1/shelters/{id}");
        let (_, set) = app
            .send(
                "PATCH",
                &path,
                &ana.secret,
                Some(json!({ "capacity": 200 })),
            )
            .await;
        assert_eq!(set["shelter"]["capacity"], 200);
        let (_, kept) = app
            .send(
                "PATCH",
                &path,
                &ana.secret,
                Some(json!({ "name": "Outro" })),
            )
            .await;
        assert_eq!(
            kept["shelter"]["capacity"], 200,
            "a key left out keeps the value"
        );
        let (_, cleared) = app
            .send(
                "PATCH",
                &path,
                &ana.secret,
                Some(json!({ "capacity": null })),
            )
            .await;
        assert_eq!(cleared["shelter"]["capacity"], Value::Null);
        for bad in [
            json!({ "capacity": 0 }),
            json!({ "name": " " }),
            json!({ "address": {} }),
        ] {
            let (status, answer) = app.send("PATCH", &path, &ana.secret, Some(bad)).await;
            assert_eq!(
                (status, error_of(&answer)),
                (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_shelter"))
            );
        }
    }

    #[tokio::test]
    async fn only_an_admin_or_the_civil_defense_closes_a_shelter() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let id = app.create_shelter(&admin, "Ginásio", ORIGIN).await;
        let volunteer = app
            .join(&app.invite(&admin, &id, "qr", "volunteer").await, "Vol")
            .await;
        let validator = app.actor("Val").await;
        app.grant(&validator, crate::volunteers::GlobalRole::Validator)
            .await;
        let civil_defense = app.actor("DC").await;
        app.grant(&civil_defense, crate::volunteers::GlobalRole::CivilDefense)
            .await;
        let path = format!("/v1/shelters/{id}/close");
        for who in [&volunteer, &validator] {
            assert_eq!(
                app.send("POST", &path, &who.secret, None).await.0,
                StatusCode::FORBIDDEN
            );
        }
        let (status, closed) = app.send("POST", &path, &civil_defense.secret, None).await;
        assert_eq!(
            (status, &closed["shelter"]["status"]),
            (StatusCode::OK, &json!("closed"))
        );
        // Closing again is harmless and not audited twice.
        let audits = audit_actions(&app.state.db)
            .await
            .iter()
            .filter(|a| *a == "shelter.close")
            .count();
        assert_eq!(
            app.send("POST", &path, &admin.secret, None).await.0,
            StatusCode::OK
        );
        assert_eq!(
            audit_actions(&app.state.db)
                .await
                .iter()
                .filter(|a| *a == "shelter.close")
                .count(),
            audits
        );
    }

    #[tokio::test]
    async fn only_a_validator_or_the_civil_defense_confirms() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let id = app.create_shelter(&admin, "Ginásio", ORIGIN).await;
        let volunteer = app
            .join(&app.invite(&admin, &id, "qr", "volunteer").await, "Vol")
            .await;
        let mediator = app.actor("Med").await;
        app.grant(&mediator, crate::volunteers::GlobalRole::Mediator)
            .await;
        let path = format!("/v1/shelters/{id}/confirm");
        for who in [&admin, &volunteer, &mediator] {
            assert_eq!(
                app.send("POST", &path, &who.secret, None).await.0,
                StatusCode::FORBIDDEN,
                "{}",
                who.id
            );
        }
        let validator = app.actor("Val").await;
        app.grant(&validator, crate::volunteers::GlobalRole::Validator)
            .await;
        let (status, body) = app.send("POST", &path, &validator.secret, None).await;
        assert_eq!(status, StatusCode::OK);
        let shelter = &body["shelter"];
        assert_eq!(
            (
                &shelter["confirmation"],
                &shelter["confirmation_type"],
                &shelter["can_endorse"]
            ),
            (&json!("confirmed"), &json!("validator"), &json!(true))
        );
        // The admin now sees it confirmed in `GET /me`.
        let (_, me) = app.get_authed("/v1/me", &admin.secret).await;
        assert_eq!(me["memberships"][0]["confirmation"], "confirmed");
        // Confirming again changes nothing.
        let audits = audit_actions(&app.state.db)
            .await
            .iter()
            .filter(|a| *a == "shelter.confirm")
            .count();
        assert_eq!(
            app.send("POST", &path, &validator.secret, None).await.0,
            StatusCode::OK
        );
        assert_eq!(
            audit_actions(&app.state.db)
                .await
                .iter()
                .filter(|a| *a == "shelter.confirm")
                .count(),
            audits
        );
    }

    #[tokio::test]
    async fn revoking_needs_a_reason_and_goes_back_to_unconfirmed() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let id = app.confirmed_shelter(&admin, "Ginásio", ORIGIN).await;
        let civil_defense = app.actor("DC").await;
        app.grant(&civil_defense, crate::volunteers::GlobalRole::CivilDefense)
            .await;
        let path = format!("/v1/shelters/{id}/revoke");
        assert_eq!(
            app.send("POST", &path, &admin.secret, Some(json!({ "reason": "x" })))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        let (status, answer) = app
            .send(
                "POST",
                &path,
                &civil_defense.secret,
                Some(json!({ "reason": "  " })),
            )
            .await;
        assert_eq!(
            (status, error_of(&answer)),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_shelter"))
        );
        let (status, body) = app
            .send(
                "POST",
                &path,
                &civil_defense.secret,
                Some(json!({ "reason": "Não existe" })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let shelter = &body["shelter"];
        assert_eq!(
            (
                &shelter["confirmation"],
                &shelter["confirmation_type"],
                &shelter["can_endorse"]
            ),
            (&json!("unconfirmed"), &Value::Null, &json!(false))
        );
        let stored: ShelterDoc = store::get(&app.state.db, COLLECTION, &id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                stored.revocations.len(),
                stored.revocations[0].reason.as_str(),
                stored.revocations[0].by.as_str()
            ),
            (1, "Não existe", civil_defense.id.as_str())
        );
        // Revoking what is not confirmed does nothing.
        assert_eq!(
            app.send(
                "POST",
                &path,
                &civil_defense.secret,
                Some(json!({ "reason": "de novo" }))
            )
            .await
            .0,
            StatusCode::OK
        );
        let stored: ShelterDoc = store::get(&app.state.db, COLLECTION, &id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.revocations.len(), 1);
    }
}

#[cfg(test)]
mod follow_up_tests {
    use super::*;
    use crate::test_support::{ORIGIN, TestApp, all_docs, north_of_origin, shelter_body};

    #[tokio::test]
    async fn a_shelter_with_an_address_names_an_event_that_had_only_a_placeholder() {
        let app = TestApp::new().await;
        let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
        // The first shelter has no address yet, so the event is "Evento – MM/YYYY".
        let first = app.create_shelter(&ana, "A", ORIGIN).await;
        let mut body = shelter_body(
            &crate::test_support::new_id(),
            "B",
            north_of_origin(1_000.0),
        );
        body["address"] = json!({ "text": "Rua B, 2", "city": "Canoas", "state": "RS" });
        let (status, created) = app
            .send("POST", "/v1/shelters", &bia.secret, Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let name = created["event"]["name"].as_str().unwrap();
        assert!(name.starts_with("Canoas/RS – "), "{name}");
        let stored: ShelterDoc = store::get(&app.state.db, COLLECTION, &first)
            .await
            .unwrap()
            .unwrap();
        let event: EventDoc = store::get(
            &app.state.db,
            events::COLLECTION,
            stored.event_id.as_deref().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!((event.name.as_str(), event.auto_named), (name, true));
    }

    #[tokio::test]
    async fn shelter_audit_entries_name_the_shelter() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.create_shelter(&admin, "A", ORIGIN).await;
        let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let volunteer = app.join(&code, "Vol").await;
        app.send(
            "POST",
            &format!("/v1/shelters/{shelter}/leave"),
            &volunteer.secret,
            None,
        )
        .await;
        app.send(
            "PATCH",
            &format!("/v1/shelters/{shelter}"),
            &admin.secret,
            Some(json!({ "capacity": 10 })),
        )
        .await;
        let entries = all_docs(&app.state.db, "audit_log").await;
        for action in [
            "shelter.create",
            "shelter.update",
            "invite.create",
            "invite.accept",
            "member.leave",
        ] {
            let entry = entries
                .iter()
                .find(|e| e["action"] == action)
                .unwrap_or_else(|| panic!("{action}"));
            assert_eq!(entry["shelter_id"], json!(shelter), "{action}");
        }
        // A login is not about a shelter.
        let logins: Vec<_> = entries
            .iter()
            .filter(|e| e["action"] == "capture.create")
            .collect();
        assert!(!logins.is_empty() && logins.iter().all(|e| e.get("shelter_id").is_none()));
    }

    #[tokio::test]
    async fn me_lists_the_shelters_by_name() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let zulu = app.create_shelter(&ana, "Zulu", ORIGIN).await;
        let alfa = app
            .create_shelter(&ana, "Alfa", north_of_origin(5_000.0))
            .await;
        let (_, me) = app.get_authed("/v1/me", &ana.secret).await;
        let names: Vec<&str> = me["memberships"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Alfa", "Zulu"]);
        assert!(
            me["memberships"][0]["shelter_id"] == json!(alfa)
                && me["memberships"][1]["shelter_id"] == json!(zulu)
        );
    }

    #[tokio::test]
    async fn the_flagged_shelter_alert_is_for_the_civil_defense_not_the_creator() {
        let app = TestApp::new().await;
        let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
        app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let mut body = shelter_body(
            &crate::test_support::new_id(),
            "Escola",
            north_of_origin(80.0),
        );
        body["other_site_reason"] = json!("Outro prédio");
        assert_eq!(
            app.send("POST", "/v1/shelters", &bia.secret, Some(body))
                .await
                .0,
            StatusCode::CREATED
        );
        let alert_id = all_docs(&app.state.db, "alerts").await[0]["_firestore_id"]
            .as_str()
            .unwrap()
            .to_string();

        // Neither the creator of the flagged shelter nor the admin of the shelter it conflicts with.
        for who in [&bia, &ana] {
            let (status, listing) = app.send("GET", "/v1/alerts", &who.secret, None).await;
            assert_eq!(
                (status, listing["alerts"].as_array().unwrap().len()),
                (StatusCode::OK, 0),
                "{}",
                who.id
            );
            let (status, _) = app
                .send(
                    "POST",
                    &format!("/v1/alerts/{alert_id}/resolve"),
                    &who.secret,
                    None,
                )
                .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{}", who.id);
        }
        let dc = app.actor("DC").await;
        app.grant(&dc, GlobalRole::CivilDefense).await;
        let (_, listing) = app.send("GET", "/v1/alerts", &dc.secret, None).await;
        assert_eq!(listing["alerts"].as_array().unwrap().len(), 1);
        assert_eq!(
            app.send(
                "POST",
                &format!("/v1/alerts/{alert_id}/resolve"),
                &dc.secret,
                None
            )
            .await
            .0,
            StatusCode::OK
        );
    }
}
