//! The two transactions of the endorsement routes. Each reads what it needs, applies the rules
//! and writes the result with its audit entry.

use axum::http::StatusCode;

use super::model::{COLLECTION, Endorse, EndorsementCodeDoc, already_confirmed};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::members;
use crate::shelters::{self, Confirmation, Endorsement, EndorsementKind, ShelterDoc};
use crate::store::{self, tx_create, tx_replace};

/// Stores a new code for an unconfirmed shelter.
pub(super) async fn create(
    state: &AppState,
    shelter_id: &str,
    code: &str,
    doc: &EndorsementCodeDoc,
    by: &str,
) -> Result<(), ApiError> {
    store::transact(&state.db, |db, tx| {
        let (shelter_id, code, doc, by) = (
            shelter_id.to_string(),
            code.to_string(),
            doc.clone(),
            by.to_string(),
        );

        Box::pin(async move {
            let shelter: ShelterDoc = store::get(&db, shelters::COLLECTION, &shelter_id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found"))?;
            if shelter.confirmation == Confirmation::Confirmed {
                return Err(already_confirmed());
            }
            tx_create(&db, tx, COLLECTION, &code, &doc)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new(super::ACTION_CODE, "shelter", &shelter_id)
                    .basis(LegalBasis::FraudPrevention)
                    .by(&by)
                    .in_shelter(&shelter_id),
            )
        })
    })
    .await
}

/// Confirms the new shelter with the code, for an endorser within `radius_m` of the code's
/// phone. Returns the shelter and its id.
pub(super) async fn endorse(
    state: &AppState,
    request: &Endorse,
    volunteer: &Volunteer,
    radius_m: f64,
) -> Result<(String, ShelterDoc), ApiError> {
    store::transact(&state.db, |db, tx| {
        let (request, volunteer) = (request.clone(), volunteer.clone());

        Box::pin(async move {
            let mut code = store::get::<EndorsementCodeDoc>(&db, COLLECTION, &request.code)
                .await?
                .filter(|c| c.is_usable(store::now()))
                .ok_or_else(|| ApiError::new(StatusCode::GONE, "endorsement_code_invalid"))?;
            let mut shelter: ShelterDoc = store::get(&db, shelters::COLLECTION, &code.shelter_id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::GONE, "endorsement_code_invalid"))?;
            if shelter.confirmation == Confirmation::Confirmed {
                return Err(already_confirmed());
            }
            crate::events::ensure_open(&db, shelter.event_id.as_deref()).await?;

            // The endorser: an admin of a confirmed shelter that may endorse, and not a member
            // of the new one (spec 3.9).
            let cannot = || ApiError::new(StatusCode::FORBIDDEN, "cannot_endorse");
            if !volunteer.is_admin_of(&request.endorser_shelter_id) {
                return Err(cannot());
            }
            let endorser_shelter: ShelterDoc =
                store::get(&db, shelters::COLLECTION, &request.endorser_shelter_id)
                    .await?
                    .ok_or_else(cannot)?;
            if endorser_shelter.confirmation != Confirmation::Confirmed
                || !endorser_shelter.can_endorse
            {
                return Err(cannot());
            }
            let member_of_new = store::get::<members::MembershipDoc>(
                &db,
                members::COLLECTION,
                &members::membership_id(&code.shelter_id, &volunteer.id),
            )
            .await?
            .is_some_and(|m| m.status == members::MembershipStatus::Active);
            if member_of_new {
                return Err(cannot());
            }

            // Both phones must know where they are, and be close (spec 3.9.3).
            let distance_m = code.distance_to(&request.position, radius_m)?;

            shelter.endorse_in_person(Endorsement {
                kind: EndorsementKind::InPerson,
                volunteer_id: volunteer.id.clone(),
                endorser_shelter_id: request.endorser_shelter_id.clone(),
                at: store::now(),
                distance_m,
            });
            code.used = true;
            let shelter_id = code.shelter_id.clone();
            tx_replace(&db, tx, shelters::COLLECTION, &shelter_id, &shelter)?;
            tx_replace(&db, tx, COLLECTION, &request.code, &code)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new(super::ACTION_CREATE, "shelter", &shelter_id)
                    .basis(LegalBasis::FraudPrevention)
                    .by(&volunteer.id)
                    .in_shelter(&shelter_id),
            )?;
            Ok((shelter_id, shelter))
        })
    })
    .await
}
