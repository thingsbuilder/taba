use axum::http::StatusCode;
use serde_json::{Value, json};

use firestore::FirestoreInstant;

use super::domain::{Neighbour, NewShelter, validate};
use super::persist::create_in;
use super::*;
use crate::error::ApiError;
use crate::events::{self, EventDoc};
use crate::geo;
use crate::store;
use crate::test_support::{
    ORIGIN, TestApp, all_docs, audit_actions, new_id, north_of_origin, shelter_body,
};
use crate::volunteers::GlobalRole;

async fn post(app: &TestApp, who: &crate::test_support::Actor, body: Value) -> (StatusCode, Value) {
    app.send("POST", "/v1/shelters", &who.secret, Some(body))
        .await
}

fn error_of(body: &Value) -> Option<&str> {
    body["error"].as_str()
}

async fn shelters_in_db(app: &TestApp) -> usize {
    all_docs(&app.state.db, COLLECTION).await.len()
}

// ---- validation ----

fn request(edit: impl FnOnce(&mut Value)) -> Result<NewShelter, ApiError> {
    let mut body = shelter_body(&new_id(), "Ginásio", ORIGIN);
    edit(&mut body);
    validate(serde_json::from_value(body).unwrap())
}

fn field_of(error: &ApiError) -> Option<&str> {
    error.detail["field"].as_str()
}

#[test]
fn a_valid_request_is_trimmed_and_defaults_the_address_to_pending() {
    let shelter = request(|b| b["name"] = json!("  Ginásio  ")).unwrap();
    assert_eq!(shelter.name, "Ginásio");
    assert!(shelter.address.pending && shelter.capacity.is_none());
    let with_address = request(|b| {
        b["address"] = json!({ "text": " Rua A, 1 ", "city": "Canoas", "state": "RS" });
        b["capacity"] = json!(200);
    })
    .unwrap();
    assert_eq!(with_address.address.city.as_deref(), Some("Canoas"));
    assert!(!with_address.address.pending);
    assert_eq!(with_address.capacity, Some(200));
    // An address with nothing in it is still pending.
    assert!(
        request(|b| b["address"] = json!({ "text": "  " }))
            .unwrap()
            .address
            .pending
    );
}

#[test]
fn invalid_requests_name_the_field() {
    type Edit = Box<dyn Fn(&mut Value)>;
    let cases: Vec<(&str, Edit)> = vec![
        ("id", Box::new(|b| b["id"] = json!("not-a-uuid"))),
        ("name", Box::new(|b| b["name"] = json!("   "))),
        ("name", Box::new(|b| b["name"] = json!("x".repeat(101)))),
        ("position", Box::new(|b| b["lat"] = json!(91.0))),
        ("position", Box::new(|b| b["lon"] = json!(-181.0))),
        ("accuracy", Box::new(|b| b["accuracy"] = json!(-1.0))),
        ("capacity", Box::new(|b| b["capacity"] = json!(0))),
        ("capacity", Box::new(|b| b["capacity"] = json!(100_001))),
        (
            "client_created_at",
            Box::new(|b| b["client_created_at"] = json!("yesterday")),
        ),
        (
            "other_site_reason",
            Box::new(|b| b["other_site_reason"] = json!("  ")),
        ),
        (
            "address",
            Box::new(|b| b["address"] = json!({ "city": "x".repeat(101) })),
        ),
    ];
    for (field, edit) in cases {
        let error = request(edit).unwrap_err();
        assert_eq!(
            (error.status, error.code, field_of(&error)),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_shelter",
                Some(field)
            )
        );
    }
}

#[test]
fn accuracy_over_100_metres_is_refused_and_100_is_accepted() {
    assert!(request(|b| b["accuracy"] = json!(100.0)).is_ok());
    let error = request(|b| b["accuracy"] = json!(101.0)).unwrap_err();
    assert_eq!(
        (error.status, error.code),
        (StatusCode::UNPROCESSABLE_ENTITY, "insufficient_accuracy")
    );
    assert_eq!(error.detail["max"], 100.0);
}

// ---- creation ----

#[tokio::test]
async fn creating_a_shelter_makes_the_creator_its_admin_with_an_event_and_an_audit_entry() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let id = new_id();
    let mut body = shelter_body(&id, "Ginásio", ORIGIN);
    body["capacity"] = json!(150);
    let (status, created) = post(&app, &ana, body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let shelter = &created["shelter"];
    assert_eq!(
        (
            &shelter["id"],
            &shelter["name"],
            &shelter["status"],
            &shelter["confirmation"],
            &shelter["can_endorse"],
            &shelter["flagged"],
            &shelter["role"],
            &shelter["capacity"],
            &shelter["vacancies"]
        ),
        (
            &json!(id),
            &json!("Ginásio"),
            &json!("open"),
            &json!("unconfirmed"),
            &json!(false),
            &json!(false),
            &json!("admin"),
            &json!(150),
            &json!(150)
        )
    );
    assert_eq!(shelter["address"], json!({ "pending": true }));
    assert_eq!(created["event"]["id"], shelter["event_id"]);
    assert!(
        created["event"]["name"]
            .as_str()
            .unwrap()
            .starts_with("Evento – ")
    );

    // Stored as the spec lists it: nine-character geohash, creator, membership as admin.
    let stored: ShelterDoc = store::get(&app.state.db, COLLECTION, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.geohash, geo::encode(ORIGIN.0, ORIGIN.1, 9));
    assert_eq!(
        (stored.created_by.as_str(), stored.accuracy_m),
        (ana.id.as_str(), 10.0)
    );
    let (_, me) = app.get_authed("/v1/me", &ana.secret).await;
    assert_eq!(
        me["memberships"],
        json!([{ "shelter_id": id, "name": "Ginásio", "role": "admin", "confirmation": "unconfirmed" }])
    );
    assert!(
        audit_actions(&app.state.db)
            .await
            .contains(&"shelter.create".to_string())
    );
}

#[tokio::test]
async fn creating_needs_a_session() {
    let app = TestApp::new().await;
    let (status, _, _) = app
        .request(
            "POST",
            "/v1/shelters",
            &[],
            Some(shelter_body(&new_id(), "X", ORIGIN)),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn accuracy_101_is_refused_with_nothing_written() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let mut body = shelter_body(&new_id(), "Ginásio", ORIGIN);
    body["accuracy"] = json!(101.0);
    let (status, answer) = post(&app, &ana, body).await;
    assert_eq!(
        (status, error_of(&answer)),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("insufficient_accuracy")
        )
    );
    assert_eq!(shelters_in_db(&app).await, 0);
    let mut ok = shelter_body(&new_id(), "Ginásio", ORIGIN);
    ok["accuracy"] = json!(100.0);
    assert_eq!(post(&app, &ana, ok).await.0, StatusCode::CREATED);
}

#[tokio::test]
async fn a_shelter_80_metres_away_is_refused_naming_the_creator_but_not_the_phone() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana Souza").await;
    let first = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
    let bia = app.actor("Bia").await;
    let (status, answer) = post(
        &app,
        &bia,
        shelter_body(&new_id(), "Escola", north_of_origin(80.0)),
    )
    .await;
    assert_eq!(
        (status, error_of(&answer)),
        (StatusCode::CONFLICT, Some("shelter_nearby"))
    );
    let detail = &answer["detail"];
    assert_eq!(
        (
            &detail["shelter_id"],
            &detail["name"],
            &detail["contact"]["first_name"]
        ),
        (&json!(first), &json!("Ginásio"), &json!("Ana"))
    );
    assert!((detail["distance_m"].as_f64().unwrap() - 80.0).abs() <= 1.0);
    assert!(detail["contact"].get("phone").is_none());
    assert_eq!(shelters_in_db(&app).await, 1);
}

#[tokio::test]
async fn the_phone_is_shown_only_when_the_creator_allowed_it() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    app.send(
        "PATCH",
        "/v1/me",
        &ana.secret,
        Some(json!({ "phone": "+55 51 99999-0000", "show_phone": true })),
    )
    .await;
    app.create_shelter(&ana, "Ginásio", ORIGIN).await;
    let bia = app.actor("Bia").await;
    let (_, answer) = post(
        &app,
        &bia,
        shelter_body(&new_id(), "Escola", north_of_origin(80.0)),
    )
    .await;
    assert_eq!(
        answer["detail"]["contact"],
        json!({ "first_name": "Ana", "phone": "+55 51 99999-0000" })
    );

    // Hidden again once she changes her mind.
    app.send(
        "PATCH",
        "/v1/me",
        &ana.secret,
        Some(json!({ "show_phone": false })),
    )
    .await;
    let (_, answer) = post(
        &app,
        &bia,
        shelter_body(&new_id(), "Escola", north_of_origin(80.0)),
    )
    .await;
    assert_eq!(answer["detail"]["contact"], json!({ "first_name": "Ana" }));
}

#[tokio::test]
async fn the_radius_comes_from_the_configuration_and_a_far_shelter_is_not_a_neighbour() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    app.create_shelter(&ana, "Ginásio", ORIGIN).await;
    let bia = app.actor("Bia").await;
    assert_eq!(
        post(
            &app,
            &bia,
            shelter_body(&new_id(), "Escola", north_of_origin(101.0))
        )
        .await
        .0,
        StatusCode::CREATED
    );
    assert_eq!(
        post(
            &app,
            &bia,
            shelter_body(&new_id(), "Igreja", north_of_origin(99.0))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn an_explanation_creates_the_shelter_flagged_and_alerts_the_civil_defense() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let first = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
    let bia = app.actor("Bia").await;
    let id = new_id();
    let mut body = shelter_body(&id, "Escola", north_of_origin(80.0));
    body["other_site_reason"] = json!("Outro prédio, do outro lado da rua");
    let (status, created) = post(&app, &bia, body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["shelter"]["flagged"], true);

    let stored: ShelterDoc = store::get(&app.state.db, COLLECTION, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.conflicts_with.as_deref(), Some(first.as_str()));
    assert_eq!(
        stored.other_site_reason.as_deref(),
        Some("Outro prédio, do outro lado da rua")
    );

    let alerts = all_docs(&app.state.db, "alerts").await;
    assert_eq!(alerts.len(), 1);
    assert_eq!(
        (
            &alerts[0]["kind"],
            &alerts[0]["target_id"],
            &alerts[0]["for_civil_defense"],
            &alerts[0]["informational"]
        ),
        (
            &json!("shelter_flagged"),
            &json!(id),
            &json!(true),
            &json!(false)
        )
    );
    assert_eq!(alerts[0]["detail"]["conflicts_with"], json!(first));
    // The count the dashboard will make (phase 6) is a query on `flagged`.
    let flagged: Vec<ShelterDoc> = app
        .state
        .db
        .fluent()
        .select()
        .from(COLLECTION)
        .filter(|q| q.for_all([q.field(firestore::path!(ShelterDoc::flagged)).eq(true)]))
        .obj()
        .query()
        .await
        .unwrap();
    assert_eq!(flagged.len(), 1);
}

#[tokio::test]
async fn an_explanation_with_no_neighbour_creates_an_ordinary_shelter() {
    // Owner's decision: the reason only applies to a conflict, so with nothing nearby the
    // shelter is normal, not flagged, and raises no alert.
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let mut body = shelter_body(&new_id(), "Ginásio", ORIGIN);
    body["other_site_reason"] = json!("só por garantia");
    let (_, created) = post(&app, &ana, body).await;
    assert_eq!(created["shelter"]["flagged"], false);
    assert!(all_docs(&app.state.db, "alerts").await.is_empty());
}

#[tokio::test]
async fn a_closed_shelter_is_not_a_neighbour() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let first = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
    let (status, _) = app
        .send(
            "POST",
            &format!("/v1/shelters/{first}/close"),
            &ana.secret,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let bia = app.actor("Bia").await;
    assert_eq!(
        post(
            &app,
            &bia,
            shelter_body(&new_id(), "Escola", north_of_origin(50.0))
        )
        .await
        .0,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn sending_the_same_creation_again_gives_the_same_answer_and_writes_nothing() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let id = new_id();
    let body = shelter_body(&id, "Ginásio", ORIGIN);
    let (status, first) = post(&app, &ana, body.clone()).await;
    assert_eq!(status, StatusCode::CREATED);
    let audits = audit_actions(&app.state.db).await.len();
    let (status, again) = post(&app, &ana, body.clone()).await;
    assert_eq!((status, &again), (StatusCode::CREATED, &first));
    assert_eq!(shelters_in_db(&app).await, 1);
    assert_eq!(audit_actions(&app.state.db).await.len(), audits);

    // Someone else cannot take over an id.
    let bia = app.actor("Bia").await;
    let (status, answer) = post(&app, &bia, body).await;
    assert_eq!(
        (status, error_of(&answer)),
        (StatusCode::CONFLICT, Some("shelter_exists"))
    );
}

// ---- 12: concurrent creations ----

#[tokio::test]
async fn two_simultaneous_creations_50_metres_apart_make_only_one_shelter() {
    let app = TestApp::new().await;
    let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
    let (a, b) = tokio::join!(
        post(&app, &ana, shelter_body(&new_id(), "Ginásio", ORIGIN)),
        post(
            &app,
            &bia,
            shelter_body(&new_id(), "Escola", north_of_origin(50.0))
        ),
    );
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::CREATED, StatusCode::CONFLICT]);
    assert_eq!(shelters_in_db(&app).await, 1);
    assert_eq!(all_docs(&app.state.db, "memberships").await.len(), 1);
}

#[tokio::test]
async fn many_simultaneous_creations_within_the_radius_make_exactly_one() {
    let app = TestApp::new().await;
    let mut actors = Vec::new();
    for n in 0..8 {
        actors.push(app.actor(&format!("V{n}")).await);
    }
    let handles: Vec<_> = actors
        .iter()
        .enumerate()
        .map(|(n, who)| {
            let (app, who) = (app.clone(), who.clone());
            let body = shelter_body(
                &new_id(),
                &format!("Abrigo {n}"),
                north_of_origin(10.0 * n as f64),
            );
            tokio::spawn(async move { post(&app, &who, body).await })
        })
        .collect();
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    let created = results
        .iter()
        .filter(|r| r.0 == StatusCode::CREATED)
        .count();
    let refused = results
        .iter()
        .filter(|r| r.0 == StatusCode::CONFLICT)
        .count();
    assert_eq!((created, refused), (1, 7), "{results:?}");
    assert_eq!(shelters_in_db(&app).await, 1);
}

#[tokio::test]
async fn the_transaction_alone_also_refuses_a_second_shelter_next_to_the_first() {
    // Without the process lock, as with more than one instance: the neighbours are read
    // inside the transaction, so a creation that raced loses and runs again.
    let app = TestApp::new().await;
    let db = app.state.db.clone();
    let mut handles = Vec::new();
    for n in 0..6 {
        let db = db.clone();
        handles.push(tokio::spawn(async move {
            let new = request_for(&format!("Abrigo {n}"), north_of_origin(10.0 * f64::from(n)));
            let creator = format!("creator-{n}");
            store::transact(&db, |db, tx| {
                let (creator, new) = (creator.clone(), new.clone());
                Box::pin(async move {
                    create_in(&db, tx, &creator, &new, 100.0, 50)
                        .await
                        .map(|c| c.event_id)
                })
            })
            .await
        }));
    }
    let mut created = 0;
    for handle in handles {
        match handle.await.unwrap() {
            Ok(_) => created += 1,
            Err(e) => assert_eq!(e.code, "shelter_nearby", "{e}"),
        }
    }
    assert_eq!(created, 1);
}

fn request_for(name: &str, at: (f64, f64)) -> NewShelter {
    validate(serde_json::from_value(shelter_body(&new_id(), name, at)).unwrap()).unwrap()
}

// ---- 10: events ----

async fn event_of_shelter(app: &TestApp, id: &str) -> String {
    let stored: ShelterDoc = store::get(&app.state.db, COLLECTION, id)
        .await
        .unwrap()
        .unwrap();
    stored.event_id.unwrap()
}

#[tokio::test]
async fn a_shelter_40_km_away_joins_the_event() {
    let app = TestApp::new().await;
    let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
    let first = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
    let near = app
        .create_shelter(&bia, "Escola", north_of_origin(40_000.0))
        .await;
    assert_eq!(
        event_of_shelter(&app, &near).await,
        event_of_shelter(&app, &first).await
    );
    assert_eq!(all_docs(&app.state.db, "events").await.len(), 1);
    assert!(all_docs(&app.state.db, "alerts").await.is_empty());
}

#[tokio::test]
async fn a_shelter_60_km_away_starts_another_event() {
    let app = TestApp::new().await;
    let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
    let first = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
    let far = app
        .create_shelter(&bia, "Igreja", north_of_origin(60_000.0))
        .await;
    assert_ne!(
        event_of_shelter(&app, &far).await,
        event_of_shelter(&app, &first).await
    );
    assert_eq!(all_docs(&app.state.db, "events").await.len(), 2);
    assert!(all_docs(&app.state.db, "alerts").await.is_empty());
}

#[tokio::test]
async fn reaching_two_events_joins_the_nearest_shelters_event_and_raises_an_overlap() {
    let app = TestApp::new().await;
    let (ana, bia, cai) = (
        app.actor("Ana").await,
        app.actor("Bia").await,
        app.actor("Cai").await,
    );
    let a = app.create_shelter(&ana, "A", ORIGIN).await;
    let b = app
        .create_shelter(&bia, "B", north_of_origin(70_000.0))
        .await;
    let (event_a, event_b) = (
        event_of_shelter(&app, &a).await,
        event_of_shelter(&app, &b).await,
    );
    assert_ne!(event_a, event_b);

    // 30 km from A and 40 km from B: both events are in range, A's shelter is nearer.
    let c = app
        .create_shelter(&cai, "C", north_of_origin(30_000.0))
        .await;
    assert_eq!(event_of_shelter(&app, &c).await, event_a);
    let alerts = all_docs(&app.state.db, "alerts").await;
    assert_eq!(alerts.len(), 1);
    assert_eq!(
        (
            &alerts[0]["kind"],
            &alerts[0]["target_id"],
            &alerts[0]["event_id"],
            &alerts[0]["for_civil_defense"]
        ),
        (
            &json!("event_overlap"),
            &json!(c),
            &json!(event_a),
            &json!(true)
        )
    );
    let mut in_range: Vec<String> =
        serde_json::from_value(alerts[0]["detail"]["event_ids"].clone()).unwrap();
    in_range.sort();
    let mut expected = vec![event_a.clone(), event_b.clone()];
    expected.sort();
    assert_eq!(in_range, expected);
    // Nothing is merged: both events stay active and untouched.
    let events = all_docs(&app.state.db, "events").await;
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e["status"] == "active"));
}

#[tokio::test]
async fn a_closed_shelter_does_not_bring_its_event_into_range() {
    let app = TestApp::new().await;
    let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
    let a = app.create_shelter(&ana, "A", ORIGIN).await;
    app.send(
        "POST",
        &format!("/v1/shelters/{a}/close"),
        &ana.secret,
        None,
    )
    .await;
    let b = app
        .create_shelter(&bia, "B", north_of_origin(10_000.0))
        .await;
    assert_ne!(
        event_of_shelter(&app, &a).await,
        event_of_shelter(&app, &b).await
    );
}

#[tokio::test]
async fn the_first_address_names_the_event_and_later_ones_do_not() {
    let app = TestApp::new().await;
    let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
    let a = app.create_shelter(&ana, "A", ORIGIN).await;
    let b = app
        .create_shelter(&bia, "B", north_of_origin(1_000.0))
        .await;
    let event_id = event_of_shelter(&app, &a).await;
    let name_of = |app: &TestApp| {
        let (db, event_id) = (app.state.db.clone(), event_id.clone());
        async move {
            store::get::<EventDoc>(&db, events::COLLECTION, &event_id)
                .await
                .unwrap()
                .unwrap()
                .name
        }
    };
    assert!(name_of(&app).await.starts_with("Evento – "));

    let address = json!({ "address": { "text": "Rua A, 1", "city": "Canoas", "state": "RS" } });
    let (status, body) = app
        .send(
            "PATCH",
            &format!("/v1/shelters/{a}"),
            &ana.secret,
            Some(address),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["shelter"]["address"],
        json!({ "text": "Rua A, 1", "city": "Canoas", "state": "RS", "pending": false })
    );
    assert!(name_of(&app).await.starts_with("Canoas/RS – "));
    assert_eq!(body["shelter"]["event"]["name"], json!(name_of(&app).await));

    let other = json!({ "address": { "city": "Esteio", "state": "RS" } });
    app.send(
        "PATCH",
        &format!("/v1/shelters/{b}"),
        &bia.secret,
        Some(other),
    )
    .await;
    assert!(
        name_of(&app).await.starts_with("Canoas/RS – "),
        "already named after a city"
    );
}

#[tokio::test]
async fn an_event_renamed_by_a_person_keeps_its_name_when_an_address_arrives() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let a = app.create_shelter(&ana, "A", ORIGIN).await;
    let event_id = event_of_shelter(&app, &a).await;
    let (status, _) = app
        .send(
            "PATCH",
            &format!("/v1/events/{event_id}"),
            &ana.secret,
            Some(json!({ "name": "Cheia de outubro" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    app.send(
        "PATCH",
        &format!("/v1/shelters/{a}"),
        &ana.secret,
        Some(json!({ "address": { "city": "Canoas", "state": "RS" } })),
    )
    .await;
    let event: EventDoc = store::get(&app.state.db, events::COLLECTION, &event_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (event.name.as_str(), event.auto_named),
        ("Cheia de outubro", false)
    );
}

#[tokio::test]
async fn writes_to_a_shelter_record_the_event_activity() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let a = app.create_shelter(&ana, "A", ORIGIN).await;
    let event_id = event_of_shelter(&app, &a).await;
    // Age the stored time and forget the cache, as an hour later would.
    let mut event: EventDoc = store::get(&app.state.db, events::COLLECTION, &event_id)
        .await
        .unwrap()
        .unwrap();
    let old = event.last_activity_at - firestore::jiff::SignedDuration::from_hours(48);
    event.last_activity_at = old;
    store::replace(&app.state.db, events::COLLECTION, &event_id, &event)
        .await
        .unwrap();
    app.state.event_activity.lock().unwrap().clear();

    app.send(
        "PATCH",
        &format!("/v1/shelters/{a}"),
        &ana.secret,
        Some(json!({ "capacity": 50 })),
    )
    .await;
    let after: EventDoc = store::get(&app.state.db, events::COLLECTION, &event_id)
        .await
        .unwrap()
        .unwrap();
    assert!(after.last_activity_at > old + firestore::jiff::SignedDuration::from_hours(47));
    // The name was not touched by the activity write.
    assert_eq!(after.name, event.name);

    // Within the hour a second write leaves it alone.
    let marker = after.last_activity_at;
    app.send(
        "PATCH",
        &format!("/v1/shelters/{a}"),
        &ana.secret,
        Some(json!({ "capacity": 60 })),
    )
    .await;
    let again: EventDoc = store::get(&app.state.db, events::COLLECTION, &event_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.last_activity_at, marker);
}

// ---- who sees and changes what ----

#[tokio::test]
async fn members_validators_and_the_civil_defense_see_all_and_others_see_the_basics() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let id = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
    let stranger = app.actor("Ze").await;
    let validator = app.actor("Val").await;
    app.grant(&validator, crate::volunteers::GlobalRole::Validator)
        .await;

    let (status, own) = app
        .send("GET", &format!("/v1/shelters/{id}"), &ana.secret, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (&own["shelter"]["role"], &own["shelter"]["lat"]),
        (&json!("admin"), &json!(ORIGIN.0))
    );
    let (_, seen_by_validator) = app
        .send(
            "GET",
            &format!("/v1/shelters/{id}"),
            &validator.secret,
            None,
        )
        .await;
    assert_eq!(seen_by_validator["shelter"]["role"], Value::Null);
    assert_eq!(seen_by_validator["shelter"]["lat"], json!(ORIGIN.0));

    let (_, basics) = app
        .send("GET", &format!("/v1/shelters/{id}"), &stranger.secret, None)
        .await;
    assert_eq!(
        basics["shelter"],
        json!({ "id": id, "name": "Ginásio", "confirmation": "unconfirmed", "capacity": null, "occupancy": 0, "vacancies": null })
    );
    let (status, missing) = app
        .send(
            "GET",
            &format!("/v1/shelters/{}", new_id()),
            &ana.secret,
            None,
        )
        .await;
    assert_eq!(
        (status, error_of(&missing)),
        (StatusCode::NOT_FOUND, Some("shelter_not_found"))
    );
    assert_eq!(
        app.request("GET", &format!("/v1/shelters/{id}"), &[], None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn members_see_the_items_and_strangers_do_not() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let stranger = app.actor("Ze").await;
    let id = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
    let path = format!("/v1/shelters/{id}");

    let (_, empty) = app.send("GET", &path, &ana.secret, None).await;
    assert_eq!(empty["shelter"]["items"], json!({}));

    let mut shelter: ShelterDoc = store::get(&app.state.db, COLLECTION, &id)
        .await
        .unwrap()
        .unwrap();
    shelter.items.insert(
        "cesta".into(),
        crate::deliveries::ItemSpec {
            scope: crate::deliveries::ItemScope::PerHousehold,
            period_days: Some(7),
            once_per_event: false,
            high_value: false,
        },
    );
    store::replace(&app.state.db, COLLECTION, &id, &shelter)
        .await
        .unwrap();

    let (_, own) = app.send("GET", &path, &ana.secret, None).await;
    assert_eq!(
        own["shelter"]["items"],
        json!({ "cesta": { "scope": "per_household", "period_days": 7, "once_per_event": false, "high_value": false } })
    );
    let (_, basics) = app.send("GET", &path, &stranger.secret, None).await;
    assert!(basics["shelter"].get("items").is_none());
}

#[tokio::test]
async fn patch_follows_the_permission_matrix() {
    let app = TestApp::new().await;
    let admin = app.actor("Admin").await;
    let id = app.create_shelter(&admin, "Ginásio", ORIGIN).await;
    let volunteer = app
        .join(&app.invite(&admin, &id, "qr", "volunteer").await, "Vol")
        .await;
    let civil_defense = app.actor("DC").await;
    app.grant(&civil_defense, crate::volunteers::GlobalRole::CivilDefense)
        .await;
    let validator = app.actor("Val").await;
    app.grant(&validator, crate::volunteers::GlobalRole::Validator)
        .await;
    let stranger = app.actor("Ze").await;
    let path = format!("/v1/shelters/{id}");

    let rename = json!({ "name": "Novo nome" });
    let capacity = json!({ "capacity": 80 });
    let expect = |who: &crate::test_support::Actor, body: &Value, status: StatusCode| {
        let (app, path, who, body) = (&app, path.clone(), who.clone(), body.clone());
        async move {
            let (got, answer) = app
                .send("PATCH", &path, &who.secret, Some(body.clone()))
                .await;
            assert_eq!(got, status, "{} {body}: {answer}", who.id);
        }
    };
    // Name and address: the admin only. Capacity: the admin and the Civil Defense.
    for who in [&volunteer, &civil_defense, &validator, &stranger] {
        expect(who, &rename, StatusCode::FORBIDDEN).await;
    }
    for who in [&volunteer, &validator, &stranger] {
        expect(who, &capacity, StatusCode::FORBIDDEN).await;
    }
    expect(&civil_defense, &capacity, StatusCode::OK).await;
    expect(
        &civil_defense,
        &json!({ "capacity": 90, "name": "x" }),
        StatusCode::FORBIDDEN,
    )
    .await;
    expect(&admin, &rename, StatusCode::OK).await;
    expect(&admin, &json!({ "capacity": 70 }), StatusCode::OK).await;
    let stored: ShelterDoc = store::get(&app.state.db, COLLECTION, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (stored.name.as_str(), stored.capacity),
        ("Novo nome", Some(70))
    );
}

#[tokio::test]
async fn capacity_can_be_cleared_and_must_be_sensible() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let id = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
    let path = format!("/v1/shelters/{id}");
    let (_, set) = app
        .send(
            "PATCH",
            &path,
            &ana.secret,
            Some(json!({ "capacity": 200 })),
        )
        .await;
    assert_eq!(set["shelter"]["capacity"], 200);
    let (_, kept) = app
        .send(
            "PATCH",
            &path,
            &ana.secret,
            Some(json!({ "name": "Outro" })),
        )
        .await;
    assert_eq!(
        kept["shelter"]["capacity"], 200,
        "a key left out keeps the value"
    );
    let (_, cleared) = app
        .send(
            "PATCH",
            &path,
            &ana.secret,
            Some(json!({ "capacity": null })),
        )
        .await;
    assert_eq!(cleared["shelter"]["capacity"], Value::Null);
    for bad in [
        json!({ "capacity": 0 }),
        json!({ "name": " " }),
        json!({ "address": {} }),
    ] {
        let (status, answer) = app.send("PATCH", &path, &ana.secret, Some(bad)).await;
        assert_eq!(
            (status, error_of(&answer)),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_shelter"))
        );
    }
}

#[tokio::test]
async fn only_an_admin_or_the_civil_defense_closes_a_shelter() {
    let app = TestApp::new().await;
    let admin = app.actor("Admin").await;
    let id = app.create_shelter(&admin, "Ginásio", ORIGIN).await;
    let volunteer = app
        .join(&app.invite(&admin, &id, "qr", "volunteer").await, "Vol")
        .await;
    let validator = app.actor("Val").await;
    app.grant(&validator, crate::volunteers::GlobalRole::Validator)
        .await;
    let civil_defense = app.actor("DC").await;
    app.grant(&civil_defense, crate::volunteers::GlobalRole::CivilDefense)
        .await;
    let path = format!("/v1/shelters/{id}/close");
    for who in [&volunteer, &validator] {
        assert_eq!(
            app.send("POST", &path, &who.secret, None).await.0,
            StatusCode::FORBIDDEN
        );
    }
    let (status, closed) = app.send("POST", &path, &civil_defense.secret, None).await;
    assert_eq!(
        (status, &closed["shelter"]["status"]),
        (StatusCode::OK, &json!("closed"))
    );
    // Closing again is harmless and not audited twice.
    let audits = audit_actions(&app.state.db)
        .await
        .iter()
        .filter(|a| *a == "shelter.close")
        .count();
    assert_eq!(
        app.send("POST", &path, &admin.secret, None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        audit_actions(&app.state.db)
            .await
            .iter()
            .filter(|a| *a == "shelter.close")
            .count(),
        audits
    );
}

#[tokio::test]
async fn only_a_validator_or_the_civil_defense_confirms() {
    let app = TestApp::new().await;
    let admin = app.actor("Admin").await;
    let id = app.create_shelter(&admin, "Ginásio", ORIGIN).await;
    let volunteer = app
        .join(&app.invite(&admin, &id, "qr", "volunteer").await, "Vol")
        .await;
    let mediator = app.actor("Med").await;
    app.grant(&mediator, crate::volunteers::GlobalRole::Mediator)
        .await;
    let path = format!("/v1/shelters/{id}/confirm");
    for who in [&admin, &volunteer, &mediator] {
        assert_eq!(
            app.send("POST", &path, &who.secret, None).await.0,
            StatusCode::FORBIDDEN,
            "{}",
            who.id
        );
    }
    let validator = app.actor("Val").await;
    app.grant(&validator, crate::volunteers::GlobalRole::Validator)
        .await;
    let (status, body) = app.send("POST", &path, &validator.secret, None).await;
    assert_eq!(status, StatusCode::OK);
    let shelter = &body["shelter"];
    assert_eq!(
        (
            &shelter["confirmation"],
            &shelter["confirmation_type"],
            &shelter["can_endorse"]
        ),
        (&json!("confirmed"), &json!("validator"), &json!(true))
    );
    // The admin now sees it confirmed in `GET /me`.
    let (_, me) = app.get_authed("/v1/me", &admin.secret).await;
    assert_eq!(me["memberships"][0]["confirmation"], "confirmed");
    // Confirming again changes nothing.
    let audits = audit_actions(&app.state.db)
        .await
        .iter()
        .filter(|a| *a == "shelter.confirm")
        .count();
    assert_eq!(
        app.send("POST", &path, &validator.secret, None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        audit_actions(&app.state.db)
            .await
            .iter()
            .filter(|a| *a == "shelter.confirm")
            .count(),
        audits
    );
}

#[tokio::test]
async fn revoking_needs_a_reason_and_goes_back_to_unconfirmed() {
    let app = TestApp::new().await;
    let admin = app.actor("Admin").await;
    let id = app.confirmed_shelter(&admin, "Ginásio", ORIGIN).await;
    let civil_defense = app.actor("DC").await;
    app.grant(&civil_defense, crate::volunteers::GlobalRole::CivilDefense)
        .await;
    let path = format!("/v1/shelters/{id}/revoke");
    assert_eq!(
        app.send("POST", &path, &admin.secret, Some(json!({ "reason": "x" })))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, answer) = app
        .send(
            "POST",
            &path,
            &civil_defense.secret,
            Some(json!({ "reason": "  " })),
        )
        .await;
    assert_eq!(
        (status, error_of(&answer)),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_shelter"))
    );
    let (status, body) = app
        .send(
            "POST",
            &path,
            &civil_defense.secret,
            Some(json!({ "reason": "Não existe" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let shelter = &body["shelter"];
    assert_eq!(
        (
            &shelter["confirmation"],
            &shelter["confirmation_type"],
            &shelter["can_endorse"]
        ),
        (&json!("unconfirmed"), &Value::Null, &json!(false))
    );
    let stored: ShelterDoc = store::get(&app.state.db, COLLECTION, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            stored.revocations.len(),
            stored.revocations[0].reason.as_str(),
            stored.revocations[0].by.as_str()
        ),
        (1, "Não existe", civil_defense.id.as_str())
    );
    // Revoking what is not confirmed does nothing.
    assert_eq!(
        app.send(
            "POST",
            &path,
            &civil_defense.secret,
            Some(json!({ "reason": "de novo" }))
        )
        .await
        .0,
        StatusCode::OK
    );
    let stored: ShelterDoc = store::get(&app.state.db, COLLECTION, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.revocations.len(), 1);
}

mod follow_up_tests {
    use super::*;
    use crate::test_support::{ORIGIN, TestApp, all_docs, north_of_origin, shelter_body};

    #[tokio::test]
    async fn a_shelter_with_an_address_names_an_event_that_had_only_a_placeholder() {
        let app = TestApp::new().await;
        let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
        // The first shelter has no address yet, so the event is "Evento – MM/YYYY".
        let first = app.create_shelter(&ana, "A", ORIGIN).await;
        let mut body = shelter_body(
            &crate::test_support::new_id(),
            "B",
            north_of_origin(1_000.0),
        );
        body["address"] = json!({ "text": "Rua B, 2", "city": "Canoas", "state": "RS" });
        let (status, created) = app
            .send("POST", "/v1/shelters", &bia.secret, Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let name = created["event"]["name"].as_str().unwrap();
        assert!(name.starts_with("Canoas/RS – "), "{name}");
        let stored: ShelterDoc = store::get(&app.state.db, COLLECTION, &first)
            .await
            .unwrap()
            .unwrap();
        let event: EventDoc = store::get(
            &app.state.db,
            events::COLLECTION,
            stored.event_id.as_deref().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!((event.name.as_str(), event.auto_named), (name, true));
    }

    #[tokio::test]
    async fn shelter_audit_entries_name_the_shelter() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.create_shelter(&admin, "A", ORIGIN).await;
        let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let volunteer = app.join(&code, "Vol").await;
        app.send(
            "POST",
            &format!("/v1/shelters/{shelter}/leave"),
            &volunteer.secret,
            None,
        )
        .await;
        app.send(
            "PATCH",
            &format!("/v1/shelters/{shelter}"),
            &admin.secret,
            Some(json!({ "capacity": 10 })),
        )
        .await;
        let entries = all_docs(&app.state.db, "audit_log").await;
        for action in [
            "shelter.create",
            "shelter.update",
            "invite.create",
            "invite.accept",
            "member.leave",
        ] {
            let entry = entries
                .iter()
                .find(|e| e["action"] == action)
                .unwrap_or_else(|| panic!("{action}"));
            assert_eq!(entry["shelter_id"], json!(shelter), "{action}");
        }
        // A login is not about a shelter.
        let logins: Vec<_> = entries
            .iter()
            .filter(|e| e["action"] == "capture.create")
            .collect();
        assert!(!logins.is_empty() && logins.iter().all(|e| e.get("shelter_id").is_none()));
    }

    #[tokio::test]
    async fn me_lists_the_shelters_by_name() {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let zulu = app.create_shelter(&ana, "Zulu", ORIGIN).await;
        let alfa = app
            .create_shelter(&ana, "Alfa", north_of_origin(5_000.0))
            .await;
        let (_, me) = app.get_authed("/v1/me", &ana.secret).await;
        let names: Vec<&str> = me["memberships"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Alfa", "Zulu"]);
        assert!(
            me["memberships"][0]["shelter_id"] == json!(alfa)
                && me["memberships"][1]["shelter_id"] == json!(zulu)
        );
    }

    #[tokio::test]
    async fn the_flagged_shelter_alert_is_for_the_civil_defense_not_the_creator() {
        let app = TestApp::new().await;
        let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
        app.create_shelter(&ana, "Ginásio", ORIGIN).await;
        let mut body = shelter_body(
            &crate::test_support::new_id(),
            "Escola",
            north_of_origin(80.0),
        );
        body["other_site_reason"] = json!("Outro prédio");
        assert_eq!(
            app.send("POST", "/v1/shelters", &bia.secret, Some(body))
                .await
                .0,
            StatusCode::CREATED
        );
        let alert_id = all_docs(&app.state.db, "alerts").await[0]["_firestore_id"]
            .as_str()
            .unwrap()
            .to_string();

        // Neither the creator of the flagged shelter nor the admin of the shelter it conflicts with.
        for who in [&bia, &ana] {
            let (status, listing) = app.send("GET", "/v1/alerts", &who.secret, None).await;
            assert_eq!(
                (status, listing["alerts"].as_array().unwrap().len()),
                (StatusCode::OK, 0),
                "{}",
                who.id
            );
            let (status, _) = app
                .send(
                    "POST",
                    &format!("/v1/alerts/{alert_id}/resolve"),
                    &who.secret,
                    None,
                )
                .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{}", who.id);
        }
        let dc = app.actor("DC").await;
        app.grant(&dc, GlobalRole::CivilDefense).await;
        let (_, listing) = app.send("GET", "/v1/alerts", &dc.secret, None).await;
        assert_eq!(listing["alerts"].as_array().unwrap().len(), 1);
        assert_eq!(
            app.send(
                "POST",
                &format!("/v1/alerts/{alert_id}/resolve"),
                &dc.secret,
                None
            )
            .await
            .0,
            StatusCode::OK
        );
    }
}

mod domain_tests {
    use super::*;
    use crate::store;

    fn at(micros: i64) -> FirestoreInstant {
        FirestoreInstant::from_microsecond(1_800_000_000_000_000 + micros).unwrap()
    }

    fn new_shelter(neighbour: Option<Neighbour>) -> ShelterDoc {
        let new = NewShelter {
            id: "s1".into(),
            name: "Abrigo".into(),
            lat: -30.0,
            lon: -51.0,
            accuracy_m: 10.0,
            address: Address::pending(),
            capacity: Some(10),
            other_site_reason: None,
            client_created_at: store::now(),
        };
        ShelterDoc::open(&new, "creator", "e1", neighbour, at(0))
    }

    fn endorsement(volunteer: &str, when: FirestoreInstant) -> Endorsement {
        Endorsement {
            kind: EndorsementKind::InPerson,
            volunteer_id: volunteer.into(),
            endorser_shelter_id: "other".into(),
            at: when,
            distance_m: 5.0,
        }
    }

    fn volunteers_of(flagged: Option<Vec<Endorsement>>) -> Vec<String> {
        flagged
            .unwrap()
            .into_iter()
            .map(|e| e.volunteer_id)
            .collect()
    }

    #[test]
    fn a_new_shelter_is_open_and_unconfirmed_and_flagged_only_next_to_another() {
        let plain = new_shelter(None);
        assert_eq!(plain.status, ShelterStatus::Open);
        assert_eq!(plain.confirmation, Confirmation::Unconfirmed);
        assert!(!plain.can_endorse);
        assert!(
            !plain.flagged && plain.conflicts_with.is_none() && plain.other_site_reason.is_none()
        );

        let near = new_shelter(Some(Neighbour {
            shelter_id: "s0".into(),
            reason: "other site".into(),
        }));
        assert!(near.flagged);
        assert_eq!(near.conflicts_with.as_deref(), Some("s0"));
        assert_eq!(near.other_site_reason.as_deref(), Some("other site"));
        assert_eq!(near.confirmation, Confirmation::Unconfirmed);
    }

    /// A repeated item does not repeat its effect: closing again changes nothing.
    #[test]
    fn closing_twice_changes_once() {
        let mut shelter = new_shelter(None);
        assert!(shelter.close());
        assert!(!shelter.close());
        assert_eq!(shelter.status, ShelterStatus::Closed);
    }

    /// Both routes that confirm go through `confirm`, and whichever came first stays: a second
    /// confirmation neither changes the type nor reports a change.
    #[test]
    fn confirming_twice_changes_once_and_keeps_the_first_type() {
        let mut shelter = new_shelter(None);
        assert!(shelter.confirm(ConfirmationType::Validator));
        assert!(shelter.can_endorse);
        assert!(!shelter.confirm(ConfirmationType::InPerson));
        assert_eq!(shelter.confirmation_type, Some(ConfirmationType::Validator));

        let mut shelter = new_shelter(None);
        shelter.endorse_in_person(endorsement("v1", at(1)));
        assert_eq!(shelter.confirmation, Confirmation::Confirmed);
        assert_eq!(shelter.confirmation_type, Some(ConfirmationType::InPerson));
        assert!(shelter.can_endorse);
        assert_eq!(shelter.endorsements.len(), 1);
    }

    /// A repeated revocation has no effect: nobody is flagged twice and no second revocation is
    /// recorded.
    #[test]
    fn revoking_twice_revokes_and_flags_once() {
        let mut shelter = new_shelter(None);
        shelter.endorse_in_person(endorsement("v1", at(1)));
        shelter.endorse_in_person(endorsement("v1", at(2)));
        shelter.endorse_in_person(endorsement("v2", at(3)));

        let flagged = shelter.revoke("validator", "reason", at(10));
        assert_eq!(volunteers_of(flagged), ["v1", "v2"], "one per volunteer");
        assert_eq!(shelter.confirmation, Confirmation::Unconfirmed);
        assert_eq!(shelter.confirmation_type, None);
        assert!(!shelter.can_endorse);
        assert_eq!(shelter.revocations.len(), 1);

        assert!(shelter.revoke("validator", "again", at(11)).is_none());
        assert_eq!(shelter.revocations.len(), 1);
        assert_eq!(shelter.revocations[0].reason, "reason");
    }

    /// A revocation flags the endorsers its confirmation rested on, not those an earlier one
    /// already flagged.
    #[test]
    fn a_second_revocation_flags_only_the_endorsers_since_the_first() {
        let mut shelter = new_shelter(None);
        shelter.endorse_in_person(endorsement("v1", at(1)));
        shelter.revoke("validator", "first", at(10)).unwrap();
        shelter.endorse_in_person(endorsement("v2", at(20)));
        let flagged = shelter.revoke("validator", "second", at(30));
        assert_eq!(volunteers_of(flagged), ["v2"]);
        assert_eq!(shelter.revocations.len(), 2);
        assert_eq!(shelter.endorsements.len(), 2, "the history is kept");
    }

    #[test]
    fn revoking_a_shelter_a_validator_confirmed_flags_no_one() {
        let mut shelter = new_shelter(None);
        shelter.confirm(ConfirmationType::Validator);
        let flagged = shelter.revoke("validator", "reason", at(10));
        assert!(volunteers_of(flagged).is_empty());
        assert_eq!(shelter.confirmation, Confirmation::Unconfirmed);
    }
}
