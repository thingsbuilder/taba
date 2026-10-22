//! Reads and writes of the people routes. Each transaction reads what it needs, hands it to
//! `domain`, and writes the result and the effects the domain returned (alert, audit entry).

use axum::http::StatusCode;
use firestore::{FirestoreDb, FirestoreInstant, FirestoreTransaction, path};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::domain::{self, Checkin, Edit, PersonData, Registered};
use super::model::{COLLECTION, Level, PersonDoc, person_not_found};
use super::view::viewer_shelter;
use crate::alerts::{self, AlertDoc, AlertKind};
use crate::audit::{self, AuditEntry};
use crate::auth::{self, AppState, CaptureDoc, Volunteer};
use crate::children;
use crate::error::ApiError;
use crate::households::{self, HouseholdDoc};
use crate::shelters::{self, ShelterDoc};
use crate::store::{self, fs_error, tx_delete, tx_replace};

pub(super) const CAPTURES: &str = "captures";

/// A page of one shelter's people. Past it the answer says `has_more`; the list is never
/// silently cut.
pub(super) const LIST_LIMIT: u32 = 1000;

/// References one late-biometrics fusion rewrites in its single transaction: the children that
/// list the old record as guardian, plus the deliveries and the rescues that point at it.
/// Firestore bounds a transaction by request size (10 MiB) and time, and older documentation
/// by 500 writes; this stays under 500 with room for the fixed writes (person, households,
/// capture, alert, audit). Above it the fusion is refused with `merge_too_large` before anything
/// is written, because a fusion in several transactions would leave references that point at
/// a record that no longer exists.
pub(super) const MAX_MERGE_REFERENCES: usize = 450;

fn merge_too_large() -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "merge_too_large");
    error.detail = json!({ "limit": MAX_MERGE_REFERENCES });
    error
}

pub(super) async fn load_shelter(db: &FirestoreDb, id: &str) -> Result<ShelterDoc, ApiError> {
    store::get(db, shelters::COLLECTION, id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found"))
}

// ---- lists ----

/// One page of the shelter's people, most recently updated first, and whether there are more.
/// With `needs_selfie_only` the database does the filtering, so the page is full of the people
/// asked for and `has_more` is about them: `assisted`, and `no_biometrics` who did not refuse
/// the selfie (spec 8.5).
pub(super) async fn people_of(
    db: &FirestoreDb,
    shelter_id: &str,
    needs_selfie_only: bool,
    limit: u32,
) -> Result<(Vec<PersonDoc>, bool), ApiError> {
    let rows: Vec<PersonDoc> = db
        .fluent()
        .select()
        .from(COLLECTION)
        .filter(|q| {
            let here = q.field(path!(PersonDoc::current_shelter_id)).eq(shelter_id);
            if !needs_selfie_only {
                return q.for_all([here]);
            }
            q.for_all([
                here,
                q.for_any([
                    q.field(path!(PersonDoc::level)).eq("assisted"),
                    q.for_all([
                        q.field(path!(PersonDoc::level)).eq("no_biometrics"),
                        q.field(path!(PersonDoc::biometrics_declined)).eq(false),
                    ]),
                ]),
            ])
        })
        .order_by([(
            path!(PersonDoc::updated_at),
            firestore::FirestoreQueryDirection::Descending,
        )])
        .limit(limit + 1)
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    Ok(domain::page(rows, limit as usize))
}

// ---- who sees a person ----

/// Whether a child is being attended: an open `child_separated` alert is about them (spec 4.11).
async fn child_alert_open(
    state: &AppState,
    person: &PersonDoc,
    person_id: &str,
) -> Result<bool, ApiError> {
    if !person.is_child {
        return Ok(false);
    }
    let alerts: Vec<AlertDoc> = state
        .db
        .fluent()
        .select()
        .from(alerts::COLLECTION)
        .filter(|q| {
            q.for_all([
                q.field(path!(AlertDoc::target_id)).eq(person_id),
                q.field(path!(AlertDoc::resolved)).eq(false),
            ])
        })
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    Ok(alerts.iter().any(|a| a.kind == AlertKind::ChildSeparated))
}

/// The person and the shelter through which the viewer sees them. Members of a shelter of the
/// person see them. With `attending`, protection and the Civil Defense also see a child while
/// an open `child_separated` alert is about them, whatever the shelter (spec 4.11); the shelter
/// is then the child's current one, and the viewer is not a member of it.
/// A person someone is not allowed to see is "not found": the route does not confirm who exists.
pub(super) async fn visible_person(
    state: &AppState,
    volunteer: &Volunteer,
    person_id: &str,
    attending: bool,
) -> Result<(PersonDoc, String), ApiError> {
    let person: PersonDoc = store::get(&state.db, COLLECTION, person_id)
        .await?
        .ok_or_else(person_not_found)?;
    if let Some(shelter) = viewer_shelter(&person, volunteer) {
        return Ok((person, shelter));
    }
    if attending
        && children::can_change_status(volunteer)
        && child_alert_open(state, &person, person_id).await?
    {
        let shelter = person
            .current_shelter_id
            .clone()
            .or_else(|| person.stays.last().map(|s| s.shelter_id.clone()))
            .unwrap_or_default();
        return Ok((person, shelter));
    }
    // A rescued person with no shelter yet is seen by the volunteer who registered the rescue
    // (spec 4.6); there is no shelter, so the empty string stands for none.
    if crate::rescues::registered_rescue_of(&state.db, volunteer, &person).await? {
        return Ok((person, String::new()));
    }
    Err(person_not_found())
}

// ---- check-in ----

/// Marks the capture as used by `person_id`; a capture used before is a `409`.
async fn consume_capture(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    capture_id: &str,
    person_id: &str,
) -> Result<(), ApiError> {
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
        &auth::consumed(capture, person_id),
    )
}

/// One transaction on `people/{person_id}`: consume the capture, create the person or raise
/// them, move the stay, write the audit entry. Returns the person and whether it is new.
pub(super) async fn checkin(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    c: &Checkin,
) -> Result<(PersonDoc, bool), ApiError> {
    let existing: Option<PersonDoc> = store::get(db, COLLECTION, &c.person_id).await?;
    if let Some(capture_id) = &c.capture_id {
        consume_capture(db, tx, capture_id, &c.person_id).await?;
    }
    let Registered {
        person,
        is_new,
        child_alert,
    } = domain::register(existing, c, store::now())?;
    tx_replace(db, tx, COLLECTION, &c.person_id, &person)?;
    crate::matching::on_person_registered(db, tx, &person, &c.person_id, Some(&c.shelter)).await?;
    // A child who arrives separated or alone raises the alert for the protection queue (spec 3.4.1).
    if let Some(status) = child_alert {
        children::raise_child_separated(db, tx, &c.person_id, &c.shelter_id, &c.shelter, status)
            .await?;
    }
    let mut entry = AuditEntry::new(super::ACTION_CHECKIN, "person", &c.person_id)
        .by(&c.volunteer_id)
        .in_shelter(&c.shelter_id)
        .taken_over_from(c.taken_over_from.as_deref())
        .basis(c.legal_basis);
    if let Some(device) = &c.device {
        entry = entry.on_device(device);
    }
    audit::add(db, tx, &entry)?;
    Ok((person, is_new))
}

// ---- edit ----

/// One transaction: re-reads the person, applies the edit, writes it with the audit entry.
pub(super) async fn patch(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    person_id: &str,
    data: &PersonData,
    edited_at: FirestoreInstant,
    may_alter_child: bool,
    entry: &AuditEntry,
) -> Result<PersonDoc, ApiError> {
    let mut person: PersonDoc = store::get(db, COLLECTION, person_id)
        .await?
        .ok_or_else(person_not_found)?;
    let child_alert =
        match domain::edit(&mut person, data, edited_at, may_alter_child, store::now())? {
            Edit::Stale => return Ok(person),
            Edit::Applied { child_alert } => child_alert,
        };
    tx_replace(db, tx, COLLECTION, person_id, &person)?;
    // A change to separated or unaccompanied raises the alert too (spec 3.4.1, item 4).
    if let (Some(status), Some(shelter_id)) = (child_alert, person.current_shelter_id.as_deref()) {
        if let Some(shelter) =
            store::get::<ShelterDoc>(db, shelters::COLLECTION, shelter_id).await?
        {
            children::raise_child_separated(db, tx, person_id, shelter_id, &shelter, status)
                .await?;
        }
    }
    audit::add(db, tx, entry)?;
    Ok(person)
}

// ---- late biometrics (spec 4.12) ----

/// Documents of later phases that point at a person; both keep the person in `person_id`.
const PERSON_REFERENCES: [&str; 2] = ["deliveries", "rescues"];

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct PersonRef {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    pub person_id: String,
}

/// Points every `deliveries.person_id` and `rescues.person_id` that holds `old` at `new`, and
/// returns how many it moved. At most `budget` may be moved; one more is `merge_too_large`.
/// The collections come to exist in phases 4 and 5; until then the queries find nothing.
pub(super) async fn repoint_person_refs(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    old: &str,
    new: &str,
    budget: usize,
) -> Result<usize, ApiError> {
    let mut moved = 0;
    for collection in PERSON_REFERENCES {
        let found: Vec<PersonRef> = db
            .fluent()
            .select()
            .from(collection)
            .filter(|q| q.for_all([q.field("person_id").eq(old)]))
            .limit(u32::try_from(budget - moved + 1).map_err(|_| merge_too_large())?)
            .obj()
            .query()
            .await
            .map_err(fs_error)?;
        if moved + found.len() > budget {
            return Err(merge_too_large());
        }
        for reference in found {
            let Some(id) = reference.id else {
                tracing::error!(collection, "integrity: a reference came back without an id");
                return Err(ApiError::internal());
            };
            db.fluent()
                .update()
                .fields(["person_id"])
                .in_col(collection)
                .document_id(&id)
                .object(&PersonRef {
                    id: None,
                    person_id: new.into(),
                })
                .add_to_transaction(tx)
                .map_err(fs_error)?;
            moved += 1;
        }
    }
    Ok(moved)
}

#[derive(Clone)]
pub(super) struct Migration {
    pub old_id: String,
    pub new_id: String,
    pub level: Level,
    pub capture_id: Option<String>,
    pub volunteer_id: String,
    pub device: Option<String>,
    /// None for a rescued person with no shelter.
    pub shelter_id: Option<String>,
    pub taken_over_from: Option<String>,
}

async fn household_of(
    db: &FirestoreDb,
    id: Option<&String>,
) -> Result<Option<HouseholdDoc>, ApiError> {
    match id {
        Some(id) => store::get(db, households::COLLECTION, id).await,
        None => Ok(None),
    }
}

/// One transaction (spec 4.12): the record under the token is created or merged, every
/// reference moves to it, the old record goes away, and the capture is consumed. Either all of
/// it is written or none: past `MAX_MERGE_REFERENCES` references the fusion is refused
/// before any write is sent, and a transaction Firestore aborts writes nothing.
pub(super) async fn migrate(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    m: &Migration,
) -> Result<PersonDoc, ApiError> {
    let old: PersonDoc = store::get(db, COLLECTION, &m.old_id)
        .await?
        .ok_or_else(person_not_found)?;
    let existing: Option<PersonDoc> = store::get(db, COLLECTION, &m.new_id).await?;
    let old_household = household_of(db, old.household_id.as_ref()).await?;
    let new_household = match existing.as_ref().and_then(|p| p.household_id.as_ref()) {
        Some(id) if Some(id) != old.household_id.as_ref() => household_of(db, Some(id)).await?,
        _ => None,
    };
    let capture = match &m.capture_id {
        Some(capture_id) => {
            let capture: CaptureDoc = store::get(db, CAPTURES, capture_id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "capture_not_found"))?;
            if capture.consumed {
                return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
            }
            Some((capture_id, capture))
        }
        None => None,
    };
    let shelter: Option<ShelterDoc> = match &m.shelter_id {
        Some(id) => store::get(db, shelters::COLLECTION, id).await?,
        None => None,
    };
    let now = store::now();
    let plan = domain::plan_migration(
        (&m.old_id, &m.new_id),
        m.level,
        &old,
        existing,
        (old_household, new_household),
        now,
    );
    let domain::MigrationPlan {
        person,
        old_household,
        new_household,
        household_conflict,
        child_alert,
    } = plan;

    for (id, household) in old_household.iter().chain(&new_household) {
        tx_replace(db, tx, households::COLLECTION, id, household)?;
    }
    // Every child who lists the old record as guardian, in any household or none (spec 4.12).
    let children: Vec<PersonDoc> = db
        .fluent()
        .select()
        .from(COLLECTION)
        .filter(|q| {
            q.for_all([q
                .field(path!(PersonDoc::guardian_ids))
                .array_contains(&m.old_id)])
        })
        .limit(u32::try_from(MAX_MERGE_REFERENCES + 1).map_err(|_| merge_too_large())?)
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    if children.len() > MAX_MERGE_REFERENCES {
        return Err(merge_too_large());
    }
    let mut budget = MAX_MERGE_REFERENCES;
    for mut child in children {
        let id = child.stored_id()?.to_string();
        if id == m.old_id || id == m.new_id {
            continue;
        }
        if domain::repoint_guardians(&mut child.guardians, &m.old_id, &m.new_id) {
            child.sync_guardian_ids();
            tx_replace(db, tx, COLLECTION, &id, &child)?;
            budget -= 1;
        }
    }
    repoint_person_refs(db, tx, &m.old_id, &m.new_id, budget).await?;

    if let Some((capture_id, capture)) = capture {
        tx_replace(
            db,
            tx,
            CAPTURES,
            capture_id,
            &auth::consumed(capture, &m.new_id),
        )?;
    }
    tx_delete(db, tx, COLLECTION, &m.old_id)?;
    tx_replace(db, tx, COLLECTION, &m.new_id, &person)?;
    crate::matching::on_person_registered(db, tx, &person, &m.new_id, shelter.as_ref()).await?;
    if let (Some(status), Some(shelter_id), Some(shelter)) = (child_alert, &m.shelter_id, &shelter)
    {
        children::raise_child_separated(db, tx, &m.new_id, shelter_id, shelter, status).await?;
    }
    if let (Some((old_household_id, new_household_id)), Some(shelter)) =
        (household_conflict, &shelter)
    {
        let mut alert = AlertDoc::new(
            AlertKind::HouseholdConflict,
            "person",
            &m.new_id,
            json!({ "old_household_id": old_household_id, "household_id": new_household_id }),
        )
        .for_shelters(m.shelter_id.iter().cloned().collect());
        if let Some(event_id) = &shelter.event_id {
            alert = alert.in_event(event_id);
        }
        alerts::add(db, tx, &alert)?;
    }
    audit::add(db, tx, &migration_entry(m, &person))?;
    Ok(person)
}

fn migration_entry(m: &Migration, person: &PersonDoc) -> AuditEntry {
    let mut entry = AuditEntry::new(super::ACTION_BIOMETRICS, "person", &m.new_id)
        .by(&m.volunteer_id)
        .taken_over_from(m.taken_over_from.as_deref())
        .basis(person.legal_basis);
    if let Some(shelter_id) = &m.shelter_id {
        entry = entry.in_shelter(shelter_id);
    }
    if let Some(device) = &m.device {
        entry = entry.on_device(device);
    }
    entry
}

/// The record already lives under the token: only the level can rise.
pub(super) async fn raise(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    m: &Migration,
) -> Result<PersonDoc, ApiError> {
    let mut person: PersonDoc = store::get(db, COLLECTION, &m.new_id)
        .await?
        .ok_or_else(person_not_found)?;
    if let Some(capture_id) = &m.capture_id {
        consume_capture(db, tx, capture_id, &m.new_id).await?;
    }
    domain::raise_level(&mut person, m.level, store::now());
    tx_replace(db, tx, COLLECTION, &m.new_id, &person)?;
    let mut entry = AuditEntry::new(super::ACTION_BIOMETRICS, "person", &m.new_id)
        .by(&m.volunteer_id)
        .taken_over_from(m.taken_over_from.as_deref())
        .basis(person.legal_basis);
    if let Some(shelter_id) = &m.shelter_id {
        entry = entry.in_shelter(shelter_id);
    }
    audit::add(db, tx, &entry)?;
    Ok(person)
}
