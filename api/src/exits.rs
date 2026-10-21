//! Exits and occupancy (spec 3.8, 4.2): `POST /v1/shelters/{id}/exits` (adults, children,
//! exceptions) and "N of capacity" from the open stays. Owner C.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use firestore::{FirestoreDb, FirestoreInstant, path};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::alerts::{self, AlertDoc, AlertKind};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{self, AppState, CaptureDoc, Purpose, Volunteer, resolve_person, usable_capture};
use crate::children::can_change_status;
use crate::error::ApiError;
use crate::members;
use crate::people::{self, ChildStatus, ExitDestination, ExitReason, GuardianLink, PersonDoc};
use crate::shelters::{self, ShelterDoc};
use crate::store::{self, fs_error, tx_replace};

const CAPTURES: &str = "captures";
const MAX_MUNICIPALITY_CHARS: usize = 100;
const MAX_REASON_CHARS: usize = 500;

// ---- occupancy ----

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
    Ok(rows
        .first()
        .map_or(0, |c| u32::try_from(c.open).unwrap_or(0)))
}

/// `capacity - occupancy`, never below zero; `None` while the shelter has no capacity set.
pub fn vacancies(capacity: Option<u32>, occupancy: u32) -> Option<u32> {
    capacity.map(|capacity| capacity.saturating_sub(occupancy))
}

// ---- POST /v1/shelters/{id}/exits ----

#[derive(Debug, Clone, Deserialize)]
pub struct Exception {
    reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExitRequest {
    person_id: String,
    #[serde(default)]
    exit_destination: Option<ExitDestination>,
    #[serde(default)]
    destination_municipality: Option<String>,
    #[serde(default)]
    guardian_id: Option<String>,
    #[serde(default)]
    capture_id: Option<String>,
    #[serde(default)]
    exception: Option<Exception>,
}

/// What an exit needs before the stay closes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Plan {
    Adult,
    Exception {
        reason: String,
    },
    /// The guardian must be revalidated with the capture (spec 3.8).
    Revalidate {
        guardian_id: String,
        capture_id: String,
    },
}

fn invalid(code: &'static str) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code)
}

fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

fn not_in_shelter() -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "not_in_shelter")
}

/// Who may do what (spec 2.3, 3.8). `member`: active member of the shelter; `privileged`:
/// `protection` or Civil Defense. Decides before anything is asked of Unico.
fn plan(
    member: bool,
    privileged: bool,
    request: &ExitRequest,
    person: &PersonDoc,
) -> Result<Plan, ApiError> {
    if let Some(exception) = &request.exception {
        if !person.is_child {
            return Err(invalid("exception_for_child_only"));
        }
        if !privileged {
            return Err(ApiError::protection_role_required());
        }
        let reason = exception.reason.trim();
        if reason.is_empty() || reason.chars().count() > MAX_REASON_CHARS {
            return Err(invalid("exception_reason_required"));
        }
        return Ok(Plan::Exception {
            reason: reason.to_string(),
        });
    }
    if !person.is_child {
        return if member {
            Ok(Plan::Adult)
        } else {
            Err(forbidden())
        };
    }
    let guardian = request
        .guardian_id
        .as_deref()
        .and_then(|id| person.guardians.iter().find(|g| g.person_id == id));
    let volunteer_may = member
        && person.child_status == Some(ChildStatus::WithGuardian)
        && guardian.is_some_and(|g| g.link == GuardianLink::DocumentChecked);
    if !privileged && !volunteer_may {
        return Err(ApiError::protection_role_required());
    }
    if request.guardian_id.is_none() {
        return Err(invalid("guardian_required"));
    }
    let guardian = guardian.ok_or_else(|| invalid("guardian_not_linked"))?;
    let capture_id = request
        .capture_id
        .clone()
        .ok_or_else(|| invalid("capture_required"))?;
    Ok(Plan::Revalidate {
        guardian_id: guardian.person_id.clone(),
        capture_id,
    })
}

fn open_stay(person: &PersonDoc, shelter_id: &str) -> Option<usize> {
    person
        .stays
        .iter()
        .position(|s| s.shelter_id == shelter_id && s.left_at.is_none())
}

fn municipality(value: &Option<String>) -> Result<Option<String>, ApiError> {
    let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > MAX_MUNICIPALITY_CHARS {
        return Err(invalid("invalid_destination_municipality"));
    }
    Ok(Some(value.to_string()))
}

/// Everything the transaction that closes the stay needs.
#[derive(Clone)]
struct Exit {
    shelter_id: String,
    volunteer: Volunteer,
    member: bool,
    privileged: bool,
    request: ExitRequest,
    destination: ExitDestination,
    municipality: Option<String>,
    /// The person the revalidation capture turned out to be, once it was resolved.
    revalidated: Option<String>,
}

/// One transaction: re-reads the person, re-checks the rules on what it read, consumes the
/// revalidation capture and closes the stay. Of two exits of the same person only one gets here.
async fn close_stay(
    db: &FirestoreDb,
    tx: &mut firestore::FirestoreTransaction<'_>,
    a: &Exit,
) -> Result<FirestoreInstant, ApiError> {
    let mut person: PersonDoc = store::get(db, people::COLLECTION, &a.request.person_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "person_not_found"))?;
    let index = open_stay(&person, &a.shelter_id).ok_or_else(not_in_shelter)?;
    let plan = plan(a.member, a.privileged, &a.request, &person)?;

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
async fn record_failed_revalidation(
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
            )
            .for_shelters(vec![a.shelter_id.clone()]);
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

pub async fn post_exit(
    State(state): State<AppState>,
    headers: HeaderMap,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
    Json(request): Json<ExitRequest>,
) -> Result<Json<Value>, ApiError> {
    let member = volunteer.role_in(&shelter_id).is_some();
    let privileged = can_change_status(&volunteer);
    if !member && !privileged {
        return Err(forbidden());
    }
    store::get::<ShelterDoc>(&state.db, shelters::COLLECTION, &shelter_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found"))?;
    let destination = request
        .exit_destination
        .ok_or_else(|| invalid("exit_destination_required"))?;
    let municipality = municipality(&request.destination_municipality)?;
    let person: PersonDoc = store::get(&state.db, people::COLLECTION, &request.person_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "person_not_found"))?;
    open_stay(&person, &shelter_id).ok_or_else(not_in_shelter)?;
    let decided = plan(member, privileged, &request, &person)?;

    let mut exit = Exit {
        shelter_id: shelter_id.clone(),
        volunteer: volunteer.clone(),
        member,
        privileged,
        request,
        destination,
        municipality,
        revalidated: None,
    };
    if let Plan::Revalidate {
        guardian_id,
        capture_id,
    } = &decided
    {
        let device = auth::device_id(&headers)?;
        let mut capture =
            usable_capture(&state, &device, capture_id, Purpose::Revalidation).await?;
        if capture.volunteer_id.as_deref() != Some(volunteer.id.as_str())
            || capture.shelter_id.as_deref() != Some(shelter_id.as_str())
            || capture.expected_person_id.as_deref() != Some(guardian_id.as_str())
        {
            return Err(invalid("wrong_capture_target"));
        }
        let resolved = resolve_person(&state, capture_id, &mut capture).await?;
        if &resolved != guardian_id {
            record_failed_revalidation(&state, &exit, capture_id, guardian_id, &resolved).await?;
            return Err(ApiError::revalidation_failed());
        }
        exit.revalidated = Some(resolved);
    }

    let left_at = store::transact(&state.db, |db, tx| {
        let exit = exit.clone();
        Box::pin(async move { close_stay(&db, tx, &exit).await })
    })
    .await?;
    members::touch_shelter_event(&state, &shelter_id).await;
    Ok(Json(json!({
        "person_id": exit.request.person_id,
        "left_at": left_at.to_string(),
        "exit_destination": destination,
    })))
}
