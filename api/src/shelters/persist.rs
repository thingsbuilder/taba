//! Reads and writes around the rules: the transaction that creates a shelter under the 100 m
//! rule (spec 3.2.7), the change a close, a confirmation or a revocation makes (spec 3.9) and the
//! flag on the endorsers of a revoked shelter.

use axum::http::StatusCode;
use firestore::FirestoreDb;
use serde_json::json;

use super::domain::{Neighbour, NewShelter};
use super::model::{COLLECTION, ConfirmationType, Endorsement, ShelterDoc};
use crate::alerts::{self, AlertDoc, AlertKind};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::error::ApiError;
use crate::events::{self, EventDoc};
use crate::geo;
use crate::members::{self, MembershipDoc};
use crate::store::{self, tx_create, tx_replace};
use crate::volunteers::{self, VolunteerDoc};

pub(super) async fn event_of(
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

pub(super) struct Created {
    pub shelter: ShelterDoc,
    pub event_id: String,
    pub event: EventDoc,
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
pub(super) async fn create_in(
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
    let neighbour = match near.first() {
        None => None,
        Some(nearest) => {
            let shelter_id = nearest.shelter.id.clone().unwrap_or_default();
            let Some(reason) = &new.other_site_reason else {
                return Err(conflict(db, &shelter_id, &nearest.shelter, nearest.distance_m).await?);
            };
            Some(Neighbour {
                shelter_id,
                reason: reason.clone(),
            })
        }
    };
    let flagged = neighbour.is_some();

    let now = store::now();
    let assignment =
        events::assign(db, new.lat, new.lon, &new.address, event_radius_km, now).await?;
    let shelter = ShelterDoc::open(new, creator_id, &assignment.event_id, neighbour, now);

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
                json!({ "conflicts_with": shelter.conflicts_with, "other_site_reason": shelter.other_site_reason }),
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
        &AuditEntry::new(super::ACTION_CREATE, "shelter", &new.id)
            .basis(LegalBasis::FraudPrevention)
            .by(creator_id)
            .in_shelter(&new.id),
    )?;
    Ok(Created {
        shelter,
        event_id: assignment.event_id,
        event,
    })
}

pub(super) fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found")
}

pub(super) async fn load(db: &FirestoreDb, id: &str) -> Result<ShelterDoc, ApiError> {
    store::get(db, COLLECTION, id).await?.ok_or_else(not_found)
}

/// What `update` does to a shelter.
#[derive(Clone)]
pub(super) enum Change {
    Close,
    Confirm,
    Revoke { by: String, reason: String },
}

impl Change {
    pub fn action(&self) -> &'static str {
        match self {
            Self::Close => super::ACTION_CLOSE,
            Self::Confirm => super::ACTION_CONFIRM,
            Self::Revoke { .. } => super::ACTION_REVOKE,
        }
    }

    /// Edits the shelter; `false` means it was already in that state, so nothing is written.
    pub async fn apply(
        &self,
        db: &FirestoreDb,
        tx: &mut firestore::FirestoreTransaction<'_>,
        id: &str,
        shelter: &mut ShelterDoc,
    ) -> Result<bool, ApiError> {
        match self {
            Self::Close => Ok(shelter.close()),
            Self::Confirm => Ok(shelter.confirm(ConfirmationType::Validator)),
            Self::Revoke { by, reason } => {
                let Some(endorsements) = shelter.revoke(by, reason, store::now()) else {
                    return Ok(false);
                };
                for endorsement in &endorsements {
                    flag_endorser(db, tx, id, reason, endorsement).await?;
                }
                Ok(true)
            }
        }
    }
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
