//! Reads and writes of the membership routes. Each transaction reads what it needs, applies the
//! rules of `domain` and writes the result with its audit entry (and alert, when there is one).

use axum::http::StatusCode;
use firestore::errors::BackoffError;
use firestore::{FirestoreDb, path};
use serde::Deserialize;
use serde_json::json;

use super::domain::apply;
use super::model::{
    COLLECTION, Change, MembershipDoc, MembershipStatus, ShelterRole, last_admin, member_not_found,
    membership_id,
};
use crate::alerts::{self, AlertDoc, AlertKind};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Membership};
use crate::error::ApiError;
use crate::shelters::{self, ShelterDoc};
use crate::store::{self, fs_error, tx_replace};
use crate::volunteers::{COLLECTION as VOLUNTEERS, GlobalRole, VolunteerDoc};

/// Grants or revokes a role in one transaction with its audit entry. `Ok(false)` means the
/// volunteer already was in the requested state, so nothing was written.
pub async fn change_role(
    db: &FirestoreDb,
    volunteer_id: &str,
    role: GlobalRole,
    change: Change,
) -> Result<bool, ApiError> {
    let volunteer_id = volunteer_id.to_string();
    db.run_transaction(|db, tx| {
        let volunteer_id = volunteer_id.clone();
        Box::pin(async move {
            let run = async {
                let mut doc: VolunteerDoc = store::get(&db, VOLUNTEERS, &volunteer_id)
                    .await?
                    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "volunteer_not_found"))?;
                if !apply(&mut doc, role, change, crate::store::now()) {
                    return Ok(false);
                }
                tx_replace(&db, tx, VOLUNTEERS, &volunteer_id, &doc)?;
                let verb = if change == Change::Grant {
                    "grant"
                } else {
                    "revoke"
                };
                let action = format!("role.{verb}.{}", role.name());
                audit::add(
                    &db,
                    tx,
                    &AuditEntry::new(action, "volunteer", &volunteer_id)
                        .basis(LegalBasis::PublicPolicy),
                )?;
                Ok(true)
            };
            run.await.map_err(|e: ApiError| BackoffError::permanent(e))
        })
    })
    .await
    .map_err(fs_error)
}

async fn active_where(
    db: &FirestoreDb,
    field: String,
    id: &str,
) -> Result<Vec<MembershipDoc>, ApiError> {
    db.fluent()
        .select()
        .from(COLLECTION)
        .filter(|q| {
            q.for_all([
                q.field(&field).eq(id),
                q.field(path!(MembershipDoc::status))
                    .eq(MembershipStatus::Active),
            ])
        })
        .obj()
        .query()
        .await
        .map_err(fs_error)
}

pub async fn active_of_volunteer(
    db: &FirestoreDb,
    volunteer_id: &str,
) -> Result<Vec<MembershipDoc>, ApiError> {
    active_where(db, path!(MembershipDoc::volunteer_id), volunteer_id).await
}

pub async fn active_in_shelter(
    db: &FirestoreDb,
    shelter_id: &str,
) -> Result<Vec<MembershipDoc>, ApiError> {
    active_where(db, path!(MembershipDoc::shelter_id), shelter_id).await
}

/// The active memberships of a volunteer with the shelter's name and confirmation, as the
/// `Volunteer` extractor and `GET /v1/me` need them.
pub async fn memberships_of(
    db: &FirestoreDb,
    volunteer_id: &str,
) -> Result<Vec<Membership>, ApiError> {
    let mut memberships = Vec::new();
    for doc in active_of_volunteer(db, volunteer_id).await? {
        let Some(shelter) =
            store::get::<ShelterDoc>(db, shelters::COLLECTION, &doc.shelter_id).await?
        else {
            continue;
        };
        memberships.push(Membership {
            shelter_id: doc.shelter_id,
            name: shelter.name,
            role: doc.role,
            confirmation: shelter.confirmation,
            event_id: shelter.event_id,
        });
    }
    memberships.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.shelter_id.cmp(&b.shelter_id))
    });
    Ok(memberships)
}

pub(super) async fn membership(
    db: &FirestoreDb,
    shelter_id: &str,
    volunteer_id: &str,
) -> Result<Option<MembershipDoc>, ApiError> {
    store::get(db, COLLECTION, &membership_id(shelter_id, volunteer_id)).await
}

/// The membership if it exists and is active.
async fn active_membership(
    db: &FirestoreDb,
    shelter_id: &str,
    volunteer_id: &str,
) -> Result<MembershipDoc, ApiError> {
    membership(db, shelter_id, volunteer_id)
        .await?
        .filter(|m| m.status == MembershipStatus::Active)
        .ok_or_else(member_not_found)
}

pub(super) async fn admin_count(db: &FirestoreDb, shelter_id: &str) -> Result<usize, ApiError> {
    Ok(active_in_shelter(db, shelter_id)
        .await?
        .iter()
        .filter(|m| m.role == ShelterRole::Admin)
        .count())
}

/// Records activity on the event of a shelter after a write to it (spec 4.3.3).
pub async fn touch_shelter_event(state: &AppState, shelter_id: &str) {
    if let Ok(Some(shelter)) =
        store::get::<ShelterDoc>(&state.db, shelters::COLLECTION, shelter_id).await
    {
        crate::events::touch(state, shelter.event_id.as_deref()).await;
    }
}

/// Only the id of a session document is needed to delete it.
#[derive(Deserialize)]
struct SessionRef {
    #[serde(alias = "_firestore_id")]
    id: String,
}

pub(super) async fn promote(
    state: &AppState,
    shelter_id: &str,
    target: &str,
    by: &str,
) -> Result<(MembershipDoc, VolunteerDoc), ApiError> {
    store::transact(&state.db, |db, tx| {
        let (shelter_id, target, by) = (shelter_id.to_string(), target.to_string(), by.to_string());

        Box::pin(async move {
            let mut doc = active_membership(&db, &shelter_id, &target).await?;
            if doc.role != ShelterRole::Admin {
                doc.role = ShelterRole::Admin;
                tx_replace(
                    &db,
                    tx,
                    COLLECTION,
                    &membership_id(&shelter_id, &target),
                    &doc,
                )?;
                audit::add(
                    &db,
                    tx,
                    &AuditEntry::new(
                        super::ACTION_PROMOTE,
                        "membership",
                        &membership_id(&shelter_id, &target),
                    )
                    .by(&by)
                    .in_shelter(&shelter_id)
                    .basis(LegalBasis::FraudPrevention),
                )?;
            }
            let profile: VolunteerDoc = store::get(&db, VOLUNTEERS, &target)
                .await?
                .ok_or_else(member_not_found)?;
            Ok((doc, profile))
        })
    })
    .await
}

pub(super) async fn remove(
    state: &AppState,
    shelter_id: &str,
    target: &str,
    by: &str,
    civil_defense: bool,
) -> Result<(), ApiError> {
    store::transact(&state.db, |db, tx| {
        let (shelter_id, target, by) = (shelter_id.to_string(), target.to_string(), by.to_string());

        Box::pin(async move {
            let mut doc = active_membership(&db, &shelter_id, &target).await?;
            if doc.role == ShelterRole::Admin {
                // An admin removes volunteers only; the Civil Defense may remove an admin
                // as long as another one stays (spec 2.3).
                if !civil_defense {
                    return Err(ApiError::new(
                        StatusCode::FORBIDDEN,
                        "admin_cannot_remove_admin",
                    ));
                }
                if admin_count(&db, &shelter_id).await? <= 1 {
                    return Err(last_admin());
                }
            }
            doc.remove(&by, store::now());
            let id = membership_id(&shelter_id, &target);
            tx_replace(&db, tx, COLLECTION, &id, &doc)?;
            // The person's admins elsewhere hear about it (spec 4.4).
            let elsewhere: Vec<String> = active_of_volunteer(&db, &target)
                .await?
                .into_iter()
                .filter(|m| m.shelter_id != shelter_id)
                .map(|m| m.shelter_id)
                .collect();
            if !elsewhere.is_empty() {
                alerts::add(
                    &db,
                    tx,
                    &AlertDoc::new(
                        AlertKind::VolunteerRemoved,
                        "volunteer",
                        &target,
                        json!({ "removed_from_shelter_id": shelter_id }),
                    )
                    .for_shelters(elsewhere)
                    .about_volunteers(vec![target.clone()]),
                )?;
            }
            audit::add(
                &db,
                tx,
                &AuditEntry::new(super::ACTION_REMOVE, "membership", &id)
                    .basis(LegalBasis::FraudPrevention)
                    .by(&by)
                    .in_shelter(&shelter_id),
            )
        })
    })
    .await
}

pub(super) async fn leave(state: &AppState, shelter_id: &str, who: &str) -> Result<(), ApiError> {
    store::transact(&state.db, |db, tx| {
        let (shelter_id, who) = (shelter_id.to_string(), who.to_string());

        Box::pin(async move {
            let mut doc = active_membership(&db, &shelter_id, &who).await?;
            if doc.role == ShelterRole::Admin && admin_count(&db, &shelter_id).await? <= 1 {
                return Err(last_admin());
            }
            doc.status = MembershipStatus::Left;
            let id = membership_id(&shelter_id, &who);
            tx_replace(&db, tx, COLLECTION, &id, &doc)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new(super::ACTION_LEAVE, "membership", &id)
                    .basis(LegalBasis::FraudPrevention)
                    .by(&who)
                    .in_shelter(&shelter_id),
            )
        })
    })
    .await
}

pub(super) async fn ban(
    state: &AppState,
    target: &str,
    by: &str,
    reason: &str,
) -> Result<(), ApiError> {
    store::transact(&state.db, |db, tx| {
        let (target, by, reason) = (target.to_string(), by.to_string(), reason.to_string());

        Box::pin(async move {
            let mut doc: VolunteerDoc = store::get(&db, VOLUNTEERS, &target)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "volunteer_not_found"))?;
            if doc.banned {
                return Ok(());
            }
            let now = store::now();
            doc.banned = true;
            doc.banned_by = Some(by.clone());
            doc.banned_at = Some(now);
            doc.flag_reasons.push(format!("banned: {reason}"));
            tx_replace(&db, tx, VOLUNTEERS, &target, &doc)?;
            let memberships = active_of_volunteer(&db, &target).await?;
            // A ban never leaves a shelter without an admin.
            for membership in memberships.iter().filter(|m| m.role == ShelterRole::Admin) {
                if admin_count(&db, &membership.shelter_id).await? <= 1 {
                    return Err(last_admin());
                }
            }
            for mut membership in memberships {
                membership.remove(&by, now);
                let id = membership_id(&membership.shelter_id, &target);
                tx_replace(&db, tx, COLLECTION, &id, &membership)?;
            }
            let sessions: Vec<SessionRef> = db
                .fluent()
                .select()
                .from(crate::auth::SESSIONS)
                .filter(|q| q.for_all([q.field("volunteer_id").eq(target.as_str())]))
                .obj()
                .query()
                .await
                .map_err(fs_error)?;
            for session in sessions {
                store::tx_delete(&db, tx, crate::auth::SESSIONS, &session.id)?;
            }
            audit::add(
                &db,
                tx,
                &AuditEntry::new(super::ACTION_BAN, "volunteer", &target)
                    .basis(LegalBasis::FraudPrevention)
                    .by(&by),
            )
        })
    })
    .await
}
