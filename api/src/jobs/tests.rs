use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;

use super::domain::{BATCH_WRITES, Budget, DAY_MICROS, WORK_BUDGET, plus_days};
use super::runner::{SHUTDOWN_GRACE, StepFuture, run_once, run_steps, spawn};
use crate::alerts;
use crate::auth::{CaptureDoc, CaptureStatus, Purpose};
use crate::endorsements;
use crate::endorsements::EndorsementCodeDoc;
use crate::error::ApiError;
use crate::events::{self, ClosureReason, EventDoc, EventStatus};
use crate::exits::fixtures::{person, put};
use crate::invites::{self, InviteStatus};
use crate::invites::{InviteDoc, InviteKind};
use crate::matches::{self, MatchDoc, MatchStatus};
use crate::members::ShelterRole;
use crate::people::Level;
use crate::people::{self, PersonDoc};
use crate::search_photos;
use crate::searches::{self, SearchDoc, SearchStatus};
use crate::searches::{Family, MissingPerson, SearchVia};
use crate::shelters::{self, ShelterDoc};
use crate::store;
use crate::test_support::{
    Actor, ORIGIN, TestApp, all_docs, audit_actions, new_id, north_of_origin,
};

struct World {
    app: TestApp,
    admin: Actor,
    shelter: String,
    event: String,
}

async fn world() -> World {
    let app = TestApp::new().await;
    let admin = app.actor("Admin").await;
    let shelter = app.create_shelter(&admin, "A", ORIGIN).await;
    let event = event_of(&app, &shelter).await;
    World {
        app,
        admin,
        shelter,
        event,
    }
}

async fn event_of(app: &TestApp, shelter: &str) -> String {
    let doc: ShelterDoc = store::get(&app.state.db, shelters::COLLECTION, shelter)
        .await
        .unwrap()
        .unwrap();
    doc.event_id.unwrap()
}

async fn event(app: &TestApp, id: &str) -> EventDoc {
    store::get(&app.state.db, events::COLLECTION, id)
        .await
        .unwrap()
        .unwrap()
}

fn ago(days: i64, extra_seconds: i64) -> FirestoreInstant {
    let micros = store::now().as_microsecond() - days * DAY_MICROS - extra_seconds * 1_000_000;
    FirestoreInstant::from_microsecond(micros).unwrap()
}

async fn idle_for(app: &TestApp, event_id: &str, days: i64) {
    let mut doc = event(app, event_id).await;
    doc.last_activity_at = ago(days, 0);
    store::replace(&app.state.db, events::COLLECTION, event_id, &doc)
        .await
        .unwrap();
}

async fn close_shelter(w: &World, shelter: &str) {
    let (status, body) = w
        .app
        .send(
            "POST",
            &format!("/v1/shelters/{shelter}/close"),
            &w.admin.secret,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn search(app: &TestApp, event_id: &str, owner: &str) -> String {
    let id = new_id();
    let doc = SearchDoc {
        id: None,
        created_by: owner.into(),
        via: SearchVia::Volunteer,
        origin_shelter_id: None,
        event_id: Some(event_id.into()),
        person_ids: vec![],
        photos_sent: 1,
        usable_photos: 1,
        retained_photos: 0,
        missing_person: MissingPerson {
            name: "Fulano".into(),
            nickname: None,
            notes: String::new(),
        },
        family: Family {
            name: "Familia".into(),
            phone: "+5551999990000".into(),
            relationship: "mother".into(),
            language: "pt-BR".into(),
        },
        status: SearchStatus::Active,
        expiry_notified_at: None,
        created_at: store::now(),
    };
    store::replace(&app.state.db, searches::COLLECTION, &id, &doc)
        .await
        .unwrap();
    id
}

async fn match_of(app: &TestApp, search_id: &str, status: MatchStatus) {
    let doc = MatchDoc {
        id: None,
        search_id: search_id.into(),
        person_id: new_id(),
        is_child: false,
        mediator_kind: matches::MediatorKind::Central,
        mediator_shelter_id: None,
        status,
        decided_by: None,
        decided_by_volunteer: None,
        decided_at: None,
        note: None,
        created_at: store::now(),
    };
    store::replace(&app.state.db, matches::COLLECTION, &new_id(), &doc)
        .await
        .unwrap();
}

async fn expiring_alerts(app: &TestApp) -> Vec<Value> {
    all_docs(&app.state.db, alerts::COLLECTION)
        .await
        .into_iter()
        .filter(|a| a["kind"] == "search_expiring")
        .collect()
}

async fn close_count(app: &TestApp) -> usize {
    audit_actions(&app.state.db)
        .await
        .iter()
        .filter(|a| *a == "event.close")
        .count()
}

async fn thumbnail_person(app: &TestApp, events: &[&str]) -> String {
    let id = new_id();
    let mut doc = person(Level::NoBiometrics, None);
    doc.thumbnail_encrypted = Some(vec![1, 2, 3]);
    doc.event_ids = events.iter().map(|e| e.to_string()).collect();
    put(app, &id, &doc).await;
    id
}

async fn has_thumbnail(app: &TestApp, id: &str) -> bool {
    let doc: PersonDoc = store::get(&app.state.db, people::COLLECTION, id)
        .await
        .unwrap()
        .unwrap();
    doc.thumbnail_encrypted.is_some()
}

#[tokio::test]
async fn an_event_without_writes_for_30_days_closes_with_its_purge_date() {
    let w = world().await;
    let quiet_shelter = w
        .app
        .create_shelter(&w.admin, "Far", (-23.55, -46.63))
        .await;
    let quiet = event_of(&w.app, &quiet_shelter).await;
    assert_ne!(quiet, w.event);
    idle_for(&w.app, &quiet, 31).await;
    idle_for(&w.app, &w.event, 29).await;

    run_once(&w.app.state).await.unwrap();

    let closed = event(&w.app, &quiet).await;
    let closed_at = closed.closed_at.unwrap();
    assert_eq!(
        (closed.status, closed.closure_reason),
        (EventStatus::Closed, Some(ClosureReason::Inactivity))
    );
    assert_eq!(
        closed.purge_at,
        Some(plus_days(closed_at, w.app.state.config.purge_after_days))
    );
    assert_eq!(
        closed.delivery_purge_at,
        Some(plus_days(
            closed_at,
            w.app.state.config.delivery_retention_days
        ))
    );
    assert!((store::now().as_second() - closed_at.as_second()).abs() < 60);
    let open = event(&w.app, &w.event).await;
    assert_eq!(
        (open.status, open.closed_at, open.purge_at),
        (EventStatus::Active, None, None)
    );
    assert_eq!(close_count(&w.app).await, 1);

    // The closed event takes no more writes; the open one does.
    let checkin = |shelter: &str| {
        let body = json!({ "id": new_id(), "legal_basis": "consent", "client_created_at": store::now().to_string(), "data": {}, "no_biometrics": true });
        (format!("/v1/shelters/{shelter}/checkins"), body)
    };
    let (path, body) = checkin(&quiet_shelter);
    let (status, answer) = w.app.send("POST", &path, &w.admin.secret, Some(body)).await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::CONFLICT, Some("event_closed"))
    );
    let (path, body) = checkin(&w.shelter);
    let (status, _) = w.app.send("POST", &path, &w.admin.secret, Some(body)).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn an_event_closes_when_all_its_shelters_are_closed_and_not_before() {
    let w = world().await;
    let second = w
        .app
        .create_shelter(&w.admin, "B", north_of_origin(5_000.0))
        .await;
    assert_eq!(event_of(&w.app, &second).await, w.event);

    close_shelter(&w, &w.shelter).await;
    run_once(&w.app.state).await.unwrap();
    assert_eq!(event(&w.app, &w.event).await.status, EventStatus::Active);

    close_shelter(&w, &second).await;
    run_once(&w.app.state).await.unwrap();
    let closed = event(&w.app, &w.event).await;
    assert_eq!(
        (closed.status, closed.closure_reason),
        (EventStatus::Closed, Some(ClosureReason::AllSheltersClosed))
    );
    assert!(closed.closed_at.is_some() && closed.purge_at.is_some());
}

#[tokio::test]
async fn the_family_photos_are_erased_after_the_maximum_age_even_with_no_event() {
    let w = world().await;
    let owner = w.app.actor("Owner").await;
    let old = search(&w.app, &w.event, &owner.id).await;
    let recent = search(&w.app, &w.event, &owner.id).await;
    for (id, created) in [(&old, ago(31, 0)), (&recent, ago(1, 0))] {
        let mut doc: SearchDoc = store::get(&w.app.state.db, searches::COLLECTION, id)
            .await
            .unwrap()
            .unwrap();
        // No event: nothing else would ever erase it.
        doc.event_id = None;
        doc.retained_photos = 1;
        doc.created_at = created;
        store::replace(&w.app.state.db, searches::COLLECTION, id, &doc)
            .await
            .unwrap();
        let photo = search_photos::PhotoDoc {
            search_id: id.clone(),
            photo_encrypted: Some(vec![1, 2, 3]),
            created_at: store::now(),
        };
        store::replace(
            &w.app.state.db,
            search_photos::COLLECTION,
            &search_photos::photo_id(id, 0),
            &photo,
        )
        .await
        .unwrap();
    }
    run_once(&w.app.state).await.unwrap();
    let left = search_photos::all(&w.app.state.db).await;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].search_id, recent);
    let audits = all_docs(&w.app.state.db, "audit_log").await;
    assert_eq!(
        audits
            .iter()
            .filter(|e| e["action"] == "search_photos.discard.max_age")
            .count(),
        1
    );
}

#[tokio::test]
async fn the_family_photos_are_erased_when_the_event_closes() {
    let w = world().await;
    let owner = w.app.actor("Owner").await;
    let id = search(&w.app, &w.event, &owner.id).await;
    let mut doc: SearchDoc = store::get(&w.app.state.db, searches::COLLECTION, &id)
        .await
        .unwrap()
        .unwrap();
    doc.retained_photos = 1;
    store::replace(&w.app.state.db, searches::COLLECTION, &id, &doc)
        .await
        .unwrap();
    let photo = search_photos::PhotoDoc {
        search_id: id.clone(),
        photo_encrypted: Some(vec![1, 2, 3]),
        created_at: store::now(),
    };
    store::replace(
        &w.app.state.db,
        search_photos::COLLECTION,
        &search_photos::photo_id(&id, 0),
        &photo,
    )
    .await
    .unwrap();
    run_once(&w.app.state).await.unwrap();
    assert_eq!(
        search_photos::all(&w.app.state.db).await.len(),
        1,
        "the event is open"
    );
    idle_for(&w.app, &w.event, 31).await;
    run_once(&w.app.state).await.unwrap();
    assert!(search_photos::all(&w.app.state.db).await.is_empty());
    let stored: SearchDoc = store::get(&w.app.state.db, searches::COLLECTION, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.retained_photos, 0);
}

#[tokio::test]
async fn a_second_run_changes_nothing_and_raises_no_second_alert() {
    let w = world().await;
    let owner = w.app.actor("Owner").await;
    let first = search(&w.app, &w.event, &owner.id).await;
    let second = search(&w.app, &w.event, &owner.id).await;
    idle_for(&w.app, &w.event, 31).await;

    run_once(&w.app.state).await.unwrap();
    let closed = event(&w.app, &w.event).await;
    assert_eq!(expiring_alerts(&w.app).await.len(), 2);

    run_once(&w.app.state).await.unwrap();
    let again = event(&w.app, &w.event).await;
    assert_eq!(
        (again.closed_at, again.purge_at),
        (closed.closed_at, closed.purge_at)
    );
    assert_eq!(expiring_alerts(&w.app).await.len(), 2);
    for id in [first, second] {
        let stored: SearchDoc = store::get(&w.app.state.db, searches::COLLECTION, &id)
            .await
            .unwrap()
            .unwrap();
        assert!(stored.expiry_notified_at.is_some());
        assert_eq!(stored.status, SearchStatus::Active);
    }
    assert_eq!(close_count(&w.app).await, 1);
}

#[tokio::test]
async fn two_runs_at_once_close_once_and_alert_once() {
    let w = world().await;
    let owner = w.app.actor("Owner").await;
    for _ in 0..5 {
        search(&w.app, &w.event, &owner.id).await;
    }
    idle_for(&w.app, &w.event, 31).await;
    let (a, b) = tokio::join!(run_once(&w.app.state), run_once(&w.app.state));
    a.unwrap();
    b.unwrap();
    assert_eq!(expiring_alerts(&w.app).await.len(), 5);
    assert_eq!(close_count(&w.app).await, 1);
}

#[tokio::test]
async fn only_searches_without_an_accepted_match_get_an_alert_for_whoever_registered_them() {
    let w = world().await;
    let owner = w.app.actor("Owner").await;
    let code = w.app.invite(&w.admin, &w.shelter, "qr", "volunteer").await;
    let other_owner = w.app.join(&code, "Other").await;
    let plain = search(&w.app, &w.event, &owner.id).await;
    let declined = search(&w.app, &w.event, &other_owner.id).await;
    let accepted = search(&w.app, &w.event, &owner.id).await;
    match_of(&w.app, &declined, MatchStatus::Declined).await;
    match_of(&w.app, &accepted, MatchStatus::Open).await;
    match_of(&w.app, &accepted, MatchStatus::Accepted).await;
    let closed_search = search(&w.app, &w.event, &owner.id).await;
    let mut doc: SearchDoc = store::get(&w.app.state.db, searches::COLLECTION, &closed_search)
        .await
        .unwrap()
        .unwrap();
    doc.status = SearchStatus::Closed;
    store::replace(&w.app.state.db, searches::COLLECTION, &closed_search, &doc)
        .await
        .unwrap();
    // A search of another event is not touched.
    let far_shelter = w
        .app
        .create_shelter(&w.admin, "Far", (-23.55, -46.63))
        .await;
    let far_event = event_of(&w.app, &far_shelter).await;
    search(&w.app, &far_event, &owner.id).await;
    idle_for(&w.app, &w.event, 31).await;

    run_once(&w.app.state).await.unwrap();

    let purge_at = event(&w.app, &w.event).await.purge_at.unwrap();
    let mut got: Vec<(String, String)> = expiring_alerts(&w.app)
        .await
        .iter()
        .map(|a| {
            assert_eq!(a["detail"], json!({ "purge_at": purge_at.to_string() }));
            assert_eq!(
                (
                    a["informational"].as_bool(),
                    a["for_civil_defense"].as_bool()
                ),
                (Some(true), Some(false))
            );
            assert_eq!(a["event_id"], json!(w.event));
            assert_eq!(a["target_kind"], "search");
            (
                a["target_id"].as_str().unwrap().to_string(),
                a["volunteer_ids"][0].as_str().unwrap().to_string(),
            )
        })
        .collect();
    got.sort();
    let mut expected = vec![
        (plain, owner.id.clone()),
        (declined, other_owner.id.clone()),
    ];
    expected.sort();
    assert_eq!(got, expected);

    // The volunteer reads it in the alert box; the shelter admin does not.
    let (status, mine) = w
        .app
        .send("GET", "/v1/alerts", &other_owner.secret, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{mine}");
    assert_eq!(mine["alerts"].as_array().unwrap().len(), 1);
    assert_eq!(mine["alerts"][0]["kind"], "search_expiring");
    let (_, admins) = w.app.send("GET", "/v1/alerts", &w.admin.secret, None).await;
    assert!(
        admins["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["kind"] != "search_expiring")
    );
}

#[tokio::test]
async fn a_closed_event_left_half_done_is_finished_by_the_next_run() {
    let w = world().await;
    let owner = w.app.actor("Owner").await;
    let pending = search(&w.app, &w.event, &owner.id).await;
    // The event was closed by an earlier run that stopped before the alerts.
    let mut doc = event(&w.app, &w.event).await;
    doc.status = EventStatus::Closed;
    doc.closed_at = Some(store::now());
    doc.closure_reason = Some(ClosureReason::Inactivity);
    doc.purge_at = Some(plus_days(store::now(), 30));
    store::replace(&w.app.state.db, events::COLLECTION, &w.event, &doc)
        .await
        .unwrap();
    run_once(&w.app.state).await.unwrap();
    let alerts = expiring_alerts(&w.app).await;
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0]["target_id"], json!(pending));
}

#[tokio::test]
async fn thumbnails_go_when_every_event_of_the_person_is_closed() {
    let w = world().await;
    let far_shelter = w
        .app
        .create_shelter(&w.admin, "Far", (-23.55, -46.63))
        .await;
    let far_event = event_of(&w.app, &far_shelter).await;
    idle_for(&w.app, &w.event, 31).await;

    let all_closed = thumbnail_person(&w.app, &[&w.event]).await;
    let mixed = thumbnail_person(&w.app, &[&w.event, &far_event]).await;
    let still_open = thumbnail_person(&w.app, &[&far_event]).await;
    let no_event = thumbnail_person(&w.app, &[]).await;

    run_once(&w.app.state).await.unwrap();

    assert!(!has_thumbnail(&w.app, &all_closed).await);
    for kept in [&mixed, &still_open, &no_event] {
        assert!(has_thumbnail(&w.app, kept).await, "{kept}");
    }
    assert!(
        audit_actions(&w.app.state.db)
            .await
            .contains(&"person.thumbnail_purge".to_string())
    );

    // When the other event closes too, the mixed person loses it as well.
    idle_for(&w.app, &far_event, 31).await;
    run_once(&w.app.state).await.unwrap();
    assert!(!has_thumbnail(&w.app, &mixed).await);
    assert!(!has_thumbnail(&w.app, &still_open).await);
    assert!(has_thumbnail(&w.app, &no_event).await);
}

#[tokio::test]
async fn a_person_with_no_event_and_no_shelter_loses_the_thumbnail_after_the_purge_period() {
    let w = world().await;
    let aged = |days: i64, shelter: Option<&str>| {
        let mut doc = person(Level::NoBiometrics, shelter);
        doc.thumbnail_encrypted = Some(vec![1, 2, 3]);
        doc.updated_at = ago(days, 0);
        doc
    };
    let (old, recent, sheltered) = (new_id(), new_id(), new_id());
    put(&w.app, &old, &aged(31, None)).await;
    put(&w.app, &recent, &aged(29, None)).await;
    put(&w.app, &sheltered, &aged(31, Some(&w.shelter))).await;

    run_once(&w.app.state).await.unwrap();

    assert!(!has_thumbnail(&w.app, &old).await);
    assert!(has_thumbnail(&w.app, &recent).await);
    assert!(has_thumbnail(&w.app, &sheltered).await);
}

fn capture(expires_at: FirestoreInstant) -> CaptureDoc {
    CaptureDoc {
        process_id: "p".into(),
        purpose: Purpose::Login,
        device_id: "d".into(),
        status: CaptureStatus::Created,
        person_id: None,
        volunteer_id: None,
        shelter_id: None,
        expected_person_id: None,
        consumed: false,
        created_at: store::now(),
        expires_at,
    }
}

fn invite(shelter: &str, status: InviteStatus, expires_at: FirestoreInstant) -> InviteDoc {
    InviteDoc {
        shelter_id: shelter.into(),
        kind: InviteKind::Qr,
        role: ShelterRole::Volunteer,
        created_by: "x".into(),
        created_at: store::now(),
        expires_at,
        status,
        used_by: None,
        used_at: None,
    }
}

fn code(shelter: &str, used: bool, expires_at: FirestoreInstant) -> EndorsementCodeDoc {
    EndorsementCodeDoc {
        shelter_id: shelter.into(),
        created_by: "x".into(),
        lat: 0.0,
        lon: 0.0,
        accuracy_m: 1.0,
        expires_at,
        used,
    }
}

async fn exists(app: &TestApp, collection: &str, id: &str) -> bool {
    store::get::<Value>(&app.state.db, collection, id)
        .await
        .unwrap()
        .is_some()
}

#[tokio::test]
async fn expired_captures_invites_and_codes_are_deleted_and_the_rest_kept() {
    let w = world().await;
    let (past, future) = (ago(0, 5), ago(0, -600));
    let db = &w.app.state.db;
    for (id, doc) in [
        ("old-capture", capture(past)),
        ("new-capture", capture(future)),
    ] {
        store::replace(db, "captures", id, &doc).await.unwrap();
    }
    for (id, status, at) in [
        ("old-invite", InviteStatus::Active, past),
        ("new-invite", InviteStatus::Active, future),
        ("used-invite", InviteStatus::Used, past),
        ("canceled-invite", InviteStatus::Canceled, past),
    ] {
        let doc = invite(&w.shelter, status, at);
        store::replace(db, invites::COLLECTION, id, &doc)
            .await
            .unwrap();
    }
    for (id, used, at) in [
        ("old-code", false, past),
        ("new-code", false, future),
        ("used-code", true, past),
    ] {
        let doc = code(&w.shelter, used, at);
        store::replace(db, endorsements::COLLECTION, id, &doc)
            .await
            .unwrap();
    }

    run_once(&w.app.state).await.unwrap();

    for (collection, id, kept) in [
        ("captures", "old-capture", false),
        ("captures", "new-capture", true),
        (invites::COLLECTION, "old-invite", false),
        (invites::COLLECTION, "new-invite", true),
        (invites::COLLECTION, "used-invite", true),
        (invites::COLLECTION, "canceled-invite", true),
        (endorsements::COLLECTION, "old-code", false),
        (endorsements::COLLECTION, "new-code", true),
        (endorsements::COLLECTION, "used-code", true),
    ] {
        assert_eq!(
            exists(&w.app, collection, id).await,
            kept,
            "{collection}/{id}"
        );
    }
    // Running again is harmless.
    run_once(&w.app.state).await.unwrap();
    assert!(exists(&w.app, "captures", "new-capture").await);
}

#[tokio::test]
async fn more_than_one_batch_of_expired_documents_is_cleared() {
    let w = world().await;
    let past = ago(0, 5);
    let before = all_docs(&w.app.state.db, "captures").await.len();
    for n in 0..(BATCH_WRITES + 50) {
        store::replace(
            &w.app.state.db,
            "captures",
            &format!("c{n}"),
            &capture(past),
        )
        .await
        .unwrap();
    }
    store::replace(&w.app.state.db, "captures", "live", &capture(ago(0, -600)))
        .await
        .unwrap();
    run_once(&w.app.state).await.unwrap();
    assert_eq!(
        all_docs(&w.app.state.db, "captures").await.len(),
        before + 1
    );
}

fn failing(code: &'static str) -> StepFuture<'static> {
    Box::pin(async move { Err(ApiError::new(axum::http::StatusCode::BAD_GATEWAY, code)) })
}

#[tokio::test]
async fn a_failed_or_slow_step_does_not_stop_the_others_and_every_failure_is_kept() {
    let ran = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counter = ran.clone();
    let steps: Vec<(&'static str, StepFuture<'static>)> = vec![
        ("first", failing("first_failed")),
        (
            "slow",
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(())
            }),
        ),
        ("second", failing("second_failed")),
        (
            "last",
            Box::pin(async move {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }),
        ),
    ];
    let failures = run_steps(steps, Duration::from_millis(100)).await;
    let seen: Vec<_> = failures.iter().map(|f| (f.step, f.error.code)).collect();
    assert_eq!(
        seen,
        [
            ("first", "first_failed"),
            ("slow", "job_step_timeout"),
            ("second", "second_failed")
        ]
    );
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_loop_stops_when_asked_even_in_the_middle_of_a_pass() {
    let app = crate::test_support::TestApp::new().await;
    // The first tick is immediate, so a pass is under way when the stop arrives.
    let jobs = spawn(app.state.clone());
    let started = tokio::time::Instant::now();
    jobs.shutdown().await;
    assert!(
        started.elapsed() < SHUTDOWN_GRACE,
        "the loop ended after the stop"
    );
}

#[test]
fn the_budget_runs_out_and_stays_out() {
    let mut budget = Budget::new("test");
    assert!(!budget.spent());
    budget.spend(WORK_BUDGET - 1);
    assert!(!budget.spent());
    budget.spend(5);
    assert!(budget.spent());
}
