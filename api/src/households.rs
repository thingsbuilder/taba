//! Households (spec 3.5.1, 7, 8.5). Owner B.
//!
//! A person is in at most one household: `people.household_id` is the single source of truth,
//! read and written in the same transaction as `households.member_ids` (spec 6.2.4). Reading a
//! household shows names and levels only, never the shelter a member is in nor their history
//! (spec 10.3, criterion 35).

use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use firestore::{FirestoreDb, FirestoreInstant};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::{self, AuditEntry};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::people::{self, PersonDoc};
use crate::store::{self, now, transact, tx_create, tx_replace};

pub const COLLECTION: &str = "households";

const MAX_MEMBERS: usize = 50;
const MAX_PETS: usize = 10;
const MAX_SPECIES_CHARS: usize = 40;
const MAX_PET_COUNT: u32 = 99;
const MAX_ID_CHARS: usize = 128;

/// Companion animals are only a count (spec 3.5.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pet {
    pub species: String,
    pub count: u32,
}

/// `households/{household_id}` (UUID from the client).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HouseholdDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    #[serde(default)]
    pub member_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_id: Option<String>,
    #[serde(default)]
    pub pets: Vec<Pet>,
    /// The household's `active` card. The transaction that issues or cancels a card keeps it in
    /// step, so "one active card per household" is a check on this one document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_code: Option<String>,
    pub created_by: String,
    pub created_shelter_id: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub client_created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub updated_at: FirestoreInstant,
}

// ---- errors and shared helpers (cards.rs uses them too) ----

fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_household");
    error.detail = json!({ "field": field });
    error
}

pub fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "household_not_found")
}

fn person_not_found(person_id: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::NOT_FOUND, "person_not_found");
    error.detail = json!({ "person_id": person_id });
    error
}

fn in_other_household(person_id: &str, household_id: &str) -> ApiError {
    let mut error = ApiError::person_in_other_household();
    error.detail = json!({ "person_id": person_id, "household_id": household_id });
    error
}

pub async fn load_household(db: &FirestoreDb, id: &str) -> Result<HouseholdDoc, ApiError> {
    store::get(db, COLLECTION, id).await?.ok_or_else(not_found)
}

async fn load_person(db: &FirestoreDb, id: &str) -> Result<PersonDoc, ApiError> {
    store::get(db, people::COLLECTION, id)
        .await?
        .ok_or_else(|| person_not_found(id))
}

/// The members that still have a document, in `member_ids` order.
pub async fn load_members(
    db: &FirestoreDb,
    household: &HouseholdDoc,
) -> Result<Vec<PersonDoc>, ApiError> {
    let mut members = Vec::new();
    for id in &household.member_ids {
        members.extend(store::get::<PersonDoc>(db, people::COLLECTION, id).await?);
    }
    Ok(members)
}

/// The shelter that lets `volunteer` act on a household: one the volunteer belongs to and where
/// some member is now (spec 8.5). Matrix 2.3 gives volunteers and admins of confirmed and
/// unconfirmed shelters the same power here.
pub fn shelter_of_access(volunteer: &Volunteer, members: &[PersonDoc]) -> Option<String> {
    members
        .iter()
        .filter_map(|p| p.current_shelter_id.as_deref())
        .find(|shelter| volunteer.role_in(shelter).is_some())
        .map(str::to_string)
}

/// Name and level only: no shelter, no history (spec 10.3).
pub fn members_view(members: &[PersonDoc]) -> Vec<Value> {
    members
        .iter()
        .map(|p| {
            json!({
                "person_id": p.id,
                "name": p.name,
                "level": p.level,
            })
        })
        .collect()
}

fn view(household: &HouseholdDoc, members: &[PersonDoc]) -> Value {
    json!({
        "id": household.id,
        "members": members_view(members),
        "head_id": household.head_id,
        "pets": household.pets,
        "card_code": household.card_code,
    })
}

// ---- validation ----

fn valid_ids(ids: &[String], field: &str) -> Result<(), ApiError> {
    let mut seen = HashSet::new();
    let ok = ids.len() <= MAX_MEMBERS
        && ids
            .iter()
            .all(|id| valid_id(id) && seen.insert(id.as_str()));
    ok.then_some(()).ok_or_else(|| invalid(field))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().count() <= MAX_ID_CHARS && !id.contains('/')
}

fn valid_pets(pets: &[Pet]) -> Result<Vec<Pet>, ApiError> {
    if pets.len() > MAX_PETS {
        return Err(invalid("pets"));
    }
    pets.iter()
        .map(|pet| {
            let species = pet.species.trim();
            let ok = !species.is_empty()
                && species.chars().count() <= MAX_SPECIES_CHARS
                && (1..=MAX_PET_COUNT).contains(&pet.count);
            ok.then(|| Pet {
                species: species.to_string(),
                count: pet.count,
            })
            .ok_or_else(|| invalid("pets"))
        })
        .collect()
}

// ---- POST /v1/shelters/{id}/households ----

#[derive(Debug, Deserialize)]
pub struct CreateHousehold {
    id: String,
    member_ids: Vec<String>,
    #[serde(default)]
    head_id: Option<String>,
    #[serde(default)]
    pets: Vec<Pet>,
    client_created_at: String,
}

#[derive(Debug, Clone)]
struct NewHousehold {
    id: String,
    member_ids: Vec<String>,
    head_id: Option<String>,
    pets: Vec<Pet>,
    client_created_at: FirestoreInstant,
}

fn validate(request: CreateHousehold) -> Result<NewHousehold, ApiError> {
    uuid::Uuid::parse_str(&request.id).map_err(|_| invalid("id"))?;
    if request.member_ids.is_empty() {
        return Err(invalid("member_ids"));
    }
    valid_ids(&request.member_ids, "member_ids")?;
    if request
        .head_id
        .as_ref()
        .is_some_and(|head| !request.member_ids.contains(head))
    {
        return Err(invalid("head_id"));
    }
    Ok(NewHousehold {
        pets: valid_pets(&request.pets)?,
        client_created_at: request
            .client_created_at
            .parse()
            .map_err(|_| invalid("client_created_at"))?,
        id: request.id,
        member_ids: request.member_ids,
        head_id: request.head_id,
    })
}

pub async fn post_household(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
    Json(request): Json<CreateHousehold>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if volunteer.role_in(&shelter_id).is_none() {
        return Err(forbidden());
    }
    let new = validate(request)?;
    let created = transact(&state.db, |db, tx| {
        let (volunteer, shelter_id, new) = (volunteer.clone(), shelter_id.clone(), new.clone());
        Box::pin(async move {
            // The same request again (a lost response, a retry) gets the same answer.
            if let Some(existing) = store::get::<HouseholdDoc>(&db, COLLECTION, &new.id).await? {
                if existing.created_by != volunteer.id {
                    return Err(ApiError::new(StatusCode::CONFLICT, "household_exists"));
                }
                return Ok(view(&existing, &load_members(&db, &existing).await?));
            }
            let mut members = Vec::new();
            for person_id in &new.member_ids {
                let person = load_person(&db, person_id).await?;
                if let Some(other) = &person.household_id {
                    return Err(in_other_household(person_id, other));
                }
                members.push(person);
            }
            let at = now();
            let household = HouseholdDoc {
                id: Some(new.id.clone()),
                member_ids: new.member_ids.clone(),
                head_id: new.head_id.clone(),
                pets: new.pets.clone(),
                card_code: None,
                created_by: volunteer.id.clone(),
                created_shelter_id: shelter_id.clone(),
                client_created_at: new.client_created_at,
                created_at: at,
                updated_at: at,
            };
            tx_create(&db, tx, COLLECTION, &new.id, &household)?;
            for person in &mut members {
                person.household_id = Some(new.id.clone());
                person.updated_at = at;
                tx_replace(&db, tx, people::COLLECTION, &person_key(person), person)?;
            }
            audit::add(
                &db,
                tx,
                &AuditEntry::new("household.create", "household", &new.id)
                    .by(&volunteer.id)
                    .in_shelter(&shelter_id),
            )?;
            Ok(view(&household, &members))
        })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(json!({ "household": created }))))
}

fn person_key(person: &PersonDoc) -> String {
    person.id.clone().unwrap_or_default()
}

// ---- GET /v1/households/{id} ----

pub async fn get_household(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let household = load_household(&state.db, &id).await?;
    let members = load_members(&state.db, &household).await?;
    shelter_of_access(&volunteer, &members).ok_or_else(forbidden)?;
    Ok(Json(view(&household, &members)))
}

// ---- PATCH /v1/households/{id} ----

#[derive(Debug, Clone, Deserialize)]
pub struct PatchHousehold {
    #[serde(default)]
    add_member_ids: Vec<String>,
    #[serde(default)]
    remove_member_ids: Vec<String>,
    #[serde(default)]
    head_id: Option<String>,
    #[serde(default)]
    pets: Option<Vec<Pet>>,
}

pub async fn patch_household(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(id): Path<String>,
    Json(patch): Json<PatchHousehold>,
) -> Result<Json<Value>, ApiError> {
    valid_ids(&patch.add_member_ids, "add_member_ids")?;
    valid_ids(&patch.remove_member_ids, "remove_member_ids")?;
    if patch
        .add_member_ids
        .iter()
        .any(|id| patch.remove_member_ids.contains(id))
    {
        return Err(invalid("remove_member_ids"));
    }
    let pets = patch.pets.as_deref().map(valid_pets).transpose()?;

    let updated = transact(&state.db, |db, tx| {
        let (volunteer, id, patch, pets) =
            (volunteer.clone(), id.clone(), patch.clone(), pets.clone());
        Box::pin(async move {
            let mut household = load_household(&db, &id).await?;
            let members = load_members(&db, &household).await?;
            let shelter = shelter_of_access(&volunteer, &members).ok_or_else(forbidden)?;

            // People whose `household_id` changes, and the added ones as read.
            let mut changed: Vec<PersonDoc> = Vec::new();
            let mut added: Vec<PersonDoc> = Vec::new();
            for person_id in &patch.remove_member_ids {
                household.member_ids.retain(|m| m != person_id);
                let leaving = members.iter().find(|p| p.id.as_deref() == Some(person_id));
                if let Some(person) = leaving.filter(|p| p.household_id.as_deref() == Some(&id)) {
                    let mut person = person.clone();
                    person.household_id = None;
                    changed.push(person);
                }
            }
            for person_id in &patch.add_member_ids {
                if household.member_ids.contains(person_id) {
                    continue;
                }
                let mut person = load_person(&db, person_id).await?;
                if let Some(other) = person.household_id.as_ref().filter(|h| **h != id) {
                    return Err(in_other_household(person_id, other));
                }
                person.household_id = Some(id.clone());
                household.member_ids.push(person_id.clone());
                added.push(person.clone());
                changed.push(person);
            }
            if household.member_ids.len() > MAX_MEMBERS {
                return Err(invalid("add_member_ids"));
            }
            match &patch.head_id {
                Some(head) if household.member_ids.contains(head) => {
                    household.head_id = Some(head.clone());
                }
                Some(_) => return Err(invalid("head_id")),
                None => {
                    household.head_id = household
                        .head_id
                        .take()
                        .filter(|head| household.member_ids.contains(head));
                }
            }
            if let Some(pets) = pets {
                household.pets = pets;
            }
            let at = now();
            household.updated_at = at;
            tx_replace(&db, tx, COLLECTION, &id, &household)?;
            for person in &mut changed {
                person.updated_at = at;
                tx_replace(&db, tx, people::COLLECTION, &person_key(person), person)?;
            }
            audit::add(
                &db,
                tx,
                &AuditEntry::new("household.update", "household", &id)
                    .by(&volunteer.id)
                    .in_shelter(&shelter),
            )?;
            let now_members: Vec<PersonDoc> = household
                .member_ids
                .iter()
                .filter_map(|m| {
                    members
                        .iter()
                        .chain(&added)
                        .find(|p| p.id.as_deref() == Some(m))
                        .cloned()
                })
                .collect();
            Ok(view(&household, &now_members))
        })
    })
    .await?;
    Ok(Json(updated))
}
