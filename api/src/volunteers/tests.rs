use super::domain::{MAX_PHONE_CHARS, ProfilePatch, valid_language, valid_name, valid_phone};
use super::*;
use crate::members::{Change, change_role};
use crate::test_support::{TestApp, audit_actions};
use axum::http::StatusCode;
use serde_json::{Value, json};

// ---- the document (no database) ----

fn doc() -> VolunteerDoc {
    VolunteerDoc::from_profile(&ProfileInput {
        name: "Ana".into(),
        phone: Some("5551999990000".into()),
        show_phone: true,
        language: "pt-BR".into(),
    })
    .unwrap()
}

fn patch_of(body: Value) -> ProfilePatch {
    serde_json::from_value(body).unwrap()
}

/// A repeated item does not repeat its effect: granting a role twice leaves one grant, and
/// revoking twice (or something never granted) changes nothing.
#[test]
fn a_role_is_granted_and_revoked_once_whatever_the_repetition() {
    let at = crate::store::now();
    let mut volunteer = doc();
    assert!(volunteer.set_role(GlobalRole::Mediator, true, "validator", at));
    assert!(!volunteer.set_role(GlobalRole::Mediator, true, "someone else", at));
    assert_eq!(volunteer.global_roles, [GlobalRole::Mediator]);
    assert_eq!(volunteer.role_grants.len(), 1);
    assert_eq!(volunteer.role_grants[0].by, "validator");

    assert!(volunteer.set_role(GlobalRole::Protection, true, "validator", at));
    assert!(volunteer.set_role(GlobalRole::Mediator, false, "validator", at));
    assert!(!volunteer.set_role(GlobalRole::Mediator, false, "validator", at));
    assert_eq!(volunteer.global_roles, [GlobalRole::Protection]);
    assert_eq!(volunteer.role_grants.len(), 1);
    assert!(!doc().set_role(GlobalRole::Validator, false, "validator", at));
}

#[test]
fn a_patch_changes_only_the_fields_it_names_and_null_clears_the_phone() {
    let mut volunteer = doc();
    patch_of(json!({ "name": "  Bia " }))
        .apply(&mut volunteer)
        .unwrap();
    assert_eq!(volunteer.name, "Bia");
    assert_eq!(volunteer.phone.as_deref(), Some("5551999990000"));
    assert!(volunteer.show_phone);

    patch_of(json!({ "phone": null, "show_phone": false, "language": "en" }))
        .apply(&mut volunteer)
        .unwrap();
    assert_eq!(volunteer.phone, None);
    assert!(!volunteer.show_phone);
    assert_eq!(volunteer.language, "en");
    assert_eq!(volunteer.name, "Bia");
}

#[test]
fn an_invalid_patch_is_refused_naming_the_field() {
    for (body, field) in [
        (json!({ "name": " " }), "name"),
        (json!({ "language": "fr" }), "language"),
        (json!({ "phone": "1".repeat(MAX_PHONE_CHARS + 1) }), "phone"),
    ] {
        let error = patch_of(body).apply(&mut doc()).unwrap_err();
        assert_eq!(error.detail["field"], field);
    }
}

async fn patch(app: &TestApp, secret: &str, body: Value) -> (StatusCode, Value) {
    let auth = format!("Bearer {secret}");
    let (status, _, json) = app
        .request(
            "PATCH",
            "/v1/me",
            &[("authorization", auth.as_str())],
            Some(body),
        )
        .await;
    (status, json)
}

#[test]
fn field_validation() {
    assert_eq!(valid_name("  Ana  ").unwrap(), "Ana");
    assert!(valid_name("").is_err() && valid_name("   ").is_err());
    assert!(valid_name(&"x".repeat(100)).is_ok() && valid_name(&"x".repeat(101)).is_err());
    assert_eq!(
        valid_phone(" +55 11 99999-0000 ").unwrap().as_deref(),
        Some("+55 11 99999-0000")
    );
    assert_eq!(valid_phone("  ").unwrap(), None);
    assert!(valid_phone(&"1".repeat(31)).is_err());
    assert!(valid_language("pt-BR").is_ok() && valid_language("en").is_ok());
    assert!(valid_language("es").is_err() && valid_language("").is_err());
}

#[tokio::test]
async fn me_returns_the_profile_roles_and_memberships() {
    let app = TestApp::new().await;
    let (secret, id) = app.login("dev", "tok", "Ana").await;
    let (status, body) = app.get_authed("/v1/me", &secret).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({
            "volunteer": { "id": id, "name": "Ana", "phone": null, "show_phone": false, "language": "pt-BR" },
            "global_roles": [],
            "memberships": [],
        })
    );
}

#[tokio::test]
async fn patch_changes_only_the_fields_sent() {
    let app = TestApp::new().await;
    let (secret, _) = app.login("dev", "tok", "Ana").await;
    let (status, body) = patch(
        &app,
        &secret,
        json!({ "name": "Ana Souza", "phone": "123", "show_phone": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["volunteer"]["name"], "Ana Souza");
    let (_, me) = app.get_authed("/v1/me", &secret).await;
    assert_eq!(
        (
            &me["volunteer"]["name"],
            &me["volunteer"]["phone"],
            &me["volunteer"]["show_phone"],
            &me["volunteer"]["language"]
        ),
        (
            &json!("Ana Souza"),
            &json!("123"),
            &json!(true),
            &json!("pt-BR")
        )
    );

    // An absent phone keeps it; an explicit null clears it.
    patch(&app, &secret, json!({ "language": "en" })).await;
    let (_, me) = app.get_authed("/v1/me", &secret).await;
    assert_eq!(
        (&me["volunteer"]["phone"], &me["volunteer"]["language"]),
        (&json!("123"), &json!("en"))
    );
    patch(&app, &secret, json!({ "phone": null })).await;
    let (_, me) = app.get_authed("/v1/me", &secret).await;
    assert_eq!(me["volunteer"]["phone"], Value::Null);
}

#[tokio::test]
async fn patch_rejects_invalid_values_and_writes_nothing() {
    let app = TestApp::new().await;
    let (secret, _) = app.login("dev", "tok", "Ana").await;
    let before = audit_actions(&app.state.db).await.len();
    for body in [
        json!({ "name": "" }),
        json!({ "language": "fr" }),
        json!({ "phone": "1".repeat(31) }),
    ] {
        let (status, json) = patch(&app, &secret, body.clone()).await;
        assert_eq!(
            (status, json["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_profile")),
            "{body}"
        );
    }
    assert_eq!(
        app.get_authed("/v1/me", &secret).await.1["volunteer"]["name"],
        "Ana"
    );
    assert_eq!(audit_actions(&app.state.db).await.len(), before);
}

#[tokio::test]
async fn patch_needs_a_session_and_is_audited() {
    let app = TestApp::new().await;
    let (status, _, _) = app
        .request("PATCH", "/v1/me", &[], Some(json!({ "name": "X" })))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (secret, _) = app.login("dev", "tok", "Ana").await;
    patch(&app, &secret, json!({ "name": "Bia" })).await;
    assert!(
        audit_actions(&app.state.db)
            .await
            .contains(&"volunteer.update".to_string())
    );
}

#[tokio::test]
async fn patch_keeps_roles_granted_meanwhile() {
    let app = TestApp::new().await;
    let (secret, id) = app.login("dev", "tok", "Ana").await;
    change_role(&app.state.db, &id, GlobalRole::Validator, Change::Grant)
        .await
        .unwrap();
    patch(&app, &secret, json!({ "name": "Bia" })).await;
    let (_, me) = app.get_authed("/v1/me", &secret).await;
    assert_eq!(
        (&me["global_roles"], &me["volunteer"]["name"]),
        (&json!(["validator"]), &json!("Bia"))
    );
}
