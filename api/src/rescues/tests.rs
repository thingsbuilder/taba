use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use super::domain::{text, valid_point};
use super::model::{COLLECTION, RescueDoc};
use super::persist::ROUTE;
use super::*;
use crate::audit::LegalBasis;
use crate::auth::Volunteer;
use crate::events::{self, EventDoc, EventStatus};
use crate::idempotency;
use crate::people::{self, Level, Origin, PersonDoc};
use crate::shelters::{self, ShelterDoc};
use crate::store;
use crate::test_support::{
    Actor, ORIGIN, TestApp, all_docs, audit_actions, new_id, north_of_origin,
};
use crate::thumbnail;
use crate::volunteers::GlobalRole;

const JPEG: [u8; 6] = [0xFF, 0xD8, 0xFF, 0xE0, 1, 2];

// ---- units ----

#[test]
fn text_is_trimmed_and_bounded() {
    assert_eq!(
        text(Some("  Ponte  ".into()), "f", 10).unwrap().as_deref(),
        Some("Ponte")
    );
    assert_eq!(text(Some("   ".into()), "f", 10).unwrap(), None);
    assert_eq!(text(None, "f", 10).unwrap(), None);
    assert!(text(Some("x".repeat(10)), "f", 10).is_ok());
    let error = text(Some("x".repeat(11)), "landing_point", 10).unwrap_err();
    assert_eq!(
        (error.status, error.code, error.detail["field"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            Some("landing_point")
        )
    );
}

#[test]
fn a_point_must_be_on_the_globe() {
    assert!(valid_point(-90.0, 180.0).is_ok());
    for (lat, lon) in [(90.1, 0.0), (-90.1, 0.0), (0.0, 180.1), (f64::NAN, 0.0)] {
        assert!(valid_point(lat, lon).is_err(), "{lat} {lon}");
    }
}

// ---- API ----

struct World {
    app: TestApp,
    admin: Actor,
    volunteer: Actor,
    shelter: String,
}

/// An unconfirmed shelter at the origin with its admin and one volunteer.
async fn world() -> World {
    let app = TestApp::new().await;
    let admin = app.actor("Admin").await;
    let shelter = app.create_shelter(&admin, "Shelter A", ORIGIN).await;
    let code = app.invite(&admin, &shelter, "link", "volunteer").await;
    let volunteer = app.join(&code, "Volunteer").await;
    World {
        app,
        admin,
        volunteer,
        shelter,
    }
}

async fn send(
    app: &TestApp,
    who: &Actor,
    method: &str,
    path: &str,
    key: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let auth = format!("Bearer {}", who.secret);
    let mut headers = vec![
        ("authorization", auth.as_str()),
        ("x-device-id", who.device.as_str()),
    ];
    if let Some(key) = key {
        headers.push(("idempotency-key", key));
    }
    let (status, _, json) = app.request(method, path, &headers, body).await;
    (status, json)
}

async fn call(
    app: &TestApp,
    who: &Actor,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    send(app, who, method, path, None, body).await
}

async fn post(app: &TestApp, who: &Actor, body: Value) -> (StatusCode, Value) {
    call(app, who, "POST", "/v1/rescues", Some(body)).await
}

fn rescue_at(at: (f64, f64), extra: Value) -> Value {
    let mut body = json!({
        "id": new_id(),
        "lat": at.0,
        "lon": at.1,
        "accuracy": 12.0,
        "captured_at": store::now().to_string(),
    });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

fn rescue(extra: Value) -> Value {
    rescue_at(ORIGIN, extra)
}

fn photo() -> Value {
    json!(STANDARD.encode(b"a face"))
}

fn photos_are(app: &TestApp, token: &str, n: usize) {
    app.unico.with(|s| {
        (0..n).for_each(|_| {
            s.post_replies.push_back((
                200,
                json!({ "id": "img", "state": "PROCESS_STATE_FINISHED", "data": { "humanToken": { "value": token } } }),
            ))
        })
    });
}

async fn rescue_doc(app: &TestApp, id: &str) -> RescueDoc {
    store::get(&app.state.db, COLLECTION, id)
        .await
        .unwrap()
        .unwrap()
}

async fn person(app: &TestApp, id: &str) -> PersonDoc {
    store::get(&app.state.db, people::COLLECTION, id)
        .await
        .unwrap()
        .unwrap()
}

fn token_id(app: &TestApp, token: &str) -> String {
    app.state.person_id_for_token(token)
}

async fn audited(app: &TestApp, action: &str) -> usize {
    audit_actions(&app.state.db)
        .await
        .iter()
        .filter(|a| *a == action)
        .count()
}

async fn check_in(w: &World, shelter: &str, extra: Value) -> Value {
    let mut body = json!({
        "id": new_id(),
        "legal_basis": "consent",
        "client_created_at": store::now().to_string(),
        "data": {},
    });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &format!("/v1/shelters/{shelter}/checkins"),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

async fn close(w: &World, shelter: &str) {
    let (status, _) = call(
        &w.app,
        &w.admin,
        "POST",
        &format!("/v1/shelters/{shelter}/close"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

fn viewer(id: &str, roles: Vec<GlobalRole>) -> Volunteer {
    let now = store::now();
    Volunteer {
        id: id.into(),
        doc: crate::volunteers::VolunteerDoc {
            name: "V".into(),
            phone: None,
            show_phone: false,
            language: "pt-BR".into(),
            global_roles: roles,
            role_grants: vec![],
            banned: false,
            banned_by: None,
            banned_at: None,
            flagged: false,
            flag_reasons: vec![],
            created_at: now,
            last_seen_at: now,
        },
        memberships: vec![],
        session_id: "s".into(),
    }
}

#[tokio::test]
async fn a_rescue_with_a_photo_creates_an_assisted_person_with_no_shelter() {
    let w = world().await;
    photos_are(&w.app, "tok-ana", 1);
    let id = new_id();
    let (status, body) = post(
        &w.app,
        &w.volunteer,
        rescue(json!({
            "id": id, "photo_base64": photo(), "landing_point": "Ponte Velha",
            "destination_shelter_id": w.shelter,
            "data": { "name": "Ana", "note": "ferida na perna" },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let person_id = token_id(&w.app, "tok-ana");
    assert_eq!(
        body,
        json!({ "rescue_id": id, "person_id": person_id, "level": "assisted" })
    );

    let doc = rescue_doc(&w.app, &id).await;
    assert_eq!(doc.person_id, person_id);
    assert_eq!(doc.volunteer_id, w.volunteer.id);
    assert_eq!(doc.device_id.as_deref(), Some(w.volunteer.device.as_str()));
    assert_eq!(
        (doc.lat, doc.lon, doc.accuracy_m),
        (ORIGIN.0, ORIGIN.1, 12.0)
    );
    assert_eq!(doc.landing_point.as_deref(), Some("Ponte Velha"));
    assert_eq!(
        doc.destination_shelter_id.as_deref(),
        Some(w.shelter.as_str())
    );
    assert_eq!(doc.note.as_deref(), Some("ferida na perna"));
    assert!(doc.received_at <= store::now());

    let person = person(&w.app, &person_id).await;
    assert_eq!(
        (person.level, person.origin, person.legal_basis),
        (Level::Assisted, Origin::Rescue, LegalBasis::VitalInterest)
    );
    assert_eq!(person.name.as_deref(), Some("Ana"));
    assert_eq!(person.rescue_ids, [id.clone()]);
    assert!(person.stays.is_empty() && person.current_shelter_id.is_none());

    let entries = all_docs(&w.app.state.db, "audit_log").await;
    let entry = entries
        .iter()
        .find(|e| e["action"] == "rescue.create")
        .expect("audited");
    assert_eq!(entry["volunteer_id"], json!(w.volunteer.id));
    assert_eq!(entry["target_id"], json!(id));
    assert_eq!(entry["legal_basis"], "vital_interest");
    // the audit entry holds ids only
    assert!(!entry.to_string().contains("Ana"));
}

#[tokio::test]
async fn the_same_face_in_a_second_rescue_adds_to_the_history_and_keeps_the_shelter() {
    let w = world().await;
    photos_are(&w.app, "tok-bia", 1);
    let first = check_in(&w, &w.shelter, json!({ "photo_base64": photo() })).await;
    let person_id = token_id(&w.app, "tok-bia");
    assert_eq!(first["person_id"], json!(person_id));
    let before = person(&w.app, &person_id).await;
    assert_eq!(before.origin, Origin::Checkin);

    let ids = [new_id(), new_id()];
    for id in &ids {
        photos_are(&w.app, "tok-bia", 1);
        let (status, body) = post(
            &w.app,
            &w.volunteer,
            rescue(json!({ "id": id, "photo_base64": photo() })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["person_id"], json!(person_id));
        assert_eq!(body["level"], "assisted");
    }
    let after = person(&w.app, &person_id).await;
    assert_eq!(after.rescue_ids, ids);
    assert_eq!(after.origin, Origin::Checkin);
    assert_eq!(after.stays, before.stays);
    assert_eq!(
        after.current_shelter_id.as_deref(),
        Some(w.shelter.as_str())
    );
    assert_eq!(all_docs(&w.app.state.db, "people").await.len(), 1);
}

#[tokio::test]
async fn a_rescue_without_a_photo_is_a_person_without_biometrics_under_the_client_id() {
    let w = world().await;
    let calls = w.app.unico.post_count();
    let id = new_id();
    let (status, body) = post(
        &w.app,
        &w.volunteer,
        rescue(json!({ "id": id, "data": { "wristband": "B-9" } })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body,
        json!({ "rescue_id": id, "person_id": id, "level": "no_biometrics" })
    );
    assert_eq!(w.app.unico.post_count(), calls);
    let person = person(&w.app, &id).await;
    assert_eq!(
        (person.level, person.wristband.as_deref()),
        (Level::NoBiometrics, Some("B-9"))
    );
}

#[tokio::test]
async fn a_photo_without_a_usable_face_registers_the_rescue_without_biometrics() {
    let w = world().await;
    for (status, reply) in [
        (400, json!({ "code": 20532, "message": "No face detected" })),
        (400, json!({ "message": "SOMETHING_ELSE_ABOUT_THE_IMAGE" })),
        (
            200,
            json!({ "id": "img", "state": "PROCESS_STATE_FINISHED", "data": { "humanToken": { "value": "  " } } }),
        ),
    ] {
        w.app
            .unico
            .with(|s| s.post_replies.push_back((status, reply)));
        let id = new_id();
        let (code, body) = post(
            &w.app,
            &w.volunteer,
            rescue(json!({ "id": id, "photo_base64": photo() })),
        )
        .await;
        assert_eq!(code, StatusCode::CREATED, "{body}");
        assert_eq!(
            (body["person_id"].as_str(), body["level"].as_str()),
            (Some(id.as_str()), Some("no_biometrics"))
        );
    }
}

#[tokio::test]
async fn an_unreadable_photo_registers_without_biometrics_and_a_big_one_is_refused_before_unico() {
    let w = world().await;
    w.app.unico.with(|s| {
        s.post_replies
            .push_back((400, json!({ "message": "INVALID_IMAGE" })))
    });
    let id = new_id();
    let (status, body) = post(
        &w.app,
        &w.volunteer,
        rescue(json!({ "id": id, "photo_base64": photo() })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        (body["person_id"].as_str(), body["level"].as_str()),
        (Some(id.as_str()), Some("no_biometrics"))
    );

    let asked = w.app.unico.post_count();
    let big = STANDARD.encode(vec![0u8; 800 * 1024 + 1]);
    let (status, body) = post(&w.app, &w.volunteer, rescue(json!({ "photo_base64": big }))).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, Some("image_too_large"))
    );
    assert_eq!(w.app.unico.post_count(), asked);
    assert_eq!(all_docs(&w.app.state.db, COLLECTION).await.len(), 1);
}

#[tokio::test]
async fn a_second_rescue_fills_the_empty_name_and_wristband_but_never_overwrites() {
    let w = world().await;
    photos_are(&w.app, "tok-twice", 2);
    let (status, _) = post(
        &w.app,
        &w.volunteer,
        rescue(json!({ "photo_base64": photo(), "data": { "name": "Ana" } })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = post(
        &w.app,
        &w.volunteer,
        rescue(json!({ "photo_base64": photo(), "data": { "name": "Zed", "wristband": "W1" } })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let known = person(&w.app, &token_id(&w.app, "tok-twice")).await;
    assert_eq!(
        (known.name.as_deref(), known.wristband.as_deref()),
        (Some("Ana"), Some("W1"))
    );
}

#[tokio::test]
async fn nearby_leaves_out_the_shelters_of_a_closed_event() {
    let w = world().await;
    let path = format!("/v1/shelters/nearby?lat={}&lon={}", ORIGIN.0, ORIGIN.1);
    let (status, body) = call(&w.app, &w.volunteer, "GET", &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["shelters"].as_array().unwrap().len(), 1);
    let shelter: ShelterDoc = store::get(&w.app.state.db, shelters::COLLECTION, &w.shelter)
        .await
        .unwrap()
        .unwrap();
    let event_id = shelter.event_id.unwrap();
    let mut event: EventDoc = store::get(&w.app.state.db, events::COLLECTION, &event_id)
        .await
        .unwrap()
        .unwrap();
    event.status = EventStatus::Closed;
    store::replace(&w.app.state.db, events::COLLECTION, &event_id, &event)
        .await
        .unwrap();
    let (status, body) = call(&w.app, &w.volunteer, "GET", &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["shelters"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn the_degraded_mode_registers_the_rescue_without_calling_unico() {
    let w = world().await;
    crate::degraded::enter_unavailable(&w.app.state.db)
        .await
        .unwrap();
    let calls = w.app.unico.post_count();
    let id = new_id();
    let (status, body) = post(
        &w.app,
        &w.volunteer,
        rescue(json!({ "id": id, "photo_base64": photo() })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        (body["person_id"].as_str(), body["level"].as_str()),
        (Some(id.as_str()), Some("no_biometrics"))
    );
    assert_eq!(w.app.unico.post_count(), calls);
}

#[tokio::test]
async fn an_invalid_image_and_a_failing_unico_are_not_turned_into_a_rescue() {
    let w = world().await;
    let (status, body) = post(
        &w.app,
        &w.volunteer,
        rescue(json!({ "photo_base64": "not base64!" })),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_image"))
    );
    for _ in 0..2 {
        w.app
            .unico
            .with(|s| s.post_replies.push_back((500, json!({}))));
    }
    let (status, body) = post(
        &w.app,
        &w.volunteer,
        rescue(json!({ "photo_base64": photo() })),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::BAD_GATEWAY, Some("unico_unavailable"))
    );
    assert!(all_docs(&w.app.state.db, COLLECTION).await.is_empty());
    assert!(all_docs(&w.app.state.db, "people").await.is_empty());
}

#[tokio::test]
async fn the_event_is_the_one_of_the_nearest_open_shelter_within_the_radius() {
    let w = world().await;
    let far = w
        .app
        .create_shelter(&w.admin, "Shelter far", north_of_origin(80_000.0))
        .await;
    let event_of = |shelter: String| {
        let app = w.app.clone();
        async move {
            store::get::<ShelterDoc>(&app.state.db, shelters::COLLECTION, &shelter)
                .await
                .unwrap()
                .unwrap()
                .event_id
                .unwrap()
        }
    };
    let (event_a, event_far) = (event_of(w.shelter.clone()).await, event_of(far).await);
    assert_ne!(event_a, event_far);

    for (at, expected) in [
        (ORIGIN, Some(&event_a)),
        (north_of_origin(30_000.0), Some(&event_a)),
        (north_of_origin(80_500.0), Some(&event_far)),
        // 60 km from A, 20 km from the far one
        (north_of_origin(60_000.0), Some(&event_far)),
        // within reach of both, nearer to A
        (north_of_origin(39_000.0), Some(&event_a)),
        (north_of_origin(300_000.0), None),
    ] {
        let id = new_id();
        let (status, body) = post(&w.app, &w.volunteer, rescue_at(at, json!({ "id": id }))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let expected = expected.cloned();
        assert_eq!(rescue_doc(&w.app, &id).await.event_id, expected, "{at:?}");
        // the person carries the event too
        assert_eq!(
            person(&w.app, &id).await.event_ids,
            expected.into_iter().collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn a_closed_shelter_or_an_event_that_is_over_gives_no_event() {
    let w = world().await;
    close(&w, &w.shelter).await;
    let id = new_id();
    let (status, body) = post(&w.app, &w.volunteer, rescue(json!({ "id": id }))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(rescue_doc(&w.app, &id).await.event_id, None);

    let other = world().await;
    let sheltered: ShelterDoc =
        store::get(&other.app.state.db, shelters::COLLECTION, &other.shelter)
            .await
            .unwrap()
            .unwrap();
    let event_id = sheltered.event_id.unwrap();
    let mut event: EventDoc = store::get(&other.app.state.db, events::COLLECTION, &event_id)
        .await
        .unwrap()
        .unwrap();
    event.status = EventStatus::Closed;
    store::replace(&other.app.state.db, events::COLLECTION, &event_id, &event)
        .await
        .unwrap();
    let id = new_id();
    let (status, body) = post(&other.app, &other.volunteer, rescue(json!({ "id": id }))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(rescue_doc(&other.app, &id).await.event_id, None);
}

#[tokio::test]
async fn a_rescue_records_activity_on_the_event() {
    let w = world().await;
    let shelter: ShelterDoc = store::get(&w.app.state.db, shelters::COLLECTION, &w.shelter)
        .await
        .unwrap()
        .unwrap();
    let event_id = shelter.event_id.unwrap();
    let mut event: EventDoc = store::get(&w.app.state.db, events::COLLECTION, &event_id)
        .await
        .unwrap()
        .unwrap();
    let old = store::now() - firestore::jiff::SignedDuration::from_hours(48);
    event.last_activity_at = old;
    store::replace(&w.app.state.db, events::COLLECTION, &event_id, &event)
        .await
        .unwrap();
    w.app.state.event_activity.lock().unwrap().clear();
    let (status, body) = post(&w.app, &w.volunteer, rescue(json!({}))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let event: EventDoc = store::get(&w.app.state.db, events::COLLECTION, &event_id)
        .await
        .unwrap()
        .unwrap();
    assert!(event.last_activity_at > old);
}

#[tokio::test]
async fn the_thumbnail_is_kept_encrypted_only_while_there_is_no_name_or_wristband() {
    let w = world().await;
    let upload = STANDARD.encode(JPEG);
    let anonymous = new_id();
    let (status, body) = post(
        &w.app,
        &w.volunteer,
        rescue(json!({ "id": anonymous, "thumbnail_base64": upload })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let sealed = person(&w.app, &anonymous)
        .await
        .thumbnail_encrypted
        .unwrap();
    assert!(!sealed.windows(JPEG.len()).any(|w| w == JPEG));
    assert_eq!(
        thumbnail::decrypt(w.app.state.thumbnail_key(), &sealed).unwrap(),
        JPEG
    );

    // with a name or a wristband the thumbnail is not stored
    for data in [json!({ "name": "Ana" }), json!({ "wristband": "B-1" })] {
        let id = new_id();
        let (status, body) = post(
            &w.app,
            &w.volunteer,
            rescue(json!({ "id": id, "data": data, "thumbnail_base64": upload })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(person(&w.app, &id).await.thumbnail_encrypted.is_none());
    }

    // a known person with a name gives up the thumbnail of a later rescue
    photos_are(&w.app, "tok-cris", 2);
    let (status, _) = post(
        &w.app,
        &w.volunteer,
        rescue(json!({ "photo_base64": photo(), "data": { "name": "Cris" } })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = post(
        &w.app,
        &w.volunteer,
        rescue(json!({ "photo_base64": photo(), "thumbnail_base64": upload })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(
        person(&w.app, &token_id(&w.app, "tok-cris"))
            .await
            .thumbnail_encrypted
            .is_none()
    );
}

#[tokio::test]
async fn only_the_rescuer_and_the_civil_defense_see_the_thumbnail_of_a_person_without_a_shelter() {
    let w = world().await;
    let id = new_id();
    let (status, _) = post(
        &w.app,
        &w.volunteer,
        rescue(json!({ "id": id, "thumbnail_base64": STANDARD.encode(JPEG) })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let rescued = person(&w.app, &id).await;
    let db = &w.app.state.db;
    let author = viewer(&w.volunteer.id, vec![]);
    let other = viewer(&w.admin.id, vec![]);
    let dc = viewer("dc", vec![GlobalRole::CivilDefense]);
    let mediator = viewer("m", vec![GlobalRole::Mediator]);
    assert!(can_see_thumbnail(db, &author, &rescued).await.unwrap());
    assert!(can_see_thumbnail(db, &dc, &rescued).await.unwrap());
    assert!(!can_see_thumbnail(db, &other, &rescued).await.unwrap());
    assert!(!can_see_thumbnail(db, &mediator, &rescued).await.unwrap());
    assert!(registered_rescue_of(db, &author, &rescued).await.unwrap());
    assert!(!registered_rescue_of(db, &other, &rescued).await.unwrap());

    // once the person has a shelter the shelter rules apply
    check_in(&w, &w.shelter, json!({ "id": id, "no_biometrics": true })).await;
    let sheltered = person(&w.app, &id).await;
    assert!(!can_see_thumbnail(db, &author, &sheltered).await.unwrap());
    assert!(!can_see_thumbnail(db, &dc, &sheltered).await.unwrap());
    // and a person who was never rescued has no such viewer
    let never = PersonDoc {
        rescue_ids: vec![],
        stays: vec![],
        ..sheltered
    };
    assert!(!can_see_thumbnail(db, &dc, &never).await.unwrap());
}

#[tokio::test]
async fn sending_the_rescue_again_answers_the_same_and_pays_for_the_photo_once() {
    let w = world().await;
    photos_are(&w.app, "tok-ana", 1);
    let calls = w.app.unico.post_count();
    let id = new_id();
    let body = rescue(json!({ "id": id, "photo_base64": photo() }));
    let (status, first) = send(
        &w.app,
        &w.volunteer,
        "POST",
        "/v1/rescues",
        Some("key-1"),
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    // the same key, another key and no key at all
    for key in [Some("key-1"), Some("key-2"), None] {
        let (status, again) = send(
            &w.app,
            &w.volunteer,
            "POST",
            "/v1/rescues",
            key,
            Some(body.clone()),
        )
        .await;
        assert_eq!((status, &again), (StatusCode::CREATED, &first), "{key:?}");
    }
    assert_eq!(w.app.unico.post_count(), calls + 1);
    assert_eq!(all_docs(&w.app.state.db, COLLECTION).await.len(), 1);
    assert_eq!(
        person(&w.app, &token_id(&w.app, "tok-ana"))
            .await
            .rescue_ids,
        [id]
    );
    assert_eq!(audited(&w.app, "rescue.create").await, 1);
}

#[tokio::test]
async fn an_answer_of_unico_kept_under_the_key_is_reused_by_the_retry() {
    let w = world().await;
    let calls = w.app.unico.post_count();
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", "key-x".parse().unwrap());
    let request = rescue(json!({ "photo_base64": photo() }));
    // a first attempt paid and then died before its transaction; the answer is bound to the
    // digest of the request, which is what the retry sends again
    let digest =
        idempotency::digest_of(&[b"POST", b"/v1/rescues", b"", request.to_string().as_bytes()]);
    let kept = idempotency::photo_person_id_of(
        &w.app.state,
        &headers,
        &w.volunteer.id,
        ROUTE,
        Some(&digest),
        async { Ok(token_id(&w.app, "tok-ana")) },
    )
    .await
    .unwrap();
    let (status, body) = send(
        &w.app,
        &w.volunteer,
        "POST",
        "/v1/rescues",
        Some("key-x"),
        Some(request),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["person_id"], json!(kept));
    assert_eq!(w.app.unico.post_count(), calls);
}

#[tokio::test]
async fn a_rescue_id_used_by_someone_else_is_refused() {
    let w = world().await;
    let id = new_id();
    let body = rescue(json!({ "id": id }));
    let (status, _) = post(&w.app, &w.volunteer, body.clone()).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, answer) = post(&w.app, &w.admin, body).await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );
    assert_eq!(rescue_doc(&w.app, &id).await.volunteer_id, w.volunteer.id);
}

#[tokio::test]
async fn copies_of_the_same_rescue_sent_at_once_make_one_rescue_and_one_person() {
    let w = world().await;
    let id = new_id();
    photos_are(&w.app, "tok-ana", 6);
    let body = rescue(json!({ "id": id, "photo_base64": photo() }));
    let mut handles = Vec::new();
    for _ in 0..6 {
        let (app, who, body) = (w.app.clone(), w.volunteer.clone(), body.clone());
        handles.push(tokio::spawn(async move { post(&app, &who, body).await }));
    }
    let mut answers = Vec::new();
    for handle in handles {
        let (status, answer) = handle.await.unwrap();
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        answers.push(answer);
    }
    assert!(answers.iter().all(|a| a == &answers[0]), "{answers:?}");
    assert_eq!(all_docs(&w.app.state.db, COLLECTION).await.len(), 1);
    assert_eq!(all_docs(&w.app.state.db, "people").await.len(), 1);
    assert_eq!(
        person(&w.app, &token_id(&w.app, "tok-ana"))
            .await
            .rescue_ids,
        [id]
    );
    assert_eq!(audited(&w.app, "rescue.create").await, 1);
}

#[tokio::test]
async fn rescues_of_the_same_face_sent_at_once_all_reach_the_history() {
    let w = world().await;
    photos_are(&w.app, "tok-ana", 6);
    let ids: Vec<String> = (0..6).map(|_| new_id()).collect();
    let mut handles = Vec::new();
    for id in &ids {
        let (app, who, id) = (w.app.clone(), w.volunteer.clone(), id.clone());
        handles.push(tokio::spawn(async move {
            post(
                &app,
                &who,
                rescue(json!({ "id": id, "photo_base64": photo() })),
            )
            .await
        }));
    }
    for handle in handles {
        let (status, answer) = handle.await.unwrap();
        assert_eq!(status, StatusCode::CREATED, "{answer}");
    }
    assert_eq!(all_docs(&w.app.state.db, "people").await.len(), 1);
    let mut history = person(&w.app, &token_id(&w.app, "tok-ana"))
        .await
        .rescue_ids;
    let mut expected = ids.clone();
    history.sort();
    expected.sort();
    assert_eq!(history, expected);
    assert_eq!(all_docs(&w.app.state.db, COLLECTION).await.len(), 6);
}

#[tokio::test]
async fn who_may_register_a_rescue() {
    let w = world().await;
    let validator = w.app.actor("Validator").await;
    w.app.grant(&validator, GlobalRole::Validator).await;
    let mediator = w.app.actor("Mediator").await;
    w.app.grant(&mediator, GlobalRole::Mediator).await;
    let protection = w.app.actor("Protection").await;
    w.app.grant(&protection, GlobalRole::Protection).await;
    let stranger = w.app.actor("Stranger").await;
    let dc = w.app.actor("DC").await;
    w.app.grant(&dc, GlobalRole::CivilDefense).await;

    for who in [&validator, &mediator, &protection, &stranger] {
        let (status, answer) = post(&w.app, who, rescue(json!({}))).await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::FORBIDDEN, Some("forbidden")),
            "{}",
            who.device
        );
    }
    assert!(all_docs(&w.app.state.db, COLLECTION).await.is_empty());
    for who in [&w.volunteer, &w.admin, &dc] {
        let (status, answer) = post(&w.app, who, rescue(json!({}))).await;
        assert_eq!(status, StatusCode::CREATED, "{} {answer}", who.device);
    }
    // a mediator who is also a volunteer of a shelter may
    let code = w
        .app
        .invite(&w.admin, &w.shelter, "link", "volunteer")
        .await;
    w.app.join_again(&code, &mediator).await;
    let (status, _) = post(&w.app, &mediator, rescue(json!({}))).await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _, answer) = w
        .app
        .request("POST", "/v1/rescues", &[], Some(rescue(json!({}))))
        .await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("unauthorized"))
    );
}

#[tokio::test]
async fn a_malformed_rescue_is_refused_and_nothing_is_written() {
    let w = world().await;
    let calls = w.app.unico.post_count();
    let long = "x".repeat(101);
    for bad in [
        json!({ "id": "not-a-uuid" }),
        json!({ "id": 7 }),
        json!({ "lat": 91.0 }),
        json!({ "lon": -180.5 }),
        json!({ "accuracy": -1.0 }),
        json!({ "captured_at": "yesterday" }),
        json!({ "lat": "up" }),
        json!({ "data": { "name": long } }),
        json!({ "data": { "wristband": long } }),
        json!({ "data": { "note": "x".repeat(501) } }),
        json!({ "data": { "religion": "x" } }),
        json!({ "landing_point": long }),
    ] {
        let (status, answer) = post(&w.app, &w.volunteer, rescue(bad.clone())).await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_request")),
            "{bad}"
        );
    }
    let mut missing = rescue(json!({}));
    missing.as_object_mut().unwrap().remove("lat");
    let (status, answer) = post(&w.app, &w.volunteer, missing).await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_request"))
    );
    let (status, answer) = post(
        &w.app,
        &w.volunteer,
        rescue(json!({ "thumbnail_base64": STANDARD.encode(b"not a jpeg") })),
    )
    .await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_thumbnail"))
    );
    let (status, answer) = post(
        &w.app,
        &w.volunteer,
        rescue(json!({ "destination_shelter_id": "nowhere" })),
    )
    .await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("shelter_not_found"))
    );
    assert!(all_docs(&w.app.state.db, COLLECTION).await.is_empty());
    assert!(all_docs(&w.app.state.db, "people").await.is_empty());
    assert_eq!(w.app.unico.post_count(), calls);
}

// ---- PATCH ----

#[tokio::test]
async fn the_author_changes_the_landing_point_and_the_destination() {
    let w = world().await;
    let id = new_id();
    let second = w
        .app
        .create_shelter(&w.admin, "Shelter B", north_of_origin(5_000.0))
        .await;
    post(
        &w.app,
        &w.volunteer,
        rescue(json!({ "id": id, "landing_point": "Ponte" })),
    )
    .await;
    let path = format!("/v1/rescues/{id}");
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "PATCH",
        &path,
        Some(json!({ "landing_point": "  Cais  ", "destination_shelter_id": second })),
    )
    .await;
    assert_eq!((status, body), (StatusCode::OK, json!({})));
    let doc = rescue_doc(&w.app, &id).await;
    assert_eq!(doc.landing_point.as_deref(), Some("Cais"));
    assert_eq!(doc.destination_shelter_id.as_deref(), Some(second.as_str()));
    // what is not sent stays, and a blank value changes nothing
    let (status, _) = call(
        &w.app,
        &w.volunteer,
        "PATCH",
        &path,
        Some(json!({ "landing_point": "   " })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let doc = rescue_doc(&w.app, &id).await;
    assert_eq!(doc.landing_point.as_deref(), Some("Cais"));
    assert_eq!(doc.destination_shelter_id.as_deref(), Some(second.as_str()));
    assert_eq!(audited(&w.app, "rescue.update").await, 2);
}

#[tokio::test]
async fn only_the_author_changes_a_rescue_and_the_values_are_checked() {
    let w = world().await;
    let id = new_id();
    post(
        &w.app,
        &w.volunteer,
        rescue(json!({ "id": id, "landing_point": "Ponte" })),
    )
    .await;
    let dc = w.app.actor("DC").await;
    w.app.grant(&dc, GlobalRole::CivilDefense).await;
    let path = format!("/v1/rescues/{id}");
    let patch = json!({ "landing_point": "Outro" });
    for who in [&w.admin, &dc] {
        let (status, body) = call(&w.app, who, "PATCH", &path, Some(patch.clone())).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::FORBIDDEN, Some("forbidden"))
        );
    }
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "PATCH",
        "/v1/rescues/nope",
        Some(patch),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("rescue_not_found"))
    );
    for (bad, error, expected) in [
        (
            json!({ "destination_shelter_id": "nowhere" }),
            "shelter_not_found",
            StatusCode::NOT_FOUND,
        ),
        (
            json!({ "landing_point": "x".repeat(101) }),
            "invalid_request",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({ "note": "no" }),
            "invalid_request",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let (status, body) = call(&w.app, &w.volunteer, "PATCH", &path, Some(bad.clone())).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (expected, Some(error)),
            "{bad}"
        );
    }
    assert_eq!(
        rescue_doc(&w.app, &id).await.landing_point.as_deref(),
        Some("Ponte")
    );
    let (status, _, _) = w
        .app
        .request("PATCH", &path, &[], Some(json!({ "landing_point": "x" })))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ---- nearby ----

#[tokio::test]
async fn nearby_lists_open_shelters_in_the_radius_with_vacancies_nearest_first() {
    let w = world().await;
    let confirmed = w
        .app
        .confirmed_shelter(&w.admin, "Confirmed", north_of_origin(2_000.0))
        .await;
    let (status, _) = call(
        &w.app,
        &w.admin,
        "PATCH",
        &format!("/v1/shelters/{confirmed}"),
        Some(json!({ "capacity": 10 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let guest = w.app.actor("Guest").await;
    let code = w
        .app
        .invite(&w.admin, &confirmed, "link", "volunteer")
        .await;
    w.app.join_again(&code, &w.volunteer).await;
    for _ in 0..3 {
        check_in(&w, &confirmed, json!({ "no_biometrics": true })).await;
    }
    let closed = w
        .app
        .create_shelter(&w.admin, "Closed", north_of_origin(1_000.0))
        .await;
    close(&w, &closed).await;
    w.app
        .create_shelter(&w.admin, "Out of reach", north_of_origin(60_000.0))
        .await;

    // anyone with a session may ask, with or without a shelter
    let (lat, lon) = north_of_origin(500.0);
    let (status, body) = call(
        &w.app,
        &guest,
        "GET",
        &format!("/v1/shelters/nearby?lat={lat}&lon={lon}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let list = body["shelters"].as_array().unwrap();
    let names: Vec<&str> = list.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Shelter A", "Confirmed"]);
    let (a, b) = (&list[0], &list[1]);
    assert_eq!(a["id"], json!(w.shelter));
    assert!(
        (a["distance_m"].as_f64().unwrap() - 500.0).abs() < 2.0,
        "{a}"
    );
    assert_eq!(
        (
            &a["capacity"],
            &a["occupancy"],
            &a["vacancies"],
            &a["confirmation"]
        ),
        (&Value::Null, &json!(0), &Value::Null, &json!("unconfirmed"))
    );
    assert!(
        (b["distance_m"].as_f64().unwrap() - 1_500.0).abs() < 2.0,
        "{b}"
    );
    assert_eq!(
        (
            &b["id"],
            &b["capacity"],
            &b["occupancy"],
            &b["vacancies"],
            &b["confirmation"]
        ),
        (
            &json!(confirmed),
            &json!(10),
            &json!(3),
            &json!(7),
            &json!("confirmed")
        )
    );
}

#[tokio::test]
async fn the_vacancies_never_go_below_zero_and_an_empty_area_is_an_empty_list() {
    let w = world().await;
    let (status, _) = call(
        &w.app,
        &w.admin,
        "PATCH",
        &format!("/v1/shelters/{}", w.shelter),
        Some(json!({ "capacity": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for _ in 0..2 {
        check_in(&w, &w.shelter, json!({ "no_biometrics": true })).await;
    }
    let (lat, lon) = ORIGIN;
    let (_, body) = call(
        &w.app,
        &w.volunteer,
        "GET",
        &format!("/v1/shelters/nearby?lat={lat}&lon={lon}"),
        None,
    )
    .await;
    assert_eq!(body["shelters"][0]["vacancies"], 0);
    assert_eq!(body["shelters"][0]["occupancy"], 2);
    let (lat, lon) = north_of_origin(500_000.0);
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "GET",
        &format!("/v1/shelters/nearby?lat={lat}&lon={lon}"),
        None,
    )
    .await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "shelters": [] })));
}

#[tokio::test]
async fn nearby_needs_a_session_and_a_valid_point() {
    let w = world().await;
    let (status, _, body) = w
        .app
        .request("GET", "/v1/shelters/nearby?lat=0&lon=0", &[], None)
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("unauthorized"))
    );
    for query in [
        "",
        "?lat=1",
        "?lat=a&lon=1",
        "?lat=91&lon=0",
        "?lat=0&lon=181",
    ] {
        let (status, body) = call(
            &w.app,
            &w.volunteer,
            "GET",
            &format!("/v1/shelters/nearby{query}"),
            None,
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_request")),
            "{query}"
        );
    }
}

// ---- late biometrics (spec 4.12) ----

#[tokio::test]
async fn late_biometrics_points_the_rescue_and_the_history_at_the_new_person() {
    let w = world().await;
    let id = new_id();
    let (status, _) = post(
        &w.app,
        &w.volunteer,
        rescue(json!({ "id": id, "data": { "name": "Ana" } })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    // later the person is checked in at the shelter, still without biometrics
    check_in(&w, &w.shelter, json!({ "id": id, "no_biometrics": true })).await;
    photos_are(&w.app, "tok-ana", 1);
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "PUT",
        &format!("/v1/people/{id}/biometrics"),
        Some(json!({ "photo_base64": photo() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let new = token_id(&w.app, "tok-ana");
    assert_eq!(body["person_id"], json!(new));

    assert_eq!(rescue_doc(&w.app, &id).await.person_id, new);
    let moved = person(&w.app, &new).await;
    assert_eq!(moved.rescue_ids, [id.clone()]);
    assert_eq!(moved.former_ids, [id.clone()]);
    assert_eq!(moved.origin, Origin::Rescue);
    assert!(
        store::get::<PersonDoc>(&w.app.state.db, people::COLLECTION, &id)
            .await
            .unwrap()
            .is_none()
    );
    // a rescue sent again after the move answers with the person it points at now
    let (status, again) = post(&w.app, &w.volunteer, rescue(json!({ "id": id }))).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        (again["person_id"].as_str(), again["level"].as_str()),
        (Some(new.as_str()), Some("assisted"))
    );
}
