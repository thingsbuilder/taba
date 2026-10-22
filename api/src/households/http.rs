//! The routes (spec 3.5.1, 8.5): `POST /v1/shelters/{id}/households`, `GET` and `PATCH`
//! `/v1/households/{id}`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::domain::{MAX_MEMBERS, shelter_of_access, valid_ids, valid_pets, validate};
use super::model::{
    COLLECTION, CreateHousehold, HouseholdDoc, PatchHousehold, forbidden, in_other_household,
    invalid,
};
use super::persist::{load_household, load_members, load_person, touch_event};
use super::view::view;
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::people::{self, PersonDoc};
use crate::store::{self, now, transact, tx_create, tx_replace};

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
            crate::events::ensure_shelter_open(&db, &shelter_id).await?;
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
                tx_replace(&db, tx, people::COLLECTION, person.stored_id()?, person)?;
            }
            audit::add(
                &db,
                tx,
                &AuditEntry::new("household.create", "household", &new.id)
                    .basis(LegalBasis::FraudPrevention)
                    .by(&volunteer.id)
                    .in_shelter(&shelter_id),
            )?;
            Ok(view(&household, &members))
        })
    })
    .await?;
    crate::members::touch_shelter_event(&state, &shelter_id).await;
    Ok((StatusCode::CREATED, Json(json!({ "household": created }))))
}

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
            crate::events::ensure_shelter_open(&db, &shelter).await?;

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
                tx_replace(&db, tx, people::COLLECTION, person.stored_id()?, person)?;
            }
            audit::add(
                &db,
                tx,
                &AuditEntry::new("household.update", "household", &id)
                    .basis(LegalBasis::FraudPrevention)
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
    touch_event(&state, &id).await;
    Ok(Json(updated))
}
