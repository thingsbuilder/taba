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

    // Only protection or the Civil Defense change the status (spec 3.4.1.4).
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
