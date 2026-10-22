//! Audit log (spec 4.10, 7): every write from phase 1 on records an `audit_log` entry.
//! Entries carry ids only, never a name or a photo. `expires_at` is one year ahead (the TTL
//! policy is in `firestore.indexes.json`). The informational anomaly alerts of spec 4.10 follow.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};

use firestore::{FirestoreDb, FirestoreInstant, FirestoreTransaction, FirestoreWritePrecondition};
use serde::{Deserialize, Serialize};

use crate::alerts::{self, AlertDoc, AlertKind};
use crate::auth::AppState;
use crate::error::ApiError;
use crate::geo;
use crate::store::{self, fs_error};

const COLLECTION: &str = "audit_log";
const RETENTION_SECONDS: i64 = 365 * 24 * 3600;

/// `audit_log.legal_basis` (spec 10.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegalBasis {
    Consent,
    VitalInterest,
    FraudPrevention,
    PublicPolicy,
    ChildProtection,
    FamilyConsent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub at: FirestoreInstant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volunteer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shelter_id: Option<String>,
    /// The volunteer whose queued item an admin took over (spec 9.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken_over_from: Option<String>,
    pub action: String,
    pub target_kind: String,
    pub target_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legal_basis: Option<LegalBasis>,
    /// What an operation that changes many documents changed, by collection (the discard); never ids of people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<std::collections::BTreeMap<String, u32>>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub expires_at: FirestoreInstant,
}

impl AuditEntry {
    pub fn new(action: impl Into<String>, target_kind: &str, target_id: &str) -> Self {
        let at = crate::store::now();
        Self {
            at,
            volunteer_id: None,
            device_id: None,
            shelter_id: None,
            taken_over_from: None,
            action: action.into(),
            target_kind: target_kind.into(),
            target_id: target_id.into(),
            legal_basis: None,
            counts: None,
            expires_at: at + firestore::jiff::SignedDuration::from_secs(RETENTION_SECONDS),
        }
    }

    pub fn by(mut self, volunteer_id: &str) -> Self {
        self.volunteer_id = Some(volunteer_id.into());
        self
    }

    pub fn on_device(mut self, device_id: &str) -> Self {
        self.device_id = Some(device_id.into());
        self
    }

    pub fn in_shelter(mut self, shelter_id: &str) -> Self {
        self.shelter_id = Some(shelter_id.into());
        self
    }

    pub fn with_counts(mut self, counts: std::collections::BTreeMap<String, u32>) -> Self {
        self.counts = Some(counts);
        self
    }

    pub fn taken_over_from(mut self, volunteer_id: Option<&str>) -> Self {
        self.taken_over_from = volunteer_id.map(str::to_string);
        self
    }

    pub fn basis(mut self, basis: LegalBasis) -> Self {
        self.legal_basis = Some(basis);
        self
    }
}

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
pub async fn write(db: &FirestoreDb, entry: &AuditEntry) -> Result<(), ApiError> {
    db.fluent()
        .insert()
        .into(COLLECTION)
        .document_id(uuid::Uuid::new_v4().to_string())
        .object(entry)
        .execute::<()>()
        .await
        .map_err(fs_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_db;

    #[tokio::test]
    async fn entry_is_stored_with_a_one_year_timestamp_ttl() {
        let db = test_db().await;
        let entry = AuditEntry::new("session.create", "session", "abc")
            .by("vol-1")
            .on_device("dev-1")
            .in_shelter("shelter-1")
            .basis(LegalBasis::FraudPrevention);
        write(&db, &entry).await.unwrap();

        let stored: Vec<AuditEntry> = db
            .fluent()
            .select()
            .from(COLLECTION)
            .obj()
            .query()
            .await
            .unwrap();
        assert_eq!(stored.len(), 1);
        let got = &stored[0];
        assert_eq!(got.action, "session.create");
        assert_eq!(got.volunteer_id.as_deref(), Some("vol-1"));
        assert_eq!(got.device_id.as_deref(), Some("dev-1"));
        assert_eq!(got.shelter_id.as_deref(), Some("shelter-1"));
        assert_eq!(got.legal_basis, Some(LegalBasis::FraudPrevention));
        let year = got.expires_at.as_second() - got.at.as_second();
        assert_eq!(year, RETENTION_SECONDS);
    }

    #[tokio::test]
    async fn legal_basis_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&LegalBasis::FraudPrevention).unwrap(),
            "\"fraud_prevention\""
        );
    }
}

/// What an activity check looks at (spec 4.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Activity {
    Checkin,
    Rescue,
}

/// Counting window of `anomaly_volume`, and how long the shelter points of a volunteer are kept.
const WINDOW_SECS: i64 = 600;
const DISTANCE_LIMIT_KM: f64 = 50.0;
const DISTANCE_WINDOW_SECS: i64 = 3600;
/// Entries each in-memory table keeps before it drops the stale ones.
const MAX_TRACKED: usize = 10_000;

/// What the anomaly rules remember between requests.
// ponytail: per process and lost on restart, so a restart or a second instance forgets the open
// window and may repeat one alert or miss one; the pilot runs one instance. Move to a Firestore
// document with a TTL if the API ever scales out.
#[derive(Default)]
struct Tracker {
    /// `(volunteer, activity)` -> start of the window, count in it, alert already raised.
    windows: HashMap<(String, Activity), (i64, u32, bool)>,
    /// `person_id` -> where and when the person was last registered.
    last_seen: HashMap<String, (f64, f64, i64)>,
    /// Volunteer -> when their shelter points were read, and the points.
    areas: HashMap<String, (i64, Vec<(f64, f64)>)>,
    /// Volunteer -> when `out_of_area` was last raised.
    out_of_area: HashMap<String, i64>,
}

static TRACKER: LazyLock<Mutex<Tracker>> = LazyLock::new(Mutex::default);

fn tracker() -> MutexGuard<'static, Tracker> {
    TRACKER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Tracker {
    /// Counts one activity; the count that first passes `limit` in a window is returned, once.
    fn count(&mut self, volunteer: &str, activity: Activity, now: i64, limit: u32) -> Option<u32> {
        if self.windows.len() > MAX_TRACKED {
            self.windows
                .retain(|_, (start, _, _)| now - *start < WINDOW_SECS);
        }
        let entry = self
            .windows
            .entry((volunteer.to_string(), activity))
            .or_insert((now, 0, false));
        if now - entry.0 >= WINDOW_SECS {
            *entry = (now, 0, false);
        }
        entry.1 += 1;
        if entry.1 > limit && !entry.2 {
            entry.2 = true;
            return Some(entry.1);
        }
        None
    }

    /// Remembers where `person` was registered; returns the distance in km and the minutes since
    /// when the last point was farther than the limit and less than an hour old.
    fn moved(&mut self, person: &str, at: (f64, f64), now: i64) -> Option<(f64, i64)> {
        if self.last_seen.len() > MAX_TRACKED {
            self.last_seen
                .retain(|_, (_, _, seen)| now - *seen < DISTANCE_WINDOW_SECS);
        }
        let previous = self
            .last_seen
            .insert(person.to_string(), (at.0, at.1, now))?;
        let km = geo::haversine_m(previous.0, previous.1, at.0, at.1) / 1000.0;
        let seconds = now - previous.2;
        (seconds < DISTANCE_WINDOW_SECS && km > DISTANCE_LIMIT_KM).then_some((km, seconds / 60))
    }

    /// True the first time in a window that `volunteer` is out of the area.
    fn first_out_of_area(&mut self, volunteer: &str, now: i64) -> bool {
        match self.out_of_area.get(volunteer) {
            Some(last) if now - last < WINDOW_SECS => false,
            _ => {
                self.out_of_area.insert(volunteer.to_string(), now);
                true
            }
        }
    }
}

/// The distance in km to the nearest of `points` when all of them are farther than `radius_km`.
fn beyond_all(points: &[(f64, f64)], at: (f64, f64), radius_km: f64) -> Option<f64> {
    let nearest = points
        .iter()
        .map(|p| geo::haversine_m(p.0, p.1, at.0, at.1) / 1000.0)
        .min_by(f64::total_cmp)?;
    (nearest > radius_km).then_some(nearest)
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

async fn note_activity_at(
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

#[cfg(test)]
mod anomaly_tests {
    use axum::http::StatusCode;
    use serde_json::{Value, json};

    use super::*;
    use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, new_id, north_of_origin};
    use crate::volunteers::GlobalRole;

    #[test]
    fn volume_alerts_once_per_window_and_counts_again_in_the_next() {
        let mut t = Tracker::default();
        for n in 1..=30 {
            assert_eq!(t.count("v", Activity::Checkin, 1000 + n, 30), None);
        }
        assert_eq!(t.count("v", Activity::Checkin, 1031, 30), Some(31));
        assert_eq!(t.count("v", Activity::Checkin, 1032, 30), None);
        assert_eq!(t.count("v", Activity::Checkin, 1033, 30), None);
        // The window opened at 1001 closes 600 s later: a new count starts.
        for n in 0..30 {
            assert_eq!(t.count("v", Activity::Checkin, 1601 + n, 30), None);
        }
        assert_eq!(t.count("v", Activity::Checkin, 1640, 30), Some(31));
    }

    #[test]
    fn volume_counts_each_volunteer_and_each_activity_apart() {
        let mut t = Tracker::default();
        for _ in 0..30 {
            t.count("a", Activity::Checkin, 1, 30);
        }
        assert_eq!(t.count("b", Activity::Checkin, 1, 30), None);
        assert_eq!(t.count("a", Activity::Rescue, 1, 100), None);
        assert_eq!(t.count("a", Activity::Checkin, 1, 30), Some(31));
    }

    #[test]
    fn distance_needs_more_than_50_km_in_less_than_an_hour() {
        let mut t = Tracker::default();
        let (origin, within, beyond) = (
            north_of_origin(0.0),
            north_of_origin(49_000.0),
            north_of_origin(51_000.0),
        );
        assert_eq!(t.moved("p", origin, 0), None, "first sighting");
        assert_eq!(t.moved("p", within, 60), None, "49 km");
        // The last point is `within`: the origin is 49 km back, `beyond` only 2 km ahead.
        assert_eq!(t.moved("p", beyond, 120), None, "2 km from the last point");
        let (km, minutes) = t.moved("p", origin, 120 + 3599).unwrap();
        assert!((km - 51.0).abs() < 0.1, "{km}");
        assert_eq!(minutes, 59);
        assert_eq!(
            t.moved("p", beyond, 120 + 3599 + 3600),
            None,
            "an hour apart"
        );
        assert_eq!(t.moved("q", beyond, 0), None, "another person");
    }

    #[test]
    fn out_of_area_is_beyond_every_point_and_raised_once_per_window() {
        let points = [north_of_origin(0.0), north_of_origin(200_000.0)];
        let km = beyond_all(&points, north_of_origin(100_000.0), 50.0).unwrap();
        assert!((km - 100.0).abs() < 0.1);
        assert_eq!(beyond_all(&points, north_of_origin(160_000.0), 50.0), None);
        assert_eq!(beyond_all(&[], north_of_origin(0.0), 50.0), None);
        let mut t = Tracker::default();
        assert!(t.first_out_of_area("v", 10));
        assert!(!t.first_out_of_area("v", 500));
        assert!(t.first_out_of_area("v", 611));
    }

    struct World {
        app: TestApp,
        volunteer: Actor,
        admin: Actor,
        validator: Actor,
        civil_defense: Actor,
    }

    /// `volunteer` and `admin` share a confirmed shelter at the origin.
    async fn world() -> World {
        let app = TestApp::new().await;
        // The identity derives from the token, and the tracker is shared by the tests of the
        // process: every test needs volunteers of its own.
        let tag = new_id();
        let admin = app.actor(&format!("Admin-{tag}")).await;
        let shelter = app.confirmed_shelter(&admin, "Shelter", ORIGIN).await;
        let volunteer = app
            .join(
                &app.invite(&admin, &shelter, "qr", "volunteer").await,
                &format!("Vol-{tag}"),
            )
            .await;
        let validator = app.actor(&format!("Validator-{tag}")).await;
        app.grant(&validator, GlobalRole::Validator).await;
        let civil_defense = app.actor(&format!("Defense-{tag}")).await;
        app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        World {
            app,
            volunteer,
            admin,
            validator,
            civil_defense,
        }
    }

    impl World {
        async fn note(&self, activity: Activity, person: &str, at: Option<(f64, f64)>, now: i64) {
            note_activity_at(
                &self.app.state,
                now,
                &self.volunteer.id,
                activity,
                person,
                at,
                None,
            )
            .await;
        }

        async fn anomalies(&self, kind: AlertKind) -> Vec<Value> {
            all_docs(&self.app.state.db, alerts::COLLECTION)
                .await
                .into_iter()
                .filter(|a| a["kind"] == json!(kind))
                .collect()
        }

        async fn visible_to(&self, who: &Actor) -> Vec<String> {
            let (status, body) = self.app.send("GET", "/v1/alerts", &who.secret, None).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            body["alerts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a["kind"].as_str().unwrap().to_string())
                .collect()
        }
    }

    const NOW: i64 = 1_800_000_000;

    #[tokio::test]
    async fn fifty_rescues_in_ten_minutes_raise_nothing() {
        let w = world().await;
        for n in 0..50 {
            w.note(Activity::Rescue, &new_id(), Some(ORIGIN), NOW + n)
                .await;
        }
        assert!(w.anomalies(AlertKind::AnomalyVolume).await.is_empty());
    }

    #[tokio::test]
    async fn fifty_checkins_raise_one_informational_alert_only_validators_and_defense_read() {
        let w = world().await;
        for n in 0..50 {
            w.note(Activity::Checkin, &new_id(), Some(ORIGIN), NOW + n)
                .await;
        }
        let raised = w.anomalies(AlertKind::AnomalyVolume).await;
        assert_eq!(raised.len(), 1, "one alert per window, not one per write");
        let alert = &raised[0];
        assert_eq!(alert["informational"], json!(true));
        assert_eq!(alert["target_id"], json!(w.volunteer.id));
        assert_eq!(alert["detail"]["activity"], json!("checkin"));
        assert_eq!(alert["detail"]["count"], json!(31));
        assert_eq!(alert["detail"]["limit"], json!(30));

        assert_eq!(w.visible_to(&w.civil_defense).await, ["anomaly_volume"]);
        assert_eq!(w.visible_to(&w.validator).await, ["anomaly_volume"]);
        // Not in the shelter admin's box, nor in the box of the volunteer it is about.
        assert!(w.visible_to(&w.admin).await.is_empty());
        assert!(w.visible_to(&w.volunteer).await.is_empty());
    }

    #[tokio::test]
    async fn a_new_window_can_raise_the_volume_alert_again() {
        let w = world().await;
        for round in 0..2 {
            for n in 0..35 {
                w.note(Activity::Checkin, &new_id(), None, NOW + round * 700 + n)
                    .await;
            }
        }
        assert_eq!(w.anomalies(AlertKind::AnomalyVolume).await.len(), 2);
    }

    #[tokio::test]
    async fn the_same_person_far_away_within_an_hour_raises_anomaly_distance() {
        let w = world().await;
        let person = new_id();
        w.note(Activity::Checkin, &person, Some(ORIGIN), NOW).await;
        let at = Some(north_of_origin(40_000.0));
        w.note(Activity::Rescue, &person, at, NOW + 60).await;
        assert!(w.anomalies(AlertKind::AnomalyDistance).await.is_empty());
        let at = Some(north_of_origin(100_000.0));
        w.note(Activity::Rescue, &person, at, NOW + 600).await;
        let raised = w.anomalies(AlertKind::AnomalyDistance).await;
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0]["informational"], json!(true));
        assert_eq!(raised[0]["target_id"], json!(person));
        assert_eq!(raised[0]["detail"]["distance_km"], json!(60.0));
        // The 100 km rescue is also out of the area of the volunteer's shelter.
        let seen = w.visible_to(&w.validator).await;
        assert!(seen.contains(&"anomaly_distance".to_string()), "{seen:?}");
        assert!(w.visible_to(&w.admin).await.is_empty());
    }

    #[tokio::test]
    async fn a_rescue_beyond_the_radius_of_every_shelter_raises_out_of_area_once() {
        let w = world().await;
        let at = Some(north_of_origin(10_000.0));
        w.note(Activity::Rescue, &new_id(), at, NOW).await;
        assert!(w.anomalies(AlertKind::OutOfArea).await.is_empty());
        let at = Some(north_of_origin(120_000.0));
        w.note(Activity::Rescue, &new_id(), at, NOW + 1).await;
        let at = Some(north_of_origin(130_000.0));
        w.note(Activity::Rescue, &new_id(), at, NOW + 2).await;
        let raised = w.anomalies(AlertKind::OutOfArea).await;
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0]["informational"], json!(true));
        assert_eq!(raised[0]["detail"]["radius_km"], json!(50.0));
        assert_eq!(w.visible_to(&w.civil_defense).await, ["out_of_area"]);
        assert!(w.visible_to(&w.admin).await.is_empty());
    }

    #[tokio::test]
    async fn a_check_in_is_never_out_of_area_and_a_volunteer_without_shelter_is_skipped() {
        let w = world().await;
        let far = Some(north_of_origin(500_000.0));
        w.note(Activity::Checkin, &new_id(), far, NOW).await;
        let loner = w.app.actor(&format!("Loner-{}", new_id())).await;
        note_activity_at(
            &w.app.state,
            NOW,
            &loner.id,
            Activity::Rescue,
            &new_id(),
            far,
            None,
        )
        .await;
        assert!(w.anomalies(AlertKind::OutOfArea).await.is_empty());
    }

    #[tokio::test]
    async fn each_alert_leaves_an_audit_entry_with_a_legal_basis_and_the_event() {
        let w = world().await;
        for n in 0..31 {
            note_activity_at(
                &w.app.state,
                NOW + n,
                &w.volunteer.id,
                Activity::Checkin,
                &new_id(),
                None,
                Some("event-1"),
            )
            .await;
        }
        let alert = &w.anomalies(AlertKind::AnomalyVolume).await[0];
        assert_eq!(alert["event_id"], json!("event-1"));
        let entries = all_docs(&w.app.state.db, COLLECTION).await;
        let entry = entries
            .iter()
            .find(|e| e["action"] == json!("alert.anomaly_volume"))
            .expect("audit entry of the alert");
        assert_eq!(entry["legal_basis"], json!("public_policy"));
        assert!(entry.get("expires_at").is_some());
    }
}
