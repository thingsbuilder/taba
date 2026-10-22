//! The routes (spec 3.2, 3.9, 8.2, 8.3): they parse and authorize the request, call `persist` for
//! the transaction and answer with the views.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::domain::{
    CreateShelter, MAX_REASON_CHARS, invalid, valid_capacity, valid_name, validate,
};
use super::model::{Address, AddressInput, COLLECTION};
use super::persist::{Change, create_in, event_of, load};
use super::view::{full_view, respond, view_for};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::events;
use crate::members::ShelterRole;
use crate::store::{self, tx_replace};
use crate::volunteers::{self, GlobalRole};

pub async fn post_shelters(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Json(request): Json<CreateShelter>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let new = validate(request)?;
    let radius_m = f64::from(state.config.shelter_radius_m);
    let event_radius_km = state.config.event_radius_km;
    let created = {
        // One creation at a time per process, so creations queue behind each other; each is one
        // short transaction. The transaction below reads the neighbours itself, so it also
        // refuses a second shelter next to the first without this lock; a lock document per
        // geohash cell would replace it if the queue ever shows in the latency of creating.
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
                &AuditEntry::new(super::ACTION_UPDATE, "shelter", &id)
                    .basis(LegalBasis::FraudPrevention)
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
                        .in_shelter(&id)
                        .basis(LegalBasis::FraudPrevention),
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
