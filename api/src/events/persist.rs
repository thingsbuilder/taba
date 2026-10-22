//! Reads and writes around the rules: which event a new shelter joins (spec 4.3.1), the gate that
//! a closed event puts on writes (spec 4.3 item 5) and the record of an event's activity (spec 4.3.3).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use firestore::{FirestoreDb, FirestoreInstant};
use serde::{Deserialize, Serialize};

use super::model::{COLLECTION, EventDoc, EventStatus};
use crate::auth::AppState;
use crate::error::ApiError;
use crate::geo;
use crate::shelters::{self, Address, ShelterDoc};
use crate::store;

/// `last_activity_at` is written at most this often per event (spec 4.3.3).
const ACTIVITY_INTERVAL: Duration = Duration::from_secs(3600);

/// Where a new shelter goes (spec 4.3.1).
pub struct Assignment {
    pub event_id: String,
    /// Set when no active event was in range and a new one has to be written.
    pub new_event: Option<EventDoc>,
    /// Every active event in range, nearest shelter's first; more than one means an overlap.
    pub in_range: Vec<String>,
}

/// Looks for open shelters of active events within `radius_km` of the point (reads through
/// `db`, so inside a transaction they belong to it). Zero events: a new one. One: join it.
/// Two or more: join the event of the nearest shelter and report the overlap, without merging.
pub async fn assign(
    db: &FirestoreDb,
    lat: f64,
    lon: f64,
    address: &Address,
    radius_km: u32,
    now: FirestoreInstant,
) -> Result<Assignment, ApiError> {
    let mut in_range: Vec<String> = Vec::new();
    // Event ids already read, active or not: many shelters of one event cost one read.
    let mut checked: HashSet<String> = HashSet::new();
    for near in geo::open_shelters_within(db, lat, lon, f64::from(radius_km) * 1000.0).await? {
        let Some(event_id) = near.shelter.event_id else {
            continue;
        };
        if !checked.insert(event_id.clone()) {
            continue;
        }
        let event: Option<EventDoc> = store::get(db, COLLECTION, &event_id).await?;
        if event.is_some_and(|e| e.status == EventStatus::Active) {
            in_range.push(event_id);
        }
    }
    Ok(match in_range.first() {
        Some(nearest) => Assignment {
            event_id: nearest.clone(),
            new_event: None,
            in_range,
        },
        None => Assignment {
            event_id: uuid::Uuid::new_v4().to_string(),
            new_event: Some(EventDoc::new(address, now)),
            in_range,
        },
    })
}

/// Gate for the writes of a shelter (spec 4.3 item 5): an event that is closed takes no writes, so
/// the shelters of a closed event answer `409 event_closed`. Every write route that names a
/// shelter calls it first; inside a transaction, pass the transaction's `db` so the read conflicts
/// with the task that closes the event. An event that is missing does not block.
pub async fn ensure_open(db: &FirestoreDb, event_id: Option<&str>) -> Result<(), ApiError> {
    let Some(event_id) = event_id else {
        return Ok(());
    };
    let event: Option<EventDoc> = store::get(db, COLLECTION, event_id).await?;
    match event {
        Some(event) if event.status != EventStatus::Active => Err(ApiError::event_closed()),
        _ => Ok(()),
    }
}

/// `ensure_open` for the event of a shelter; a shelter that is missing does not block.
pub async fn ensure_shelter_open(db: &FirestoreDb, shelter_id: &str) -> Result<(), ApiError> {
    if shelter_id.is_empty() {
        return Ok(());
    }
    let shelter: Option<ShelterDoc> = store::get(db, shelters::COLLECTION, shelter_id).await?;
    ensure_open(db, shelter.and_then(|s| s.event_id).as_deref()).await
}

/// Whether the event's activity should be written now, and if so, remembers it.
// The "written within the hour" memory is per process and is lost on restart. Limit: one extra
// write per event after a restart, and N writes an hour per event with N instances (the deployment
// runs one). Nothing is lost either way; `last_activity_at` on the event document would share the
// limit between instances.
pub(super) fn activity_due(state: &AppState, event_id: &str, now: Instant) -> bool {
    let mut seen = state
        .event_activity
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match seen.get(event_id) {
        Some(last) if now.duration_since(*last) < ACTIVITY_INTERVAL => false,
        _ => {
            seen.insert(event_id.into(), now);
            true
        }
    }
}

/// Records activity on the event after a write to one of its shelters, at most once an hour
/// per event (spec 4.3.3). A failure is logged: the write it follows already succeeded.
pub async fn touch(state: &AppState, event_id: Option<&str>) {
    let Some(event_id) = event_id else { return };
    if !activity_due(state, event_id, Instant::now()) {
        return;
    }
    #[derive(Serialize, Deserialize)]
    struct Activity {
        #[serde(with = "firestore::serialize_as_timestamp")]
        last_activity_at: FirestoreInstant,
    }
    let written = state
        .db
        .fluent()
        .update()
        .fields(["last_activity_at"])
        .in_col(COLLECTION)
        .precondition(firestore::FirestoreWritePrecondition::Exists(true))
        .document_id(event_id)
        .object(&Activity {
            last_activity_at: store::now(),
        })
        .execute::<()>()
        .await;
    if let Err(err) = written {
        tracing::warn!(%err, event_id, "could not record event activity");
    }
}
