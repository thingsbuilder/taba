//! What the phase 3 modules do together: who may ask for a capture and for what, the child alert
//! on check-in, and the degraded mode across Unico-dependent and independent routes.

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::degraded;
use crate::store;
use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, new_id, north_of_origin};
use crate::volunteers::GlobalRole;

fn error_of(body: &Value) -> Option<&str> {
    body["error"].as_str()
}

/// A request with the actor's session and device.
async fn call(
    app: &TestApp,
    who: &Actor,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let auth = format!("Bearer {}", who.secret);
    let (status, _, json) = app
        .request(
            method,
            path,
            &[
                ("authorization", auth.as_str()),
                ("x-device-id", who.device.as_str()),
            ],
            body,
        )
        .await;
    (status, json)
}

fn checkin_body(extra: Value) -> Value {
    let mut body = json!({
        "id": new_id(),
        "legal_basis": "consent",
        "client_created_at": store::now().to_string(),
        "data": {},
    });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

async fn capture(app: &TestApp, who: &Actor, body: Value) -> (StatusCode, Value) {
    call(app, who, "POST", "/v1/captures", Some(body)).await
}

/// Finishes the journey of a capture with `token`.
async fn finish(app: &TestApp, capture_id: &str, token: &str) {
    let stored: crate::auth::CaptureDoc = store::get(&app.state.db, "captures", capture_id)
        .await
        .unwrap()
        .unwrap();
    app.unico.finish_with_token(&stored.process_id, token);
}

struct Scene {
    app: TestApp,
    admin: Actor,
    volunteer: Actor,
    shelter: String,
}

async fn scene() -> Scene {
    let app = TestApp::new().await;
    let admin = app.actor("Admin").await;
    let shelter = app.confirmed_shelter(&admin, "Abrigo", ORIGIN).await;
    let volunteer = app
        .join(
            &app.invite(&admin, &shelter, "qr", "volunteer").await,
            "Vol",
        )
        .await;
    Scene {
        app,
        admin,
        volunteer,
        shelter,
    }
}

// ---- POST /v1/captures: who may ask for what ----

#[tokio::test]
async fn session_captures_need_a_session() {
    let s = scene().await;
    let journeys = s.app.unico.post_count();
    for purpose in [
        json!({ "purpose": "checkin", "shelter_id": s.shelter }),
        json!({ "purpose": "biometrics", "expected_person_id": "p1" }),
        json!({ "purpose": "revalidation", "shelter_id": s.shelter, "expected_person_id": "p1" }),
    ] {
        let (status, body) = s.app.post("/v1/captures", "dev-x", purpose.clone()).await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::UNAUTHORIZED, Some("unauthorized")),
            "{purpose}"
        );
    }
    assert_eq!(
        s.app.unico.post_count(),
        journeys,
        "no journey is opened for them"
    );
}

#[tokio::test]
async fn session_captures_say_what_they_are_for() {
    let s = scene().await;
    let journeys = s.app.unico.post_count();
    for body in [
        json!({ "purpose": "checkin" }),
        json!({ "purpose": "biometrics" }),
        json!({ "purpose": "biometrics", "shelter_id": s.shelter }),
        json!({ "purpose": "revalidation", "shelter_id": s.shelter }),
        json!({ "purpose": "revalidation", "expected_person_id": "p1" }),
    ] {
        let (status, answer) = capture(&s.app, &s.volunteer, body.clone()).await;
        assert_eq!(
            (status, error_of(&answer)),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("capture_target_required")
            ),
            "{body}"
        );
    }
    assert_eq!(s.app.unico.post_count(), journeys);
}

#[tokio::test]
async fn a_capture_for_a_shelter_needs_a_member_of_it() {
    let s = scene().await;
    let stranger = s.app.actor("Stranger").await;
    let validator = s.app.actor("Validator").await;
    s.app.grant(&validator, GlobalRole::Validator).await;
    for who in [&stranger, &validator] {
        for body in [
            json!({ "purpose": "checkin", "shelter_id": s.shelter }),
            json!({ "purpose": "revalidation", "shelter_id": s.shelter, "expected_person_id": "p1" }),
            json!({ "purpose": "biometrics", "shelter_id": s.shelter, "expected_person_id": "p1" }),
        ] {
            let (status, answer) = capture(&s.app, who, body.clone()).await;
            assert_eq!(
                (status, error_of(&answer)),
                (StatusCode::FORBIDDEN, Some("forbidden")),
                "{} {body}",
                who.id
            );
        }
    }
    // Members get theirs; `protection` and the Civil Defense may also revalidate anywhere.
    for who in [&s.volunteer, &s.admin] {
        let (status, answer) = capture(
            &s.app,
            who,
            json!({ "purpose": "checkin", "shelter_id": s.shelter }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
    }
    let civil_defense = s.app.actor("DC").await;
    s.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
    let revalidation =
        json!({ "purpose": "revalidation", "shelter_id": s.shelter, "expected_person_id": "p1" });
    assert_eq!(
        capture(&s.app, &civil_defense, revalidation.clone())
            .await
            .0,
        StatusCode::CREATED
    );
    // ... but not check people in.
    let (status, _) = capture(
        &s.app,
        &civil_defense,
        json!({ "purpose": "checkin", "shelter_id": s.shelter }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_checkin_capture_only_checks_in_to_the_shelter_it_was_asked_for() {
    let s = scene().await;
    let other = s
        .app
        .create_shelter(&s.volunteer, "Outro", north_of_origin(5_000.0))
        .await;
    let (_, made) = capture(
        &s.app,
        &s.volunteer,
        json!({ "purpose": "checkin", "shelter_id": other }),
    )
    .await;
    let capture_id = made["capture_id"].as_str().unwrap();
    finish(&s.app, capture_id, "tok-ana").await;
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", s.shelter),
        Some(checkin_body(json!({ "capture_id": capture_id }))),
    )
    .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::FORBIDDEN, Some("capture_wrong_target")),
        "{body}"
    );
    assert!(all_docs(&s.app.state.db, "people").await.is_empty());
    // The capture is not spent by the refusal: the right shelter takes it.
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "POST",
        &format!("/v1/shelters/{other}/checkins"),
        Some(checkin_body(json!({ "capture_id": capture_id }))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn a_biometrics_capture_only_raises_the_person_it_was_asked_for() {
    let s = scene().await;
    let register = |name: &'static str| {
        let (app, who, shelter) = (s.app.clone(), s.volunteer.clone(), s.shelter.clone());
        async move {
            let (status, body) = call(
                &app,
                &who,
                "POST",
                &format!("/v1/shelters/{shelter}/checkins"),
                Some(checkin_body(
                    json!({ "no_biometrics": true, "data": { "name": name } }),
                )),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
            body["person_id"].as_str().unwrap().to_string()
        }
    };
    let (ana, bia) = (register("Ana").await, register("Bia").await);
    let (_, made) = capture(
        &s.app,
        &s.volunteer,
        json!({ "purpose": "biometrics", "expected_person_id": bia }),
    )
    .await;
    let capture_id = made["capture_id"].as_str().unwrap();
    finish(&s.app, capture_id, "tok-ana").await;
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "PUT",
        &format!("/v1/people/{ana}/biometrics"),
        Some(json!({ "capture_id": capture_id })),
    )
    .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::FORBIDDEN, Some("capture_wrong_target")),
        "{body}"
    );
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "PUT",
        &format!("/v1/people/{bia}/biometrics"),
        Some(json!({ "capture_id": capture_id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

// ---- criterion 42: a child alone raises the alert with the reference phones ----

#[tokio::test]
async fn an_unaccompanied_child_checking_in_raises_child_separated_with_the_reference_phones() {
    let s = scene().await;
    // The event's reference contacts, set by a validator.
    let validator = s.app.actor("Validator").await;
    s.app.grant(&validator, GlobalRole::Validator).await;
    let (_, shelter) = s
        .app
        .send(
            "GET",
            &format!("/v1/shelters/{}", s.shelter),
            &validator.secret,
            None,
        )
        .await;
    let event = shelter["shelter"]["event_id"].as_str().unwrap().to_string();
    let (status, _) = s
        .app
        .send("PATCH", &format!("/v1/events/{event}"), &validator.secret, Some(json!({ "reference_contacts": { "civil_defense_phone": "199", "child_protection_phone": "100" } })))
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", s.shelter),
        Some(checkin_body(json!({
            "no_biometrics": true,
            "legal_basis": "child_protection",
            "data": { "name": "Menina", "is_child": true, "child_status": "unaccompanied" },
        }))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let person_id = body["person_id"].as_str().unwrap();

    let alerts = all_docs(&s.app.state.db, "alerts").await;
    assert_eq!(alerts.len(), 1);
    assert_eq!(
        (
            &alerts[0]["kind"],
            &alerts[0]["target_id"],
            &alerts[0]["for_protection"],
            &alerts[0]["for_civil_defense"],
            &alerts[0]["shelter_ids"]
        ),
        (
            &json!("child_separated"),
            &json!(person_id),
            &json!(true),
            &json!(true),
            &json!([s.shelter])
        )
    );
    assert_eq!(
        alerts[0]["detail"]["reference_contacts"],
        json!({ "civil_defense_phone": "199", "child_protection_phone": "100" })
    );
    // The shelter's admin and the Civil Defense are the ones who see it.
    let (_, listing) = s.app.send("GET", "/v1/alerts", &s.admin.secret, None).await;
    assert_eq!(listing["alerts"].as_array().unwrap().len(), 1);
    assert_eq!(
        listing["alerts"][0]["detail"]["reference_contacts"]["child_protection_phone"],
        "100"
    );
    // The Civil Defense, who belongs to no shelter, gets it and reads the child it is about.
    let civil_defense = s.app.actor("DC").await;
    s.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
    let (_, listing) = s
        .app
        .send("GET", "/v1/alerts", &civil_defense.secret, None)
        .await;
    assert_eq!(listing["alerts"][0]["target_id"], json!(person_id));
    let path = format!("/v1/people/{person_id}");
    assert_eq!(
        call(&s.app, &civil_defense, "GET", &path, None).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_child_with_a_guardian_raises_no_alert_and_a_status_change_raises_one() {
    let s = scene().await;
    let (_, body) = call(
        &s.app,
        &s.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", s.shelter),
        Some(checkin_body(json!({ "no_biometrics": true, "legal_basis": "child_protection", "data": { "name": "Menino", "is_child": true, "child_status": "with_guardian" } }))),
    )
    .await;
    let person_id = body["person_id"].as_str().unwrap().to_string();
    assert!(all_docs(&s.app.state.db, "alerts").await.is_empty());

    // Only protection or the Civil Defense change the status (spec 3.4.1, item 4).
    // (The Civil Defense officer here also belongs to the shelter: `GET`/`PATCH /people` only show
    // people of the shelters one belongs to, see the open question in the PR.)
    let civil_defense = s
        .app
        .join(
            &s.app.invite(&s.admin, &s.shelter, "qr", "volunteer").await,
            "DC",
        )
        .await;
    s.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
    let (status, answer) = s
        .app
        .send("PATCH", &format!("/v1/people/{person_id}"), &civil_defense.secret, Some(json!({ "data": { "child_status": "separated" }, "client_edited_at": store::now().to_string() })))
        .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let alerts = all_docs(&s.app.state.db, "alerts").await;
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0]["detail"]["child_status"], "separated");
}

// ---- degraded mode (spec 5.5, 5.7) ----

#[tokio::test]
async fn in_degraded_mode_unico_routes_answer_503_and_path_c_keeps_working() {
    let s = scene().await;
    let journeys = s.app.unico.post_count();
    degraded::enter_limit(&s.app.state.db).await.unwrap();

    // Captures and photos need Unico.
    let (status, body) = capture(
        &s.app,
        &s.volunteer,
        json!({ "purpose": "checkin", "shelter_id": s.shelter }),
    )
    .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("unico_degraded")),
        "{body}"
    );
    let photo = json!(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        b"a face"
    ));
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", s.shelter),
        Some(checkin_body(json!({ "photo_base64": photo }))),
    )
    .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("unico_degraded")),
        "{body}"
    );
    assert_eq!(s.app.unico.post_count(), journeys, "nothing reached Unico");

    // A check-in without biometrics (path C) goes on, and so does everything that does not need Unico.
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", s.shelter),
        Some(checkin_body(json!({ "no_biometrics": true, "biometrics_declined": true, "data": { "name": "Ana" } }))),
    )
    .await;
    assert_eq!(
        (status, body["level"].as_str()),
        (StatusCode::CREATED, Some("no_biometrics")),
        "{body}"
    );
    let (status, _) = call(
        &s.app,
        &s.volunteer,
        "GET",
        &format!("/v1/shelters/{}/people", s.shelter),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, shown) = s.app.request("GET", "/v1/status", &[], None).await;
    assert_eq!(
        (status, shown["unico"].as_str(), shown["reason"].as_str()),
        (StatusCode::OK, Some("degraded"), Some("limit"))
    );
}

// ---- a capture belongs to the volunteer who asked for it ----

#[tokio::test]
async fn another_volunteer_on_the_same_phone_cannot_use_a_checkin_or_biometrics_capture() {
    let s = scene().await;
    let colleague = s
        .app
        .join(
            &s.app.invite(&s.admin, &s.shelter, "qr", "volunteer").await,
            "Colleague",
        )
        .await;
    // The colleague works on the same phone (same device id) and has a session of their own.
    let on_same_phone = Actor {
        device: s.volunteer.device.clone(),
        ..colleague
    };

    let (_, made) = capture(
        &s.app,
        &s.volunteer,
        json!({ "purpose": "checkin", "shelter_id": s.shelter }),
    )
    .await;
    let checkin_capture = made["capture_id"].as_str().unwrap().to_string();
    finish(&s.app, &checkin_capture, "tok-ana").await;
    let (status, body) = call(
        &s.app,
        &on_same_phone,
        "POST",
        &format!("/v1/shelters/{}/checkins", s.shelter),
        Some(checkin_body(json!({ "capture_id": checkin_capture }))),
    )
    .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::FORBIDDEN, Some("capture_wrong_target")),
        "{body}"
    );
    // The one who asked still can.
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", s.shelter),
        Some(checkin_body(json!({ "capture_id": checkin_capture }))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // Late biometrics of a person without biometrics: same rule.
    let (_, anon) = call(
        &s.app,
        &s.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", s.shelter),
        Some(checkin_body(
            json!({ "no_biometrics": true, "data": { "name": "Bia" } }),
        )),
    )
    .await;
    let bia = anon["person_id"].as_str().unwrap();
    let (_, made) = capture(
        &s.app,
        &s.volunteer,
        json!({ "purpose": "biometrics", "expected_person_id": bia }),
    )
    .await;
    let biometrics_capture = made["capture_id"].as_str().unwrap();
    finish(&s.app, biometrics_capture, "tok-bia").await;
    let (status, body) = call(
        &s.app,
        &on_same_phone,
        "PUT",
        &format!("/v1/people/{bia}/biometrics"),
        Some(json!({ "capture_id": biometrics_capture })),
    )
    .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::FORBIDDEN, Some("capture_wrong_target")),
        "{body}"
    );
    let (status, _) = call(
        &s.app,
        &s.volunteer,
        "PUT",
        &format!("/v1/people/{bia}/biometrics"),
        Some(json!({ "capture_id": biometrics_capture })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// ---- spec 10.1.1: the legal basis of looking at a thumbnail ----

#[tokio::test]
async fn looking_at_a_thumbnail_is_audited_with_the_vital_interest_basis() {
    let s = scene().await;
    let jpeg = [0xFFu8, 0xD8, 0xFF, 0xE0, 1, 2];
    // Registered with consent: the basis of the registration is not the one of the look.
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", s.shelter),
        Some(checkin_body(json!({
            "no_biometrics": true,
            "legal_basis": "consent",
            "thumbnail_base64": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, jpeg),
        }))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let path = format!(
        "/v1/people/{}/thumbnail",
        body["person_id"].as_str().unwrap()
    );
    assert_eq!(
        call(&s.app, &s.volunteer, "GET", &path, None).await.0,
        StatusCode::OK
    );
    let looks: Vec<Value> = all_docs(&s.app.state.db, "audit_log")
        .await
        .into_iter()
        .filter(|e| e["action"] == "thumbnail.view")
        .collect();
    assert_eq!(looks.len(), 1);
    assert_eq!(looks[0]["legal_basis"], "vital_interest");
    assert_eq!(looks[0]["shelter_id"], json!(s.shelter));
}

// ---- spec 4.11: protection and the Civil Defense see a child while an alert about them is open ----

#[tokio::test]
async fn protection_and_the_civil_defense_reach_a_child_only_while_an_alert_about_them_is_open() {
    let s = scene().await;
    let civil_defense = s.app.actor("DC").await;
    s.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
    let protection = s.app.actor("Prot").await;
    s.app.grant(&protection, GlobalRole::Protection).await;
    let mediator = s.app.actor("Med").await;
    s.app.grant(&mediator, GlobalRole::Mediator).await;
    let register = |status: &'static str| {
        let (app, who, shelter) = (s.app.clone(), s.volunteer.clone(), s.shelter.clone());
        async move {
            let (code, body) = call(
                &app,
                &who,
                "POST",
                &format!("/v1/shelters/{shelter}/checkins"),
                Some(checkin_body(json!({
                    "no_biometrics": true,
                    "legal_basis": "child_protection",
                    "data": { "name": "Crianca", "is_child": true, "child_status": status },
                }))),
            )
            .await;
            assert_eq!(code, StatusCode::CREATED, "{body}");
            body["person_id"].as_str().unwrap().to_string()
        }
    };
    let edit = |status: &str| json!({ "data": { "child_status": status }, "client_edited_at": store::now().to_string() });

    // With a guardian there is no alert, so nobody outside the shelter sees the child.
    let calm = register("with_guardian").await;
    for who in [&civil_defense, &protection] {
        assert_eq!(
            call(&s.app, who, "GET", &format!("/v1/people/{calm}"), None)
                .await
                .0,
            StatusCode::NOT_FOUND,
            "{}",
            who.id
        );
        assert_eq!(
            call(
                &s.app,
                who,
                "PATCH",
                &format!("/v1/people/{calm}"),
                Some(edit("separated"))
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }

    // Alone: the alert is open, and the two read the child and change the status; nothing else.
    let alone = register("unaccompanied").await;
    let path = format!("/v1/people/{alone}");
    for who in [&civil_defense, &protection] {
        let (status, body) = call(&s.app, who, "GET", &path, None).await;
        assert_eq!(
            (status, body["person"]["child_status"].as_str()),
            (StatusCode::OK, Some("unaccompanied")),
            "{}",
            who.id
        );
    }
    // A mediator, a validator and a stranger have no such access.
    let validator = s.app.actor("Val").await;
    s.app.grant(&validator, GlobalRole::Validator).await;
    for who in [&mediator, &validator] {
        assert_eq!(
            call(&s.app, who, "GET", &path, None).await.0,
            StatusCode::NOT_FOUND,
            "{}",
            who.id
        );
    }
    let rename =
        json!({ "data": { "name": "Outro nome" }, "client_edited_at": store::now().to_string() });
    assert_eq!(
        call(&s.app, &protection, "PATCH", &path, Some(rename))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, body) = call(
        &s.app,
        &civil_defense,
        "PATCH",
        &path,
        Some(edit("with_guardian")),
    )
    .await;
    assert_eq!(
        (status, body["person"]["child_status"].as_str()),
        (StatusCode::OK, Some("with_guardian")),
        "{body}"
    );

    // Once the alert is resolved the access ends.
    let alerts = all_docs(&s.app.state.db, "alerts").await;
    let open = alerts
        .iter()
        .find(|a| a["target_id"] == json!(alone))
        .unwrap();
    let resolve = format!(
        "/v1/alerts/{}/resolve",
        open["_firestore_id"].as_str().unwrap()
    );
    assert_eq!(
        s.app
            .send("POST", &resolve, &protection.secret, None)
            .await
            .0,
        StatusCode::OK
    );
    for who in [&civil_defense, &protection] {
        assert_eq!(
            call(&s.app, who, "GET", &path, None).await.0,
            StatusCode::NOT_FOUND,
            "{}",
            who.id
        );
    }
}

// ---- phase 4: rescue, search, family link and matches together ----

use base64::{Engine, engine::general_purpose::STANDARD};

const JPEG: [u8; 6] = [0xFF, 0xD8, 0xFF, 0xE0, 1, 2];
const FAMILY_PHONE: &str = "+55 51 99999-1234";

fn photo() -> Value {
    json!(STANDARD.encode(b"a face"))
}

/// The next photos sent to Unico come back as these tokens, in order.
fn tokens(app: &TestApp, tokens: &[&str]) {
    app.unico.with(|s| {
        for token in tokens {
            s.post_replies.push_back((
                200,
                json!({ "id": "img", "state": "PROCESS_STATE_FINISHED", "data": { "humanToken": { "value": token } } }),
            ));
        }
    });
}

fn search_body(photos: usize) -> Value {
    json!({
        "photos_base64": (0..photos).map(|_| photo()).collect::<Vec<_>>(),
        "missing_person": { "name": "Joao", "nickname": "Jo", "notes": "camisa azul" },
        "family": { "name": "Maria", "phone": FAMILY_PHONE, "relationship": "mae", "language": "pt-BR" },
    })
}

/// A search typed by `who`, whose photo is `token`.
async fn register_search(app: &TestApp, who: &Actor, token: &str) -> (StatusCode, Value) {
    tokens(app, &[token]);
    let mut body = search_body(1);
    body["id"] = json!(new_id());
    call(app, who, "POST", "/v1/searches", Some(body)).await
}

/// A link made by `who` for `shelter`, and the family's submission of a photo that is `token`.
async fn family_search(
    app: &TestApp,
    who: &Actor,
    shelter: &str,
    token: &str,
) -> (StatusCode, Value) {
    let (status, link) = call(
        app,
        who,
        "POST",
        "/v1/search-links",
        Some(json!({ "shelter_id": shelter })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{link}");
    tokens(app, &[token]);
    let (status, _, answer) = app
        .request(
            "POST",
            &format!("/v1/search-links/{}/submit", link["code"].as_str().unwrap()),
            &[("x-device-id", "dev-family")],
            Some(search_body(1)),
        )
        .await;
    (status, answer)
}

/// A check-in by photo of whoever `token` is.
async fn checkin_photo(
    app: &TestApp,
    who: &Actor,
    shelter: &str,
    token: &str,
    extra: Value,
) -> (StatusCode, Value) {
    tokens(app, &[token]);
    let mut body = checkin_body(extra);
    body["photo_base64"] = photo();
    call(
        app,
        who,
        "POST",
        &format!("/v1/shelters/{shelter}/checkins"),
        Some(body),
    )
    .await
}

async fn matches_of(app: &TestApp, who: &Actor) -> Vec<Value> {
    let (status, body) = call(app, who, "GET", "/v1/matches", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["matches"].as_array().unwrap().clone()
}

async fn match_docs(app: &TestApp) -> usize {
    all_docs(&app.state.db, "matches").await.len()
}

async fn actor_with(app: &TestApp, name: &str, role: GlobalRole) -> Actor {
    let actor = app.actor(name).await;
    app.grant(&actor, role).await;
    actor
}

#[tokio::test]
async fn a_family_link_search_meets_a_later_check_in_and_the_mediator_gets_the_contact() {
    let s = scene().await;
    let mediator = actor_with(&s.app, "Mediator", GlobalRole::Mediator).await;
    let protection = actor_with(&s.app, "Protection", GlobalRole::Protection).await;

    // The family sends the search; the volunteer who made the link is told, and only that.
    let (status, answer) = family_search(&s.app, &s.admin, &s.shelter, "tok-adult").await;
    assert_eq!(
        (status, &answer),
        (
            StatusCode::ACCEPTED,
            &json!({ "message": "search_registered", "usable_photos": 1 })
        )
    );
    assert_eq!(match_docs(&s.app).await, 0);
    let alerts = all_docs(&s.app.state.db, "alerts").await;
    let received: Vec<_> = alerts
        .iter()
        .filter(|a| a["kind"] == "family_search_received")
        .collect();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["detail"], json!({ "usable_photos": 1 }));
    assert_eq!(received[0]["volunteer_ids"], json!([s.admin.id]));

    // The person checks in with the same face: a match in the queue of the confirmed shelter.
    let (status, body) =
        checkin_photo(&s.app, &s.volunteer, &s.shelter, "tok-adult", json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let queue = matches_of(&s.app, &s.admin).await;
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0]["mediator_kind"], "shelter");
    assert_eq!(matches_of(&s.app, &mediator).await.len(), 1);
    assert!(matches_of(&s.app, &protection).await.is_empty());
    // Criterion 51: checking in again does not make another one.
    let (status, _) = checkin_photo(&s.app, &s.volunteer, &s.shelter, "tok-adult", json!({})).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(match_docs(&s.app).await, 1);

    // A child goes to protection, whatever the shelter, and the admins never see it.
    let (status, _) = family_search(&s.app, &s.admin, &s.shelter, "tok-child").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let child = json!({ "legal_basis": "child_protection", "data": { "is_child": true, "child_status": "unaccompanied" } });
    let (status, body) = checkin_photo(&s.app, &s.volunteer, &s.shelter, "tok-child", child).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(matches_of(&s.app, &s.admin).await.len(), 1);
    let kids = matches_of(&s.app, &protection).await;
    assert_eq!(
        (
            kids.len(),
            kids[0]["mediator_kind"].as_str(),
            kids[0]["is_child"].as_bool()
        ),
        (1, Some("protection"), Some(true))
    );

    // The mediator checks the data, which has no contact, and accepts: only now the contact comes.
    let id = queue[0]["id"].as_str().unwrap();
    let (status, detail) = call(&s.app, &mediator, "GET", &format!("/v1/matches/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert!(!detail.to_string().contains("99999"), "{detail}");
    let decision = json!({ "decision": "accept", "decided_by": "person" });
    let (status, accepted) = call(
        &s.app,
        &mediator,
        "POST",
        &format!("/v1/matches/{id}/decision"),
        Some(decision.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["family"]["phone"], FAMILY_PHONE);
    assert!(
        accepted["whatsapp_link"]
            .as_str()
            .unwrap()
            .starts_with("https://wa.me/5551999991234?text=")
    );
    let (status, again) = call(
        &s.app,
        &s.admin,
        "POST",
        &format!("/v1/matches/{id}/decision"),
        Some(decision),
    )
    .await;
    assert_eq!(
        (status, error_of(&again)),
        (StatusCode::CONFLICT, Some("match_already_decided"))
    );
}

#[tokio::test]
async fn a_search_meets_a_later_rescue_of_the_same_person_in_the_central_queue() {
    let s = scene().await;
    let mediator = actor_with(&s.app, "Mediator", GlobalRole::Mediator).await;
    let (status, _) = register_search(&s.app, &s.admin, "tok-rescued").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    tokens(&s.app, &["tok-rescued"]);
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "POST",
        "/v1/rescues",
        Some(json!({
            "id": new_id(), "photo_base64": photo(), "lat": ORIGIN.0, "lon": ORIGIN.1,
            "accuracy": 12.0, "captured_at": store::now().to_string(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // A person with no shelter is for the central queue: the mediator and the Civil Defense.
    let queue = matches_of(&s.app, &mediator).await;
    assert_eq!(
        (queue.len(), queue[0]["mediator_kind"].as_str()),
        (1, Some("central"))
    );
    assert!(matches_of(&s.app, &s.admin).await.is_empty());
}

#[tokio::test]
async fn late_biometrics_triggers_the_match() {
    let s = scene().await;
    let (status, _) = register_search(&s.app, &s.admin, "tok-late").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    // Registered without biometrics: nothing to compare yet.
    let person = new_id();
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", s.shelter),
        Some(checkin_body(json!({ "id": person, "no_biometrics": true }))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(match_docs(&s.app).await, 0);
    // The photo comes later and carries the person to the token of the search.
    tokens(&s.app, &["tok-late"]);
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "PUT",
        &format!("/v1/people/{person}/biometrics"),
        Some(json!({ "photo_base64": photo() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let queue = matches_of(&s.app, &s.admin).await;
    assert_eq!(
        (queue.len(), queue[0]["mediator_kind"].as_str()),
        (1, Some("shelter"))
    );
}

#[tokio::test]
async fn the_answer_to_a_search_is_the_same_with_and_without_a_match() {
    let s = scene().await;
    let (status, _) = checkin_photo(&s.app, &s.volunteer, &s.shelter, "tok-here", json!({})).await;
    assert_eq!(status, StatusCode::CREATED);
    let (with_status, with_match) = register_search(&s.app, &s.admin, "tok-here").await;
    assert_eq!(match_docs(&s.app).await, 1);
    let (without_status, without_match) = register_search(&s.app, &s.admin, "tok-nobody").await;
    assert_eq!(match_docs(&s.app).await, 1);
    assert_eq!((with_status, &with_match), (without_status, &without_match));
    assert_eq!(
        with_match,
        json!({ "message": "search_registered", "usable_photos": 1 })
    );
}

#[tokio::test]
async fn a_rescued_person_with_no_shelter_is_seen_by_who_registered_the_rescue_and_by_the_civil_defense()
 {
    let s = scene().await;
    let dc = actor_with(&s.app, "DC", GlobalRole::CivilDefense).await;
    let mediator = actor_with(&s.app, "Mediator", GlobalRole::Mediator).await;
    // No name, no wristband: a thumbnail is kept.
    let id = new_id();
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "POST",
        "/v1/rescues",
        Some(json!({
            "id": id, "lat": ORIGIN.0, "lon": ORIGIN.1, "accuracy": 12.0,
            "captured_at": store::now().to_string(), "thumbnail_base64": STANDARD.encode(JPEG),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let path = format!("/v1/people/{id}/thumbnail");
    for (who, expected) in [
        (&s.volunteer, StatusCode::OK),
        (&dc, StatusCode::OK),
        (&s.admin, StatusCode::FORBIDDEN),
        (&mediator, StatusCode::FORBIDDEN),
    ] {
        let (status, _) = call(&s.app, who, "GET", &path, None).await;
        assert_eq!(status, expected, "{}", who.id);
    }
    // The volunteer of the landing point opens the record; someone else does not find it.
    let record = format!("/v1/people/{id}");
    assert_eq!(
        call(&s.app, &s.volunteer, "GET", &record, None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&s.app, &s.admin, "GET", &record, None).await.0,
        StatusCode::NOT_FOUND
    );
    // And the late photo works without a shelter.
    tokens(&s.app, &["tok-rescued-late"]);
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "PUT",
        &format!("{record}/biometrics"),
        Some(json!({ "photo_base64": photo() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["level"], "assisted");
}

#[tokio::test]
async fn a_shelter_of_a_closed_event_takes_no_write_of_phase_4_or_of_its_members() {
    let s = scene().await;
    let shelter: crate::shelters::ShelterDoc =
        store::get(&s.app.state.db, crate::shelters::COLLECTION, &s.shelter)
            .await
            .unwrap()
            .unwrap();
    let event_id = shelter.event_id.unwrap();
    let mut event: crate::events::EventDoc =
        store::get(&s.app.state.db, crate::events::COLLECTION, &event_id)
            .await
            .unwrap()
            .unwrap();
    event.status = crate::events::EventStatus::Closed;
    store::replace(
        &s.app.state.db,
        crate::events::COLLECTION,
        &event_id,
        &event,
    )
    .await
    .unwrap();

    let invite = format!("/v1/shelters/{}/invites", s.shelter);
    let writes = [
        (
            "POST",
            invite.clone(),
            Some(json!({ "kind": "qr", "role": "volunteer" })),
        ),
        (
            "PATCH",
            format!("/v1/shelters/{}", s.shelter),
            Some(json!({ "name": "Outro" })),
        ),
        (
            "POST",
            "/v1/search-links".to_string(),
            Some(json!({ "shelter_id": s.shelter })),
        ),
        (
            "POST",
            "/v1/searches".to_string(),
            Some({
                let mut b = search_body(1);
                b["id"] = json!(new_id());
                b
            }),
        ),
        (
            "PATCH",
            format!("/v1/shelters/{}/members/{}", s.shelter, s.volunteer.id),
            Some(json!({ "role": "admin" })),
        ),
        (
            "DELETE",
            format!("/v1/shelters/{}/members/{}", s.shelter, s.volunteer.id),
            None,
        ),
        (
            "POST",
            format!("/v1/shelters/{}/endorsements/in-person/code", s.shelter),
            Some(json!({ "lat": ORIGIN.0, "lon": ORIGIN.1, "accuracy": 10.0 })),
        ),
    ];
    for (method, path, body) in writes {
        let (status, answer) = call(&s.app, &s.admin, method, &path, body).await;
        assert_eq!(
            (status, error_of(&answer)),
            (StatusCode::CONFLICT, Some("event_closed")),
            "{method} {path}"
        );
    }
    // Reading and leaving are still allowed.
    assert_eq!(
        call(&s.app, &s.admin, "GET", &invite, None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &s.app,
            &s.volunteer,
            "POST",
            &format!("/v1/shelters/{}/leave", s.shelter),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
}
