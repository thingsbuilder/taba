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

// ---- event activity (spec 4.3.3: any write of a shelter) and the idempotency of a decision ----

/// How many seconds ago the event of the shelter last had a write, after making it look 31 days idle.
async fn make_idle(app: &TestApp, shelter: &str) -> String {
    let shelter: crate::shelters::ShelterDoc =
        store::get(&app.state.db, crate::shelters::COLLECTION, shelter)
            .await
            .unwrap()
            .unwrap();
    let event_id = shelter.event_id.unwrap();
    let mut event: crate::events::EventDoc =
        store::get(&app.state.db, crate::events::COLLECTION, &event_id)
            .await
            .unwrap()
            .unwrap();
    event.last_activity_at = crate::store::now() - std::time::Duration::from_secs(31 * 86_400);
    store::replace(&app.state.db, crate::events::COLLECTION, &event_id, &event)
        .await
        .unwrap();
    // The write of the hour was just made; forget it so the next one is written.
    app.state.event_activity.lock().unwrap().clear();
    event_id
}

async fn last_activity_age_secs(app: &TestApp, event_id: &str) -> i64 {
    let event: crate::events::EventDoc =
        store::get(&app.state.db, crate::events::COLLECTION, event_id)
            .await
            .unwrap()
            .unwrap();
    (crate::store::now().as_microsecond() - event.last_activity_at.as_microsecond()) / 1_000_000
}

#[tokio::test]
async fn any_write_of_a_shelter_postpones_the_closing_of_its_event() {
    let s = scene().await;
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
    let household = new_id();
    let (status, body) = call(&s.app, &s.volunteer, "POST", &format!("/v1/shelters/{}/households", s.shelter), Some(json!({ "id": household, "member_ids": [person], "head_id": person, "client_created_at": store::now().to_string() }))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let writes: Vec<(&str, String, Value)> = vec![
        (
            "issue a card",
            format!("/v1/households/{household}/cards"),
            json!({ "code": "ABCD2345", "client_issued_at": store::now().to_string() }),
        ),
        (
            "change the household",
            format!("/v1/households/{household}"),
            json!({ "pets": [{ "species": "dog", "count": 1 }] }),
        ),
        (
            "change the person",
            format!("/v1/people/{person}"),
            json!({ "data": { "name": "Ana" }, "client_edited_at": store::now().to_string() }),
        ),
        ("register a search", "/v1/searches".to_string(), {
            let mut b = search_body(1);
            b["id"] = json!(new_id());
            b
        }),
    ];
    for (what, path, body) in writes {
        let event_id = make_idle(&s.app, &s.shelter).await;
        if what == "register a search" {
            tokens(&s.app, &["tok-activity"]);
        }
        let method = if what.starts_with("change") {
            "PATCH"
        } else {
            "POST"
        };
        let (status, answer) = call(&s.app, &s.admin, method, &path, Some(body)).await;
        assert!(status.is_success(), "{what}: {status} {answer}");
        assert!(
            last_activity_age_secs(&s.app, &event_id).await < 60,
            "{what} did not record activity"
        );
        crate::jobs::run_once(&s.app.state).await.unwrap();
        let event: crate::events::EventDoc =
            store::get(&s.app.state.db, crate::events::COLLECTION, &event_id)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(event.status, crate::events::EventStatus::Active, "{what}");
    }
    // Without a write the same idle event does close.
    let event_id = make_idle(&s.app, &s.shelter).await;
    crate::jobs::run_once(&s.app.state).await.unwrap();
    let event: crate::events::EventDoc =
        store::get(&s.app.state.db, crate::events::COLLECTION, &event_id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(event.status, crate::events::EventStatus::Closed);
}

#[tokio::test]
async fn the_family_contact_of_an_accept_is_not_kept_for_the_idempotency_replay() {
    let s = scene().await;
    let mediator = actor_with(&s.app, "Mediator", GlobalRole::Mediator).await;
    let (status, _) = register_search(&s.app, &s.admin, "tok-keyed").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, _) = checkin_photo(&s.app, &s.volunteer, &s.shelter, "tok-keyed", json!({})).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = matches_of(&s.app, &mediator).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let key = new_id();
    let path = format!("/v1/matches/{id}/decision");
    let body = json!({ "decision": "accept", "decided_by": "person" });
    let (status, accepted) = s
        .app
        .send_keyed("POST", &path, &mediator.secret, &key, Some(body.clone()))
        .await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["family"]["phone"], FAMILY_PHONE);

    // The retry with the same key does not get the contact again, and nothing keeps it.
    let (status, replay) = s
        .app
        .send_keyed("POST", &path, &mediator.secret, &key, Some(body))
        .await;
    assert_eq!(
        (status, error_of(&replay)),
        (StatusCode::CONFLICT, Some("match_already_decided"))
    );
    let kept = all_docs(&s.app.state.db, "idempotency").await;
    assert!(
        !kept
            .iter()
            .any(|d| d.to_string().contains("99999") || d.to_string().contains("wa.me")),
        "{kept:?}"
    );
}

#[tokio::test]
async fn the_volunteer_of_a_rescue_loses_sight_of_the_person_once_a_shelter_has_them() {
    let s = scene().await;
    let rescuer = s.app.actor("Rescuer").await;
    s.app
        .create_shelter(&rescuer, "Outro abrigo", north_of_origin(300_000.0))
        .await;
    tokens(&s.app, &["tok-moved"]);
    let (status, body) = call(
        &s.app,
        &rescuer,
        "POST",
        "/v1/rescues",
        Some(json!({
            "id": new_id(), "photo_base64": photo(), "lat": ORIGIN.0, "lon": ORIGIN.1, "accuracy": 12.0,
            "captured_at": store::now().to_string(), "thumbnail_base64": STANDARD.encode(JPEG),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let person = body["person_id"].as_str().unwrap().to_string();
    let record = format!("/v1/people/{person}");
    // Before any shelter: the record and the thumbnail are the rescuer's.
    assert_eq!(
        call(&s.app, &rescuer, "GET", &record, None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &s.app,
            &rescuer,
            "GET",
            &format!("{record}/thumbnail"),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    // A shelter checks the person in: from then on the shelter rules apply, for both.
    let (status, body) =
        checkin_photo(&s.app, &s.volunteer, &s.shelter, "tok-moved", json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        call(&s.app, &rescuer, "GET", &record, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &s.app,
            &rescuer,
            "GET",
            &format!("{record}/thumbnail"),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&s.app, &s.volunteer, "GET", &record, None).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn the_family_link_of_a_closed_event_answers_like_any_dead_link() {
    let s = scene().await;
    let (status, link) = call(
        &s.app,
        &s.admin,
        "POST",
        "/v1/search-links",
        Some(json!({ "shelter_id": s.shelter })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{link}");
    let event_id = make_idle(&s.app, &s.shelter).await;
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

    let (status, _, answer) = s
        .app
        .request(
            "POST",
            &format!("/v1/search-links/{}/submit", link["code"].as_str().unwrap()),
            &[("x-device-id", "dev-family")],
            Some(search_body(1)),
        )
        .await;
    assert_eq!(
        (status, error_of(&answer)),
        (StatusCode::GONE, Some("search_link_invalid"))
    );
}

/// The family opens the link and sends the search; `reply` is what Unico answers to the photo.
async fn submit_to(app: &TestApp, code: &str, unico: (u16, Value)) -> (StatusCode, Value) {
    app.unico.with(|s| s.post_replies.push_back(unico));
    let (status, _, answer) = app
        .request(
            "POST",
            &format!("/v1/search-links/{code}/submit"),
            &[("x-device-id", "dev-family")],
            Some(search_body(1)),
        )
        .await;
    (status, answer)
}

#[tokio::test]
async fn a_link_whose_photos_all_fail_is_not_used_and_makes_no_search_until_a_photo_serves() {
    let s = scene().await;
    let (status, link) = call(
        &s.app,
        &s.admin,
        "POST",
        "/v1/search-links",
        Some(json!({ "shelter_id": s.shelter })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{link}");
    let code = link["code"].as_str().unwrap();

    // No face: the same answer as always, with zero usable photos, and the link stays valid.
    let no_face = (400, json!({ "code": 20532, "message": "No face detected" }));
    for attempt in 1..=2u64 {
        let (status, answer) = submit_to(&s.app, code, no_face.clone()).await;
        assert_eq!(
            (status, answer),
            (
                StatusCode::ACCEPTED,
                json!({ "message": "search_registered", "usable_photos": 0 })
            )
        );
        let doc = all_docs(&s.app.state.db, "search_links")
            .await
            .pop()
            .unwrap();
        assert_eq!(
            (doc["status"].as_str(), doc["attempts"].as_u64()),
            (Some("active"), Some(attempt))
        );
        assert!(all_docs(&s.app.state.db, "searches").await.is_empty());
        assert!(
            all_docs(&s.app.state.db, "alerts")
                .await
                .iter()
                .all(|a| a["kind"] != "family_search_received")
        );
        let (status, _, valid) = s
            .app
            .request("GET", &format!("/v1/search-links/{code}"), &[], None)
            .await;
        assert_eq!(
            (status, valid["valid"].as_bool()),
            (StatusCode::OK, Some(true))
        );
    }

    // Another photo serves: now the search exists, the link is used and the volunteer is told.
    let (status, answer) = submit_to(&s.app, code, (200, json!({ "id": "img", "state": "PROCESS_STATE_FINISHED", "data": { "humanToken": { "value": "tok-second-try" } } }))).await;
    assert_eq!(
        (status, answer),
        (
            StatusCode::ACCEPTED,
            json!({ "message": "search_registered", "usable_photos": 1 })
        )
    );
    let doc = all_docs(&s.app.state.db, "search_links")
        .await
        .pop()
        .unwrap();
    assert_eq!(doc["status"], "used");
    assert_eq!(all_docs(&s.app.state.db, "searches").await.len(), 1);
    assert_eq!(
        all_docs(&s.app.state.db, "alerts")
            .await
            .iter()
            .filter(|a| a["kind"] == "family_search_received")
            .count(),
        1
    );
}

#[tokio::test]
async fn a_link_made_by_the_civil_defense_for_a_chosen_shelter_gives_the_search_that_shelter_and_its_event()
 {
    let s = scene().await;
    let dc = actor_with(&s.app, "DC", GlobalRole::CivilDefense).await;
    let (status, link) = call(
        &s.app,
        &dc,
        "POST",
        "/v1/search-links",
        Some(json!({ "shelter_id": s.shelter })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{link}");
    let (status, _) = submit_to(&s.app, link["code"].as_str().unwrap(), (200, json!({ "id": "img", "state": "PROCESS_STATE_FINISHED", "data": { "humanToken": { "value": "tok-chosen" } } }))).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let shelter: crate::shelters::ShelterDoc =
        store::get(&s.app.state.db, crate::shelters::COLLECTION, &s.shelter)
            .await
            .unwrap()
            .unwrap();
    let searches = all_docs(&s.app.state.db, "searches").await;
    assert_eq!(searches.len(), 1);
    assert_eq!(searches[0]["created_by"], json!(dc.id));
    assert_eq!(searches[0]["via"], "family_link");
    assert_eq!(searches[0]["origin_shelter_id"], json!(s.shelter));
    assert_eq!(searches[0]["event_id"], json!(shelter.event_id));
}

// ---- phase 5: donations together (criteria 25 to 33) ----

const CARD: &str = "ABCD2345";

async fn deliver(
    app: &TestApp,
    who: &Actor,
    shelter: &str,
    person: &str,
    item: &str,
    extra: Value,
) -> (StatusCode, Value) {
    let mut body = json!({
        "id": new_id(), "card_code": CARD, "person_id": person, "item": item, "quantity": 1,
        "delivered_at": store::now().to_string(),
    });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    call(
        app,
        who,
        "POST",
        &format!("/v1/shelters/{shelter}/deliveries"),
        Some(body),
    )
    .await
}

/// The marks of a delivery the app queued offline: it says so and carries a time from minutes ago.
fn queued() -> Value {
    json!({ "offline": true, "delivered_at": (store::now() - std::time::Duration::from_secs(600)).to_string() })
}

async fn delivery_doc(app: &TestApp, id: &str) -> crate::deliveries::DeliveryDoc {
    store::get(&app.state.db, "deliveries", id)
        .await
        .unwrap()
        .unwrap()
}

fn spec(scope: &str, once: bool, high: bool) -> Value {
    json!({ "scope": scope, "period_days": 7, "once_per_event": once, "high_value": high })
}

/// A confirmed shelter with its items, two people (one with biometrics, one without) in a
/// household with a card, no draw (the rate is zero), and a second, unconfirmed shelter.
struct Donations {
    s: Scene,
    p1: String,
    p2: String,
    household: String,
    other: Actor,
    other_shelter: String,
}

async fn donations() -> Donations {
    let mut s = scene().await;
    std::sync::Arc::make_mut(&mut s.app.state.config).delivery_sample_rate = 0.0;
    let items = json!({ "items": {
        "cesta": spec("per_household", false, false),
        "kit": spec("per_person", false, false),
        "colchao": spec("per_person", true, true),
        "vale": spec("per_person", false, true),
        "agua": spec("per_household", false, false),
    } });
    let (status, body) = call(
        &s.app,
        &s.admin,
        "PUT",
        &format!("/v1/shelters/{}/items", s.shelter),
        Some(items.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The saved items come back in the view of the shelter, which is where the delivery screen reads them.
    let (status, view) = call(
        &s.app,
        &s.admin,
        "GET",
        &format!("/v1/shelters/{}", s.shelter),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["shelter"]["items"]["cesta"]["scope"], "per_household");
    assert_eq!(view["shelter"]["items"]["colchao"]["high_value"], true);

    let (status, body) = checkin_photo(
        &s.app,
        &s.volunteer,
        &s.shelter,
        "tok-p1",
        json!({ "data": { "name": "Ana" } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let p1 = s.app.state.person_id_for_token("tok-p1");
    let p2 = new_id();
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", s.shelter),
        Some(checkin_body(
            json!({ "id": p2, "no_biometrics": true, "data": { "name": "Bruno" } }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let household = new_id();
    let (status, body) = call(&s.app, &s.volunteer, "POST", &format!("/v1/shelters/{}/households", s.shelter), Some(json!({ "id": household, "member_ids": [p1, p2], "head_id": p1, "client_created_at": store::now().to_string() }))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "POST",
        &format!("/v1/households/{household}/cards"),
        Some(json!({ "code": CARD, "client_issued_at": store::now().to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let other = s.app.actor("Other").await;
    let other_shelter = s
        .app
        .create_shelter(&other, "Galpão", north_of_origin(5_000.0))
        .await;
    let (status, body) = call(
        &s.app,
        &other,
        "PUT",
        &format!("/v1/shelters/{other_shelter}/items"),
        Some(items),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    Donations {
        s,
        p1,
        p2,
        household,
        other,
        other_shelter,
    }
}

#[tokio::test]
async fn a_delivery_by_card_is_registered_without_asking_unico_and_the_second_one_to_the_household_goes_to_review()
 {
    let d = donations().await;
    let (app, shelter, volunteer) = (&d.s.app, &d.s.shelter, &d.s.volunteer);
    let asked = app.unico.post_count();

    // Criteria 25, 27, 28: assisted and without biometrics both receive an ordinary item by the card.
    let (status, first) = deliver(app, volunteer, shelter, &d.p1, "cesta", json!({})).await;
    assert_eq!(
        (status, first["status"].as_str()),
        (StatusCode::CREATED, Some("ok")),
        "{first}"
    );
    let doc = delivery_doc(app, first["delivery_id"].as_str().unwrap()).await;
    assert_eq!(
        (doc.verification, doc.household_id.as_deref()),
        (
            crate::deliveries::Verification::Card,
            Some(d.household.as_str())
        )
    );
    let (status, kit) = deliver(app, volunteer, shelter, &d.p2, "kit", json!({})).await;
    assert_eq!(
        (status, kit["status"].as_str()),
        (StatusCode::CREATED, Some("ok")),
        "{kit}"
    );
    assert_eq!(
        app.unico.post_count(),
        asked,
        "an ordinary delivery never asks Unico"
    );

    // Criterion 31: per_household, a second member in the period goes to review; per_person does not.
    let (status, second) = deliver(app, volunteer, shelter, &d.p2, "cesta", json!({})).await;
    assert_eq!(
        (status, second["status"].as_str()),
        (StatusCode::CREATED, Some("in_review")),
        "{second}"
    );
    assert_eq!(second["reasons"][0]["kind"], "duplicate_in_period");
    assert_eq!(
        second["reasons"][0]["previous_delivery_id"],
        first["delivery_id"]
    );
    let (_, kit_p1) = deliver(app, volunteer, shelter, &d.p1, "kit", json!({})).await;
    assert_eq!(kit_p1["status"], "ok");
    let (_, kit_again) = deliver(app, volunteer, shelter, &d.p1, "kit", json!({})).await;
    assert_eq!(kit_again["status"], "in_review");

    // The admin lists what waits, never sees a person id, and decides.
    let list = format!("/v1/shelters/{shelter}/deliveries?status=in_review");
    assert_eq!(
        call(app, volunteer, "GET", &list, None).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, listed) = call(app, &d.s.admin, "GET", &list, None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["deliveries"].as_array().unwrap().len(), 2);
    assert!(
        !listed.to_string().contains(&d.p1) && !listed.to_string().contains(&d.p2),
        "{listed}"
    );
    let id = second["delivery_id"].as_str().unwrap();
    let (status, body) = call(
        app,
        &d.s.admin,
        "POST",
        &format!("/v1/deliveries/{id}/review"),
        Some(json!({ "decision": "reviewed_ok", "note": "familia grande" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let reviewed = delivery_doc(app, id).await;
    assert_eq!(
        (reviewed.status, reviewed.reviewed_by.as_deref()),
        (
            crate::deliveries::DeliveryStatus::ReviewedOk,
            Some(d.s.admin.id.as_str())
        )
    );
    let (_, listed) = call(app, &d.s.admin, "GET", &list, None).await;
    assert_eq!(listed["deliveries"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_high_value_item_asks_for_a_selfie_and_a_wrong_face_records_nothing() {
    let d = donations().await;
    let (app, shelter, volunteer) = (&d.s.app, &d.s.shelter, &d.s.volunteer);
    // Criterion 29: no capture, 428; for a person without biometrics only an admin can settle it.
    let (status, asked) = deliver(app, volunteer, shelter, &d.p1, "colchao", json!({})).await;
    assert_eq!(
        (status, error_of(&asked)),
        (StatusCode::PRECONDITION_REQUIRED, Some("liveness_required")),
        "{asked}"
    );
    assert_eq!(
        asked["detail"],
        json!({ "reason": "high_value", "admin_approval_only": false })
    );
    let (_, asked) = deliver(app, volunteer, shelter, &d.p2, "colchao", json!({})).await;
    assert_eq!(
        asked["detail"],
        json!({ "reason": "high_value", "admin_approval_only": true })
    );

    // The right face: ok with verification liveness.
    let revalidation = |person: &str| json!({ "purpose": "revalidation", "shelter_id": shelter, "expected_person_id": person });
    let (status, made) = capture(app, volunteer, revalidation(&d.p1)).await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    finish(app, made["capture_id"].as_str().unwrap(), "tok-p1").await;
    let (status, done) = deliver(
        app,
        volunteer,
        shelter,
        &d.p1,
        "colchao",
        json!({ "capture_id": made["capture_id"] }),
    )
    .await;
    assert_eq!(
        (status, done["status"].as_str()),
        (StatusCode::CREATED, Some("ok")),
        "{done}"
    );
    assert_eq!(
        delivery_doc(app, done["delivery_id"].as_str().unwrap())
            .await
            .verification,
        crate::deliveries::Verification::Liveness
    );

    // Another face: 409 and no delivery written.
    let before = all_docs(&app.state.db, "deliveries").await.len();
    let (_, made) = capture(app, volunteer, revalidation(&d.p1)).await;
    finish(
        app,
        made["capture_id"].as_str().unwrap(),
        "tok-someone-else",
    )
    .await;
    let (status, wrong) = deliver(
        app,
        volunteer,
        shelter,
        &d.p1,
        "vale",
        json!({ "capture_id": made["capture_id"] }),
    )
    .await;
    assert_eq!(
        (status, error_of(&wrong)),
        (StatusCode::CONFLICT, Some("revalidation_failed")),
        "{wrong}"
    );
    assert_eq!(all_docs(&app.state.db, "deliveries").await.len(), before);
}

#[tokio::test]
async fn the_admin_vouches_for_a_person_without_a_selfie_and_a_volunteer_cannot() {
    let d = donations().await;
    let (app, shelter) = (&d.s.app, &d.s.shelter);
    let (status, refused) = deliver(
        app,
        &d.s.volunteer,
        shelter,
        &d.p2,
        "colchao",
        json!({ "admin_approval": true }),
    )
    .await;
    assert_eq!(
        (status, error_of(&refused)),
        (StatusCode::FORBIDDEN, Some("admin_approval_required")),
        "{refused}"
    );
    let (status, ok) = deliver(
        app,
        &d.s.admin,
        shelter,
        &d.p2,
        "colchao",
        json!({ "admin_approval": true }),
    )
    .await;
    assert_eq!(
        (status, ok["status"].as_str()),
        (StatusCode::CREATED, Some("ok")),
        "{ok}"
    );
    let doc = delivery_doc(app, ok["delivery_id"].as_str().unwrap()).await;
    assert_eq!(
        (doc.verification, doc.approved_by.as_deref()),
        (
            crate::deliveries::Verification::AdminApproval,
            Some(d.s.admin.id.as_str())
        )
    );
    assert!(
        crate::test_support::audit_actions(&app.state.db)
            .await
            .contains(&"delivery.admin_approval".to_string())
    );
}

#[tokio::test]
async fn offline_a_high_value_item_goes_to_review_unless_an_admin_approves() {
    let d = donations().await;
    let (app, shelter) = (&d.s.app, &d.s.shelter);
    let asked = app.unico.post_count();
    // Criteria 4 and 33.
    let (status, review) = deliver(app, &d.s.volunteer, shelter, &d.p1, "vale", queued()).await;
    assert_eq!(
        (status, review["status"].as_str()),
        (StatusCode::CREATED, Some("in_review")),
        "{review}"
    );
    assert_eq!(review["reasons"][0]["kind"], "liveness_unavailable");
    let (status, approved) = deliver(app, &d.s.admin, shelter, &d.p2, "vale", {
        let mut approval = queued();
        approval["admin_approval"] = json!(true);
        approval
    })
    .await;
    assert_eq!(
        (status, approved["status"].as_str()),
        (StatusCode::CREATED, Some("ok")),
        "{approved}"
    );
    assert_eq!(app.unico.post_count(), asked);
}

#[tokio::test]
async fn the_late_biometrics_repoints_the_deliveries_and_an_old_id_still_finds_the_person() {
    let d = donations().await;
    let (app, shelter, volunteer) = (&d.s.app, &d.s.shelter, &d.s.volunteer);
    let (_, before) = deliver(app, volunteer, shelter, &d.p2, "kit", json!({})).await;
    let first = before["delivery_id"].as_str().unwrap().to_string();

    tokens(app, &["tok-p2-late"]);
    let (status, body) = call(
        app,
        volunteer,
        "PUT",
        &format!("/v1/people/{}/biometrics", d.p2),
        Some(json!({ "photo_base64": photo() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let new = app.state.person_id_for_token("tok-p2-late");
    assert_eq!(
        delivery_doc(app, &first).await.person_id.as_deref(),
        Some(new.as_str())
    );

    // An offline delivery still carrying the old id reaches the server now: it is the same person.
    let (status, late) = deliver(app, volunteer, shelter, &d.p2, "kit", queued()).await;
    assert_eq!(status, StatusCode::CREATED, "{late}");
    assert_eq!(late["status"], "in_review"); // the kit of this person was already given
    assert_eq!(
        delivery_doc(app, late["delivery_id"].as_str().unwrap())
            .await
            .person_id
            .as_deref(),
        Some(new.as_str())
    );
}

#[tokio::test]
async fn what_a_delivery_tells_of_the_other_one_depends_on_who_asks() {
    let d = donations().await;
    let (app, confirmed, volunteer) = (&d.s.app, &d.s.shelter, &d.s.volunteer);
    let told = |answer: &Value| {
        (
            answer["reasons"][0].get("previous_delivered_at").is_some(),
            answer["reasons"][0].get("previous_shelter_name").cloned(),
        )
    };

    // An unconfirmed requester, the other delivery at a confirmed shelter: nothing, not even the date.
    let (_, first) = deliver(app, volunteer, confirmed, &d.p1, "agua", json!({})).await;
    assert_eq!(first["status"], "ok", "{first}");
    let (_, a) = deliver(app, &d.other, &d.other_shelter, &d.p2, "agua", json!({})).await;
    assert_eq!(a["status"], "in_review", "{a}");
    assert_eq!(told(&a), (false, None), "{a}");
    // ... also when the same delivery is sent again, and in the review list of that shelter.
    let again = call(app, &d.other, "POST", &format!("/v1/shelters/{}/deliveries", d.other_shelter), Some(json!({
        "id": a["delivery_id"], "card_code": CARD, "person_id": d.p2, "item": "agua", "quantity": 1, "delivered_at": store::now().to_string(),
    }))).await;
    assert_eq!(told(&again.1), (false, None), "{}", again.1);
    let (status, list) = call(
        app,
        &d.other,
        "GET",
        &format!(
            "/v1/shelters/{}/deliveries?status=in_review",
            d.other_shelter
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert!(
        !list.to_string().contains("previous_delivered_at")
            && !list.to_string().contains("\"Abrigo\""),
        "{list}"
    );

    // An unconfirmed requester, the other delivery its own: date and name.
    let (_, b) = deliver(app, &d.other, &d.other_shelter, &d.p1, "agua", json!({})).await;
    assert_eq!(b["status"], "in_review");
    assert_eq!(told(&b), (true, Some(json!("Galpão"))), "{b}");

    // A confirmed requester, the other delivery at an unconfirmed shelter: the date, not the name.
    let (_, c) = deliver(app, volunteer, confirmed, &d.p2, "agua", json!({})).await;
    assert_eq!(c["status"], "in_review");
    assert_eq!(told(&c), (true, None), "{c}");

    // A confirmed requester, the other delivery at another confirmed shelter: both.
    let admin2 = app.actor("Admin2").await;
    let second = app
        .confirmed_shelter(&admin2, "Segundo", north_of_origin(10_000.0))
        .await;
    let (status, body) = call(
        app,
        &admin2,
        "PUT",
        &format!("/v1/shelters/{second}/items"),
        Some(json!({ "items": { "cesta": spec("per_household", false, false) } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, e) = deliver(app, &admin2, &second, &d.p1, "cesta", json!({})).await;
    assert_eq!(e["status"], "ok", "{e}");
    let (_, f) = deliver(app, volunteer, confirmed, &d.p2, "cesta", json!({})).await;
    assert_eq!(f["status"], "in_review", "{f}");
    assert_eq!(told(&f), (true, Some(json!("Segundo"))), "{f}");
}

#[tokio::test]
async fn the_server_believes_offline_only_for_a_delivery_that_waited_and_the_clock_of_the_phone_has_limits()
 {
    let d = donations().await;
    let (app, shelter, volunteer) = (&d.s.app, &d.s.shelter, &d.s.volunteer);
    let ago = |secs: u64| (store::now() - std::time::Duration::from_secs(secs)).to_string();

    // "offline" with a delivered_at of now is not believed: a high value item still asks for the selfie.
    let (status, asked) = deliver(
        app,
        volunteer,
        shelter,
        &d.p1,
        "vale",
        json!({ "offline": true, "delivered_at": ago(5) }),
    )
    .await;
    assert_eq!(
        (status, error_of(&asked)),
        (StatusCode::PRECONDITION_REQUIRED, Some("liveness_required")),
        "{asked}"
    );
    // After a real wait it is: review for lack of a selfie, marked in the delivery and in the audit.
    let (status, queued) = deliver(
        app,
        volunteer,
        shelter,
        &d.p1,
        "vale",
        json!({ "offline": true, "delivered_at": ago(120) }),
    )
    .await;
    assert_eq!(
        (status, queued["status"].as_str()),
        (StatusCode::CREATED, Some("in_review")),
        "{queued}"
    );
    assert!(
        delivery_doc(app, queued["delivery_id"].as_str().unwrap())
            .await
            .offline_claimed
    );
    assert!(
        crate::test_support::audit_actions(&app.state.db)
            .await
            .contains(&"delivery.create_offline".to_string())
    );
    // An online delivery is not marked.
    let (_, online) = deliver(app, volunteer, shelter, &d.p1, "kit", json!({})).await;
    assert!(
        !delivery_doc(app, online["delivery_id"].as_str().unwrap())
            .await
            .offline_claimed
    );

    // delivered_at: from 30 days back; a time ahead is cut (see the next test).
    let days = |n: u64| (store::now() - std::time::Duration::from_secs(n * 86_400)).to_string();
    for (at, ok) in [(days(29), true), (days(31), false)] {
        let (status, body) = deliver(
            app,
            volunteer,
            shelter,
            &d.p2,
            "kit",
            json!({ "delivered_at": at }),
        )
        .await;
        if ok {
            assert_eq!(status, StatusCode::CREATED, "{at}: {body}");
        } else {
            assert_eq!(
                (status, error_of(&body), body["detail"]["field"].as_str()),
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Some("invalid_request"),
                    Some("delivered_at")
                ),
                "{at}: {body}"
            );
        }
    }
}

#[tokio::test]
async fn a_phone_with_the_clock_ahead_still_delivers_and_the_time_is_cut_to_the_receipt() {
    let d = donations().await;
    let (app, shelter, volunteer) = (&d.s.app, &d.s.shelter, &d.s.volunteer);
    let ahead = |secs: u64| (store::now() + std::time::Duration::from_secs(secs)).to_string();
    let behind = |secs: u64| (store::now() - std::time::Duration::from_secs(secs)).to_string();

    // 10 minutes and 2 hours ahead: 201, delivered_at is the receipt, and it is marked in the delivery and the audit.
    for (item, secs) in [("kit", 600), ("cesta", 7_200)] {
        let (status, body) = deliver(
            app,
            volunteer,
            shelter,
            &d.p1,
            item,
            json!({ "delivered_at": ahead(secs) }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{secs}: {body}");
        let doc = delivery_doc(app, body["delivery_id"].as_str().unwrap()).await;
        assert_eq!(doc.delivered_at, doc.received_at, "{secs}");
        assert!(doc.delivered_at_clamped, "{secs}");
    }
    assert!(
        crate::test_support::audit_actions(&app.state.db)
            .await
            .contains(&"delivery.create_clamped".to_string())
    );
    // A time that is not ahead is left as it is and not marked.
    let (_, normal) = deliver(
        app,
        volunteer,
        shelter,
        &d.p2,
        "agua",
        json!({ "delivered_at": behind(60) }),
    )
    .await;
    let doc = delivery_doc(app, normal["delivery_id"].as_str().unwrap()).await;
    assert!(!doc.delivered_at_clamped && doc.delivered_at < doc.received_at);

    // The same delivery sent again answers the same.
    let id = new_id();
    let send = |at: String| {
        deliver(
            app,
            volunteer,
            shelter,
            &d.p2,
            "colchao",
            json!({ "id": id, "delivered_at": at, "admin_approval": true }),
        )
    };
    let _ = send(ahead(3_600)).await; // a volunteer's approval is refused for a high value item: 403
    let (status, first) = deliver(
        app,
        &d.s.admin,
        shelter,
        &d.p2,
        "colchao",
        json!({ "id": id, "delivered_at": ahead(3_600), "admin_approval": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, again) = deliver(
        app,
        &d.s.admin,
        shelter,
        &d.p2,
        "colchao",
        json!({ "id": id, "delivered_at": ahead(3_600), "admin_approval": true }),
    )
    .await;
    assert_eq!((status, &again), (StatusCode::CREATED, &first));

    // The duplicate decision uses the cut time: one 6 days 23 hours ago, and one 2 hours ahead that
    // would be 7 days 1 hour apart (no duplicate) if it were not cut.
    let (_, old) = deliver(
        app,
        volunteer,
        shelter,
        &d.p1,
        "vale",
        json!({ "delivered_at": behind(7 * 86_400 - 3_600), "offline": true }),
    )
    .await;
    assert_eq!(old["status"], "in_review", "{old}"); // offline, high value, no admin
    let (status, dup) = deliver(
        app,
        &d.s.admin,
        shelter,
        &d.p1,
        "vale",
        json!({ "delivered_at": ahead(7_200), "admin_approval": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{dup}");
    assert_eq!(dup["status"], "in_review", "{dup}");
    assert_eq!(dup["reasons"][0]["kind"], "duplicate_in_period");
}

#[tokio::test]
async fn an_admin_approval_where_no_selfie_would_be_asked_is_an_ordinary_delivery() {
    let d = donations().await;
    let (app, shelter) = (&d.s.app, &d.s.shelter);
    // The flag on an ordinary item, by an admin (person 1) and by a volunteer (person 2): card,
    // nobody approved, no 403.
    for (who, person) in [(&d.s.admin, &d.p1), (&d.s.volunteer, &d.p2)] {
        let (status, body) = deliver(
            app,
            who,
            shelter,
            person,
            "kit",
            json!({ "admin_approval": true }),
        )
        .await;
        assert_eq!(
            (status, body["status"].as_str()),
            (StatusCode::CREATED, Some("ok")),
            "{body}"
        );
        let doc = delivery_doc(app, body["delivery_id"].as_str().unwrap()).await;
        assert_eq!(
            (doc.verification, doc.approved_by),
            (crate::deliveries::Verification::Card, None)
        );
    }
    assert!(
        !crate::test_support::audit_actions(&app.state.db)
            .await
            .iter()
            .any(|a| a.starts_with("delivery.admin_approval"))
    );
}

// ---- phase 6: the dashboard, the exports, the anomalies and the discard together ----

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// A request with the actor's session whose answer is text (a CSV), not JSON.
async fn text(app: &TestApp, who: &Actor, path: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method("GET")
        .uri(path)
        .header("authorization", format!("Bearer {}", who.secret))
        .header("x-device-id", who.device.as_str())
        .body(Body::empty())
        .unwrap();
    let response = crate::router(app.state.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn doc_count(app: &TestApp, collection: &str) -> usize {
    all_docs(&app.state.db, collection).await.len()
}

fn current_year() -> i64 {
    i64::from(
        store::now()
            .to_zoned(firestore::jiff::tz::TimeZone::UTC)
            .year(),
    )
}

#[tokio::test]
async fn anomalies_reach_the_validator_and_the_civil_defense_and_never_the_admin() {
    let mut s = scene().await;
    std::sync::Arc::make_mut(&mut s.app.state.config).anomaly_checkins_per_10min = 2;
    let validator = actor_with(&s.app, "Validator", GlobalRole::Validator).await;
    let dc = actor_with(&s.app, "DC", GlobalRole::CivilDefense).await;
    for n in 0..3 {
        let (status, body) = call(&s.app, &s.volunteer, "POST", &format!("/v1/shelters/{}/checkins", s.shelter), Some(checkin_body(json!({ "id": new_id(), "no_biometrics": true, "data": { "name": format!("P{n}") } })))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let kinds = |body: &Value| -> Vec<String> {
        body["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["kind"].as_str().unwrap().to_string())
            .collect()
    };
    for who in [&validator, &dc] {
        let (status, body) = call(&s.app, who, "GET", "/v1/alerts", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            kinds(&body).contains(&"anomaly_volume".to_string()),
            "{body}"
        );
    }
    // The admin of the shelter, who is also who the volunteer works for, never sees it in the box of "Avisos".
    for who in [&s.admin, &s.volunteer] {
        let (_, body) = call(&s.app, who, "GET", "/v1/alerts", None).await;
        assert!(
            !kinds(&body)
                .iter()
                .any(|k| k.starts_with("anomaly_") || k == "out_of_area"),
            "{body}"
        );
    }
}

#[tokio::test]
async fn the_dashboard_counts_the_event_and_the_csv_has_a_line_per_shelter_and_no_person() {
    let mut s = scene().await;
    // No delivery is drawn for the Unico: 5% of the runs would fail on `liveness_required`.
    std::sync::Arc::make_mut(&mut s.app.state.config).delivery_sample_rate = 0.0;
    let validator = actor_with(&s.app, "Validator", GlobalRole::Validator).await;
    let dc = actor_with(&s.app, "DC", GlobalRole::CivilDefense).await;
    let other = s.app.actor("Other").await;
    let other_shelter = s
        .app
        .create_shelter(&other, "Galpão", north_of_origin(5_000.0))
        .await;
    let shelter: crate::shelters::ShelterDoc =
        store::get(&s.app.state.db, crate::shelters::COLLECTION, &s.shelter)
            .await
            .unwrap()
            .unwrap();
    let event = shelter.event_id.clone().unwrap();
    let year = current_year();

    // An assisted pregnant adult, a child without biometrics (a household with two dogs), a verified
    // old person with a disability, an exit, and one person at the unconfirmed shelter.
    let (status, ana) = checkin_photo(&s.app, &s.volunteer, &s.shelter, "tok-dash-ana", json!({ "data": { "name": "Ana Souza", "birth_year": year - 30, "vulnerabilities": { "pregnant": true, "lactating": false, "disability": [] } } })).await;
    assert_eq!(status, StatusCode::CREATED, "{ana}");
    let ana_id = s.app.state.person_id_for_token("tok-dash-ana");
    let bruno = new_id();
    let (status, body) = call(&s.app, &s.volunteer, "POST", &format!("/v1/shelters/{}/checkins", s.shelter), Some(checkin_body(json!({ "id": bruno, "no_biometrics": true, "data": { "name": "Bruno Lima", "birth_year": year - 5 } })))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut old = crate::exits::fixtures::person(crate::people::Level::Verified, Some(&s.shelter));
    old.name = Some("Carla Dias".into());
    old.birth_year = Some((year - 70) as u16);
    old.event_ids = vec![event.clone()];
    old.vulnerabilities.disability = vec![crate::people::Disability::Mobility];
    let carla = new_id();
    crate::exits::fixtures::put(&s.app, &carla, &old).await;
    let leaving = new_id();
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", s.shelter),
        Some(checkin_body(
            json!({ "id": leaving, "no_biometrics": true, "data": { "name": "Davi Rocha" } }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "POST",
        &format!("/v1/shelters/{}/exits", s.shelter),
        Some(json!({ "person_id": leaving, "exit_destination": "own_home" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &s.app,
        &other,
        "POST",
        &format!("/v1/shelters/{other_shelter}/checkins"),
        Some(checkin_body(
            json!({ "id": new_id(), "no_biometrics": true, "data": { "name": "Eva Costa" } }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let household = new_id();
    let (status, body) = call(&s.app, &s.volunteer, "POST", &format!("/v1/shelters/{}/households", s.shelter), Some(json!({ "id": household, "member_ids": [ana_id, bruno], "head_id": ana_id, "pets": [{ "species": "dog", "count": 2 }], "client_created_at": store::now().to_string() }))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // A delivery, a rescue, and a search that meets the rescued person: a match for the central queue.
    let (status, body) = call(
        &s.app,
        &s.volunteer,
        "POST",
        &format!("/v1/households/{household}/cards"),
        Some(json!({ "code": CARD, "client_issued_at": store::now().to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = call(
        &s.app,
        &s.admin,
        "PUT",
        &format!("/v1/shelters/{}/items", s.shelter),
        Some(json!({ "items": { "kit": spec("per_person", false, false) } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, delivered) =
        deliver(&s.app, &s.volunteer, &s.shelter, &ana_id, "kit", json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{delivered}");
    let (status, _) = register_search(&s.app, &s.admin, "tok-dash-rescued").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    tokens(&s.app, &["tok-dash-rescued"]);
    let (status, body) = call(&s.app, &s.volunteer, "POST", "/v1/rescues", Some(json!({ "id": new_id(), "photo_base64": photo(), "lat": ORIGIN.0, "lon": ORIGIN.1, "accuracy": 12.0, "captured_at": store::now().to_string() }))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // Who reads it: a validator and the Civil Defense; nobody else.
    let paths = [
        "/v1/dashboard/events".to_string(),
        format!("/v1/dashboard/events/{event}"),
        format!("/v1/dashboard/events/{event}/daily.csv"),
    ];
    for path in &paths {
        for who in [&validator, &dc] {
            assert_eq!(text(&s.app, who, path).await.0, StatusCode::OK, "{path}");
        }
        for who in [&s.admin, &s.volunteer, &other] {
            assert_eq!(
                text(&s.app, who, path).await.0,
                StatusCode::FORBIDDEN,
                "{path}"
            );
        }
    }

    // The numbers (criteria 52 and 53).
    let (_, list) = call(&s.app, &validator, "GET", "/v1/dashboard/events", None).await;
    let listed = list["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == json!(event))
        .unwrap();
    assert_eq!(
        listed["unconfirmed_shelters"][0]["id"],
        json!(other_shelter)
    );
    assert_eq!(
        listed["unconfirmed_shelters"][0]["created_by_name"],
        "Other"
    );
    let (status, dash) = call(
        &s.app,
        &dc,
        "GET",
        &format!("/v1/dashboard/events/{event}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{dash}");
    assert_eq!(dash["notice"], crate::dashboard::NOTICE);
    let t = &dash["totals"];
    assert_eq!(t["unique_people"], 4, "{dash}"); // Ana, Bruno, Carla and Eva; Davi left
    assert_eq!(
        t["by_level"],
        json!({ "verified": 1, "assisted": 1, "no_biometrics": 2 })
    );
    assert_eq!(
        (
            t["age_bands"]["18_59"].as_u64(),
            t["age_bands"]["0_11"].as_u64(),
            t["age_bands"]["60_plus"].as_u64()
        ),
        (Some(1), Some(1), Some(1))
    );
    assert_eq!(
        (
            t["pregnant"].as_u64(),
            t["disability"]["mobility"].as_u64(),
            t["households"].as_u64(),
            t["pets"].as_u64()
        ),
        (Some(1), Some(1), Some(1), Some(2))
    );
    assert_eq!(dash["exits_by_destination"]["own_home"], 1);
    assert_eq!(dash["rescues"], 1);
    assert_eq!(dash["open_matches"]["central"], 1, "{dash}");
    let rows = dash["shelters"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let row = |id: &str| rows.iter().find(|r| r["id"] == json!(id)).unwrap();
    assert_eq!(
        (
            row(&s.shelter)["confirmation"].as_str(),
            row(&s.shelter)["occupancy"].as_u64()
        ),
        (Some("confirmed"), Some(3))
    );
    assert_eq!(
        (
            row(&other_shelter)["confirmation"].as_str(),
            row(&other_shelter)["occupancy"].as_u64()
        ),
        (Some("unconfirmed"), Some(1))
    );

    // The daily CSV: a line per shelter, no name and no person id.
    let (_, csv) = text(&s.app, &validator, &paths[2]).await;
    assert_eq!(csv.lines().count(), 3, "{csv}");
    for hidden in [
        "Ana",
        "Bruno",
        "Carla",
        "Eva",
        "Davi",
        ana_id.as_str(),
        bruno.as_str(),
        carla.as_str(),
        leaving.as_str(),
    ] {
        assert!(!csv.contains(hidden), "{hidden} in {csv}");
    }
    assert!(csv.contains("Galpão"), "the shelter name is there: {csv}");

    // The export of a shelter (criterion 16): the admin of the confirmed one gets it, with no
    // person id nor thumbnail; the unconfirmed shelter gets 403.
    let (status, export) = text(
        &s.app,
        &s.admin,
        &format!("/v1/shelters/{}/export.csv", s.shelter),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{export}");
    for hidden in [ana_id.as_str(), bruno.as_str(), carla.as_str(), "thumbnail"] {
        assert!(!export.contains(hidden), "{hidden} in {export}");
    }
    assert!(export.contains("Ana Souza"));
    assert_eq!(
        text(
            &s.app,
            &s.volunteer,
            &format!("/v1/shelters/{}/export.csv", s.shelter)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        text(
            &s.app,
            &other,
            &format!("/v1/shelters/{other_shelter}/export.csv")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn the_discard_deletes_what_the_event_gathered_only_with_the_key_on_and_pseudonymizes_the_deliveries()
 {
    let mut d = donations().await;
    let (shelter, volunteer) = (d.s.shelter.clone(), d.s.volunteer.clone());
    let (status, delivered) =
        deliver(&d.s.app, &volunteer, &shelter, &d.p1, "cesta", json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{delivered}");
    let (status, _) = register_search(&d.s.app, &d.s.admin, "tok-discard").await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // An event closed 40 days ago, whose date of discard has passed.
    let stored: crate::shelters::ShelterDoc =
        store::get(&d.s.app.state.db, crate::shelters::COLLECTION, &shelter)
            .await
            .unwrap()
            .unwrap();
    let event_id = stored.event_id.unwrap();
    let mut event: crate::events::EventDoc =
        store::get(&d.s.app.state.db, crate::events::COLLECTION, &event_id)
            .await
            .unwrap()
            .unwrap();
    let ago = |days: u64| store::now() - std::time::Duration::from_secs(days * 86_400);
    event.status = crate::events::EventStatus::Closed;
    event.closed_at = Some(ago(40));
    event.purge_at = Some(ago(10));
    event.delivery_purge_at = Some(store::now() + std::time::Duration::from_secs(50 * 86_400));
    store::replace(
        &d.s.app.state.db,
        crate::events::COLLECTION,
        &event_id,
        &event,
    )
    .await
    .unwrap();
    let (people, households, searches, deliveries) = (
        doc_count(&d.s.app, "people").await,
        doc_count(&d.s.app, "households").await,
        doc_count(&d.s.app, "searches").await,
        doc_count(&d.s.app, "deliveries").await,
    );
    assert!(people > 0 && households > 0 && searches > 0 && deliveries > 0);

    // The key is off by default: the pass only counts, nothing is deleted.
    assert!(!d.s.app.state.config.purge_enabled);
    crate::jobs::run_once(&d.s.app.state).await.unwrap();
    assert_eq!(
        (
            doc_count(&d.s.app, "people").await,
            doc_count(&d.s.app, "households").await,
            doc_count(&d.s.app, "searches").await,
            doc_count(&d.s.app, "deliveries").await
        ),
        (people, households, searches, deliveries)
    );
    for doc in all_docs(&d.s.app.state.db, "deliveries").await {
        assert!(
            doc.get("person_id").is_some() && doc["pseudonymized"] != json!(true),
            "{doc}"
        );
    }

    // With the key on: people, households and searches go; the deliveries stay, pseudonymized; the event stays with its stats.
    std::sync::Arc::make_mut(&mut d.s.app.state.config).purge_enabled = true;
    crate::jobs::run_once(&d.s.app.state).await.unwrap();
    assert_eq!(
        (
            doc_count(&d.s.app, "people").await,
            doc_count(&d.s.app, "households").await,
            doc_count(&d.s.app, "searches").await
        ),
        (0, 0, 0)
    );
    let kept = all_docs(&d.s.app.state.db, "deliveries").await;
    assert_eq!(kept.len(), deliveries);
    for doc in &kept {
        assert!(
            doc.get("person_id").is_none()
                && doc.get("card_code").is_none()
                && doc.get("household_id").is_none(),
            "{doc}"
        );
        assert!(
            doc["household_pseudonym"].is_string() && doc["pseudonymized"] == json!(true),
            "{doc}"
        );
    }
    let purged: crate::events::EventDoc =
        store::get(&d.s.app.state.db, crate::events::COLLECTION, &event_id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(purged.status, crate::events::EventStatus::Purged);
    assert!(purged.purged_at.is_some() && purged.stats.is_some());
}

async fn alert_kinds(app: &TestApp, who: &Actor) -> Vec<String> {
    let (_, body) = call(app, who, "GET", "/v1/alerts", None).await;
    body["alerts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn a_rescue_feeds_the_anomalies_once_and_a_replay_does_not_count() {
    let mut s = scene().await;
    std::sync::Arc::make_mut(&mut s.app.state.config).anomaly_rescues_per_10min = 2;
    let validator = actor_with(&s.app, "Validator", GlobalRole::Validator).await;
    // The tracker of the anomalies is per process and keyed by volunteer: a volunteer of this test alone.
    let rescuer = s
        .app
        .join(
            &s.app.invite(&s.admin, &s.shelter, "qr", "volunteer").await,
            &format!("Rescuer-{}", new_id()),
        )
        .await;
    let (lat, lon) = north_of_origin(200_000.0);
    let rescue = |id: &str| json!({ "id": id, "photo_base64": photo(), "lat": lat, "lon": lon, "accuracy": 12.0, "captured_at": store::now().to_string() });
    let first = new_id();
    tokens(&s.app, &["tok-anom-1", "tok-anom-2", "tok-anom-3"]);
    for _ in 0..3 {
        let (status, body) = call(
            &s.app,
            &rescuer,
            "POST",
            "/v1/rescues",
            Some(rescue(&first)),
        )
        .await;
        assert!(
            status == StatusCode::CREATED || status == StatusCode::OK,
            "{body}"
        );
    }
    let (status, body) = call(
        &s.app,
        &rescuer,
        "POST",
        "/v1/rescues",
        Some(rescue(&new_id())),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // Two rescues made, three sent: the replays did not count toward the limit of two.
    let kinds = alert_kinds(&s.app, &validator).await;
    assert!(kinds.contains(&"out_of_area".to_string()), "{kinds:?}");
    assert!(!kinds.contains(&"anomaly_volume".to_string()), "{kinds:?}");
    let (status, body) = call(
        &s.app,
        &rescuer,
        "POST",
        "/v1/rescues",
        Some(rescue(&new_id())),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(
        alert_kinds(&s.app, &validator)
            .await
            .contains(&"anomaly_volume".to_string())
    );
}

const FAMILY_JPEG: [u8; 6] = [0xFF, 0xD8, 0xFF, 0xE0, 1, 2];

/// A search with a JPEG and something that is not an image, both of the person `token`.
async fn jpeg_search(app: &TestApp, who: &Actor, token: &str, id: &str) -> (StatusCode, Value) {
    tokens(app, &[token, token]);
    let mut body = search_body(2);
    body["id"] = json!(id);
    body["photos_base64"] = json!([
        STANDARD.encode(FAMILY_JPEG),
        STANDARD.encode(b"not an image")
    ]);
    call(app, who, "POST", "/v1/searches", Some(body)).await
}

/// The photos are written after the answer: waits until `n` documents are there.
async fn wait_for_photos(app: &TestApp, n: usize) -> Vec<crate::search_photos::PhotoDoc> {
    for _ in 0..200 {
        let kept = crate::search_photos::all(&app.state.db).await;
        if kept.len() == n {
            return kept;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("the photos never reached {n}");
}

#[tokio::test]
async fn the_family_photos_are_kept_only_for_a_search_with_a_match_and_erased_on_the_decision() {
    let s = scene().await;
    // A search nobody matches keeps nothing.
    let (status, _) = jpeg_search(&s.app, &s.admin, "tok-nobody", &new_id()).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(crate::search_photos::all(&s.app.state.db).await.is_empty());

    // Someone is at the shelter; a search for them has a match and keeps the image, encrypted
    // (the second "photo" is not an image and is not kept).
    let (status, body) = checkin_photo(
        &s.app,
        &s.volunteer,
        &s.shelter,
        "tok-here",
        json!({ "data": { "name": "Ana" } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, _) = jpeg_search(&s.app, &s.admin, "tok-here", &new_id()).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let kept = wait_for_photos(&s.app, 1).await;
    assert!(
        !kept[0]
            .photo_encrypted
            .as_ref()
            .unwrap()
            .windows(4)
            .any(|w| w == &FAMILY_JPEG[..4])
    );

    // The attendant of the queue sees it; a plain volunteer does not.
    let listed = matches_of(&s.app, &s.admin).await;
    let id = listed[0]["id"].as_str().unwrap().to_string();
    let (status, view) = call(&s.app, &s.admin, "GET", &format!("/v1/matches/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["search_photos"], 1);
    let path = format!("/v1/matches/{id}/search-photos/0");
    let (status, shown) = text(&s.app, &s.admin, &path).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!shown.is_empty());
    assert_eq!(
        text(&s.app, &s.volunteer, &path).await.0,
        StatusCode::FORBIDDEN
    );

    // The decision erases it.
    let (status, body) = call(
        &s.app,
        &s.admin,
        "POST",
        &format!("/v1/matches/{id}/decision"),
        Some(json!({ "decision": "decline", "decided_by": "person" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(crate::search_photos::all(&s.app.state.db).await.is_empty());
}

#[tokio::test]
async fn the_answer_of_a_search_does_not_wait_for_the_photos_to_be_written() {
    let s = scene().await;
    let (status, body) = checkin_photo(
        &s.app,
        &s.volunteer,
        &s.shelter,
        "tok-gate",
        json!({ "data": { "name": "Ana" } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = new_id();
    crate::search_photos::gate::close(&id);
    // With the writing held, the answer still comes: it does not depend on the photos.
    let answered = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        jpeg_search(&s.app, &s.admin, "tok-gate", &id),
    )
    .await
    .expect("the answer waited for the photos");
    assert_eq!(answered.0, StatusCode::ACCEPTED);
    assert!(crate::search_photos::all(&s.app.state.db).await.is_empty());
    let search: crate::searches::SearchDoc = store::get(&s.app.state.db, "searches", &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(search.retained_photos, 0);
    crate::search_photos::gate::open(&id);
    wait_for_photos(&s.app, 1).await;
}
