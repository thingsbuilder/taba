//! Reads and transactions of the exit route: the occupancy count, the transaction that
//! closes the stay and the one that records a failed revalidation.

use axum::http::StatusCode;
use firestore::{FirestoreDb, FirestoreInstant, path};
use serde::Deserialize;
use serde_json::json;

use super::domain::{open_stay, plan};
use super::model::{Exit, Plan, not_in_shelter};
use crate::alerts::{self, AlertDoc, AlertKind};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{self, AppState, CaptureDoc};
use crate::error::ApiError;
use crate::people::{self, ExitReason, PersonDoc};
use crate::store::{self, fs_error, tx_replace};

pub(super) const CAPTURES: &str = "captures";

/// People with an open stay in the shelter (spec 4.2). `current_shelter_id` is set exactly
/// while a stay is open.
pub async fn occupancy(db: &FirestoreDb, shelter_id: &str) -> Result<u32, ApiError> {
    #[derive(Deserialize)]
    struct Count {
        open: i64,
    }
    let rows: Vec<Count> = db
        .fluent()
        .select()
        .from(people::COLLECTION)
        .filter(|q| q.for_all([q.field(path!(PersonDoc::current_shelter_id)).eq(shelter_id)]))
        .aggregate(|a| a.fields([a.field("open").count()]))
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    // an aggregate that cannot be read as a count is an error, not an empty shelter
    rows.first().map_or(Ok(0), |c| {
        u32::try_from(c.open).map_err(|_| {
            tracing::error!(open = c.open, "integrity: occupancy count out of range");
            ApiError::internal()
        })
    })
}

/// One transaction: re-reads the person, re-checks the rules on what it read, consumes the
/// revalidation capture and closes the stay. Of two exits of the same person only one gets here.
pub(super) async fn close_stay(
    db: &FirestoreDb,
    tx: &mut firestore::FirestoreTransaction<'_>,
    a: &Exit,
) -> Result<FirestoreInstant, ApiError> {
    let mut person: PersonDoc = store::get(db, people::COLLECTION, &a.request.person_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "person_not_found"))?;
    let index = open_stay(&person, &a.shelter_id).ok_or_else(not_in_shelter)?;
    let plan = plan(a.member, a.privileged, &a.request, &person)?;

    // The plan was made before the transaction. If the person became a child meanwhile, the exit
    // has no revalidation behind it and must not close the stay: the caller starts again.
    if let Plan::Revalidate { guardian_id, .. } = &plan {
        if a.revalidated.as_ref() != Some(guardian_id) {
            return Err(ApiError::new(StatusCode::CONFLICT, "person_changed"));
        }
    }
    if let (Plan::Revalidate { capture_id, .. }, Some(resolved)) = (&plan, &a.revalidated) {
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
            &auth::consumed(capture, resolved),
        )?;
    }

    let now = store::now();
    let (reason, action) = match plan {
        Plan::Exception { .. } => (ExitReason::ExceptionalExit, "person.exit_exceptional"),
        _ => (ExitReason::Exit, "person.exit"),
    };
    let stay = &mut person.stays[index];
    stay.left_at = Some(now);
    stay.exit_reason = Some(reason);
    stay.exit_destination = Some(a.destination);
    stay.destination_municipality = a.municipality.clone();
    if person.current_shelter_id.as_deref() == Some(a.shelter_id.as_str()) {
        person.current_shelter_id = None;
    }
    person.updated_at = now;
    tx_replace(db, tx, people::COLLECTION, &a.request.person_id, &person)?;

    let basis = if person.is_child {
        LegalBasis::ChildProtection
    } else {
        person.legal_basis
    };
    audit::add(
        db,
        tx,
        &AuditEntry::new(action, "person", &a.request.person_id)
            .by(&a.volunteer.id)
            .in_shelter(&a.shelter_id)
            .basis(basis),
    )?;
    if let Plan::Exception { reason } = plan {
        alerts::add(
            db,
            tx,
            &AlertDoc::new(
                AlertKind::ExceptionalExit,
                "person",
                &a.request.person_id,
                json!({ "shelter_id": a.shelter_id, "by": a.volunteer.id, "reason": reason }),
            ),
        )?;
    }
    Ok(now)
}

/// A revalidation that did not match: the capture is spent and the protection queue is told.
/// The exit is not registered (spec 3.8.6).
pub(super) async fn record_failed_revalidation(
    state: &AppState,
    a: &Exit,
    capture_id: &str,
    guardian_id: &str,
    resolved: &str,
) -> Result<(), ApiError> {
    store::transact(&state.db, |db, tx| {
        let (a, capture_id, guardian_id, resolved) = (
            a.clone(),
            capture_id.to_string(),
            guardian_id.to_string(),
            resolved.to_string(),
        );
        Box::pin(async move {
            let capture: CaptureDoc = store::get(&db, CAPTURES, &capture_id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "capture_not_found"))?;
            if capture.consumed {
                return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
            }
            tx_replace(
                &db,
                tx,
                CAPTURES,
                &capture_id,
                &auth::consumed(capture, &resolved),
            )?;
            let mut alert = AlertDoc::new(
                AlertKind::RevalidationFailed,
                "person",
                &a.request.person_id,
                json!({
                    "shelter_id": a.shelter_id,
                    "guardian_id": guardian_id,
                    "by": a.volunteer.id,
                }),
            );
            // For protection and the Civil Defense only: the admins of the shelter do not get it.
            alert.for_protection = true;
            alerts::add(&db, tx, &alert)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new(
                    "person.exit_revalidation_failed",
                    "person",
                    &a.request.person_id,
                )
                .by(&a.volunteer.id)
                .in_shelter(&a.shelter_id)
                .basis(LegalBasis::ChildProtection),
            )
        })
    })
    .await
}
