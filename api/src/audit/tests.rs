use axum::http::StatusCode;
use serde_json::{Value, json};

use super::domain::{Tracker, beyond_all};
use super::model::{Activity, AuditEntry, COLLECTION, LegalBasis, RETENTION_SECONDS};
use super::persist::{note_activity_at, write};
use crate::alerts::{self, AlertKind};
use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, new_id, north_of_origin, test_db};
use crate::volunteers::GlobalRole;

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
