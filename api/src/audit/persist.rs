//! Writing the audit entry in the transaction of the write it describes, and the informational
//! anomaly alerts raised after it.

use firestore::{FirestoreDb, FirestoreTransaction, FirestoreWritePrecondition};

use super::domain::{MAX_TRACKED, WINDOW_SECS, beyond_all, tracker};
use super::model::{Activity, AuditEntry, COLLECTION, LegalBasis};
use crate::alerts::{self, AlertDoc, AlertKind};
use crate::auth::AppState;
use crate::error::ApiError;
use crate::store::{self, fs_error};

/// Adds the entry to a transaction, so it commits together with the write it describes.
pub fn add(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    entry: &AuditEntry,
) -> Result<(), ApiError> {
    // Spec 10.1.1: every entry names its legal basis; the test suite trips on one that does not.
    debug_assert!(
        entry.legal_basis.is_some(),
        "audit entry {} has no legal basis",
        entry.action
    );
    db.fluent()
        .update()
        .in_col(COLLECTION)
        .precondition(FirestoreWritePrecondition::Exists(false))
        .document_id(uuid::Uuid::new_v4().to_string())
        .object(entry)
        .add_to_transaction(tx)
        .map_err(fs_error)?;
    Ok(())
}

/// Test helper: a standalone entry.
#[cfg(test)]
pub(super) async fn write(db: &FirestoreDb, entry: &AuditEntry) -> Result<(), ApiError> {
    db.fluent()
        .insert()
        .into(COLLECTION)
        .document_id(uuid::Uuid::new_v4().to_string())
        .object(entry)
        .execute::<()>()
        .await
        .map_err(fs_error)
}

/// The points of the shelters where the volunteer is a member, read at most once per window.
async fn shelter_points(state: &AppState, volunteer_id: &str, now: i64) -> Option<Vec<(f64, f64)>> {
    if let Some((read_at, points)) = tracker().areas.get(volunteer_id) {
        if now - read_at < WINDOW_SECS {
            return Some(points.clone());
        }
    }
    let mut points = Vec::new();
    for membership in crate::members::active_of_volunteer(&state.db, volunteer_id)
        .await
        .ok()?
    {
        let shelter: Option<crate::shelters::ShelterDoc> = store::get(
            &state.db,
            crate::shelters::COLLECTION,
            &membership.shelter_id,
        )
        .await
        .ok()?;
        points.extend(shelter.map(|s| (s.lat, s.lon)));
    }
    let mut tracker = tracker();
    if tracker.areas.len() > MAX_TRACKED {
        tracker
            .areas
            .retain(|_, (read_at, _)| now - *read_at < WINDOW_SECS);
    }
    tracker
        .areas
        .insert(volunteer_id.to_string(), (now, points.clone()));
    Some(points)
}

/// Writes an informational alert (spec 4.10) and its audit entry in one transaction.
async fn raise(state: &AppState, mut alert: AlertDoc, volunteer_id: &str) {
    alert.informational = true;
    alert.volunteer_ids = vec![volunteer_id.to_string()];
    let entry = AuditEntry::new(
        format!("alert.{}", alert.kind.name()),
        &alert.target_kind,
        &alert.target_id,
    )
    .basis(LegalBasis::PublicPolicy);
    let written = store::transact(&state.db, |db, tx| {
        let (alert, entry) = (alert.clone(), entry.clone());
        Box::pin(async move {
            alerts::add(&db, tx, &alert)?;
            add(&db, tx, &entry)
        })
    })
    .await;
    if let Err(err) = written {
        tracing::warn!(?err, kind = ?alert.kind, "anomaly alert was not written");
    }
}

/// Records that `volunteer_id` did `activity` and raises the informational anomalies of spec 4.10
/// (`anomaly_volume`, `anomaly_distance`, `out_of_area`: `informational = true`, for validators
/// and the Civil Defense, never in the admins' box). `at` is the point of a rescue (the shelter of
/// a check-in); `person_id` is who was registered. Best effort, after the write: it never fails
/// the request. Called by `people::post_checkin` and `rescues::post_rescue`.
pub async fn note_activity(
    state: &AppState,
    volunteer_id: &str,
    activity: Activity,
    person_id: &str,
    at: Option<(f64, f64)>,
    event_id: Option<&str>,
) {
    note_activity_at(
        state,
        store::now().as_second(),
        volunteer_id,
        activity,
        person_id,
        at,
        event_id,
    )
    .await;
}

pub(super) async fn note_activity_at(
    state: &AppState,
    now: i64,
    volunteer_id: &str,
    activity: Activity,
    person_id: &str,
    at: Option<(f64, f64)>,
    event_id: Option<&str>,
) {
    let config = &state.config;
    let in_event = |alert: AlertDoc| match event_id {
        Some(event_id) => alert.in_event(event_id),
        None => alert,
    };
    let (limit, label) = match activity {
        Activity::Checkin => (config.anomaly_checkins_per_10min, "checkin"),
        Activity::Rescue => (config.anomaly_rescues_per_10min, "rescue"),
    };
    // Each lock is released before the next await.
    let over = tracker().count(volunteer_id, activity, now, limit);
    if let Some(count) = over {
        let detail = serde_json::json!({
            "activity": label, "count": count, "limit": limit, "window_minutes": WINDOW_SECS / 60,
        });
        let alert = AlertDoc::new(AlertKind::AnomalyVolume, "volunteer", volunteer_id, detail);
        raise(state, in_event(alert), volunteer_id).await;
    }
    let Some(at) = at else { return };
    let far = tracker().moved(person_id, at, now);
    if let Some((km, minutes)) = far {
        let detail = serde_json::json!({ "distance_km": km.round(), "minutes_apart": minutes });
        let alert = AlertDoc::new(AlertKind::AnomalyDistance, "person", person_id, detail);
        raise(state, in_event(alert), volunteer_id).await;
    }
    if activity != Activity::Rescue {
        return;
    }
    let radius_km = f64::from(config.event_radius_km);
    let Some(points) = shelter_points(state, volunteer_id, now).await else {
        return;
    };
    let Some(km) = beyond_all(&points, at, radius_km) else {
        return;
    };
    let first = tracker().first_out_of_area(volunteer_id, now);
    if first {
        let detail = serde_json::json!({ "distance_km": km.round(), "radius_km": radius_km });
        let alert = AlertDoc::new(AlertKind::OutOfArea, "volunteer", volunteer_id, detail);
        raise(state, in_event(alert), volunteer_id).await;
    }
}
