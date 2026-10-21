//! Children (spec 3.4.1, 4.9): guardians, who may change `child_status`, and the
//! `child_separated` alert. Owner C.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use firestore::{FirestoreDb, FirestoreTransaction};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::alerts::{self, AlertDoc, AlertKind};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::events::{self, EventDoc, ReferenceContacts};
use crate::people::{self, ChildStatus, Guardian, GuardianLink, Level, PersonDoc};
use crate::shelters::ShelterDoc;
use crate::store::{self, tx_replace};
use crate::volunteers::GlobalRole;

/// `child_status` changes after the check-in belong to `protection` and the Civil Defense
/// (spec 3.4.1.4); the same two also exit separated children (spec 2.3).
pub fn can_change_status(volunteer: &Volunteer) -> bool {
    volunteer.has_global(GlobalRole::Protection) || volunteer.has_global(GlobalRole::CivilDefense)
}

/// Adds the `child_separated` alert to the transaction of the write that made the child
/// `separated` or `unaccompanied`; a no-op for `with_guardian`. The alert reaches the
/// protection queue, the Civil Defense and the admins of the shelter, and carries the
/// reference phones of the event (no push, spec 3.4.1.3).
#[allow(dead_code)] // called by `people.rs` (owner A) when a child checks in or changes status
pub async fn raise_child_separated(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    person_id: &str,
    shelter_id: &str,
    shelter: &ShelterDoc,
    status: ChildStatus,
) -> Result<(), ApiError> {
    if status == ChildStatus::WithGuardian {
        return Ok(());
    }
    let contacts = match &shelter.event_id {
        Some(event_id) => store::get::<EventDoc>(db, events::COLLECTION, event_id)
            .await?
            .map(|event| event.reference_contacts)
            .unwrap_or_default(),
        None => ReferenceContacts::default(),
    };
    let mut alert = AlertDoc::new(
        AlertKind::ChildSeparated,
        "person",
        person_id,
        json!({ "child_status": status, "shelter_id": shelter_id, "reference_contacts": contacts }),
    )
    .for_shelters(vec![shelter_id.to_string()]);
    alert.for_protection = true;
    alert.event_id = shelter.event_id.clone();
    alerts::add(db, tx, &alert)
}

// ---- POST /v1/people/{person_id}/guardians ----

#[derive(Debug, Clone, Deserialize)]
pub struct GuardianRequest {
    guardian_id: String,
    link: GuardianLink,
}

/// The entry for `request` as `volunteer` records it: only `document_checked` says who looked
/// at the document and when.
fn guardian_entry(request: &GuardianRequest, volunteer: &Volunteer) -> Guardian {
    let checked = request.link == GuardianLink::DocumentChecked;
    Guardian {
        person_id: request.guardian_id.clone(),
        link: request.link,
        checked_by: checked.then(|| volunteer.id.clone()),
        checked_at: checked.then(store::now),
    }
}

pub async fn post_guardian(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(person_id): Path<String>,
    Json(request): Json<GuardianRequest>,
) -> Result<Json<Value>, ApiError> {
    let guardians = store::transact(&state.db, |db, tx| {
        let (person_id, request, volunteer) =
            (person_id.clone(), request.clone(), volunteer.clone());
        Box::pin(async move {
            let mut child: PersonDoc = store::get(&db, people::COLLECTION, &person_id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "person_not_found"))?;
            let in_shelter = child
                .current_shelter_id
                .as_deref()
                .is_some_and(|shelter| volunteer.role_in(shelter).is_some());
            if !in_shelter && !can_change_status(&volunteer) {
                return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden"));
            }
            if !child.is_child {
                return Err(ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "not_a_child",
                ));
            }
            if request.guardian_id == person_id {
                return Err(ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_guardian",
                ));
            }
            let guardian: Option<PersonDoc> =
                store::get(&db, people::COLLECTION, &request.guardian_id).await?;
            if guardian.is_none_or(|g| g.level != Level::Verified) {
                return Err(ApiError::guardian_not_verified());
            }
            let entry = guardian_entry(&request, &volunteer);
            match child
                .guardians
                .iter_mut()
                .find(|g| g.person_id == entry.person_id)
            {
                Some(existing) => *existing = entry,
                None => child.guardians.push(entry),
            }
            child.updated_at = store::now();
            tx_replace(&db, tx, people::COLLECTION, &person_id, &child)?;
            let mut audit_entry = AuditEntry::new("person.guardian", "person", &person_id)
                .by(&volunteer.id)
                .basis(LegalBasis::ChildProtection);
            if let Some(shelter) = &child.current_shelter_id {
                audit_entry = audit_entry.in_shelter(shelter);
            }
            audit::add(&db, tx, &audit_entry)?;
            Ok(child.guardians)
        })
    })
    .await?;
    let view: Vec<Value> = guardians
        .iter()
        .map(|g| {
            json!({
                "person_id": g.person_id,
                "link": g.link,
                "checked_by": g.checked_by,
                "checked_at": g.checked_at.map(|t| t.to_string()),
            })
        })
        .collect();
    Ok(Json(json!({ "person_id": person_id, "guardians": view })))
}
