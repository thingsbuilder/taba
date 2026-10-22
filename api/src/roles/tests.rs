use crate::store;
use crate::test_support::{Actor, ORIGIN, TestApp, audit_actions};
use crate::volunteers::{COLLECTION as VOLUNTEERS, GlobalRole, VolunteerDoc};
use axum::http::StatusCode;
use serde_json::{Value, json};

struct World {
    app: TestApp,
    validator: Actor,
    civil_defense: Actor,
    admin: Actor,
    volunteer: Actor,
    target: Actor,
}

async fn world() -> World {
    let app = TestApp::new().await;
    let validator = app.actor("Val").await;
    app.grant(&validator, GlobalRole::Validator).await;
    let civil_defense = app.actor("DC").await;
    app.grant(&civil_defense, GlobalRole::CivilDefense).await;
    let admin = app.actor("Admin").await;
    let shelter = app.create_shelter(&admin, "A", ORIGIN).await;
    let volunteer = app
        .join(
            &app.invite(&admin, &shelter, "qr", "volunteer").await,
            "Vol",
        )
        .await;
    let target = app.actor("Target").await;
    World {
        app,
        validator,
        civil_defense,
        admin,
        volunteer,
        target,
    }
}

async fn doc(w: &World, id: &str) -> VolunteerDoc {
    store::get(&w.app.state.db, VOLUNTEERS, id)
        .await
        .unwrap()
        .unwrap()
}

async fn post(w: &World, who: &Actor, id: &str, role: &str) -> (StatusCode, Value) {
    let path = format!("/v1/volunteers/{id}/roles");
    w.app
        .send("POST", &path, &who.secret, Some(json!({ "role": role })))
        .await
}

async fn delete(w: &World, who: &Actor, id: &str, role: &str) -> (StatusCode, Value) {
    let path = format!("/v1/volunteers/{id}/roles/{role}");
    w.app.send("DELETE", &path, &who.secret, None).await
}

#[tokio::test]
async fn a_validator_or_the_civil_defense_grants_and_revokes_both_roles() {
    let w = world().await;
    for (by, role) in [(&w.validator, "mediator"), (&w.civil_defense, "protection")] {
        let (status, body) = post(&w, by, &w.target.id, role).await;
        assert_eq!((status, body), (StatusCode::OK, json!({})), "{role}");
    }
    let granted = doc(&w, &w.target.id).await;
    assert_eq!(
        granted.global_roles,
        [GlobalRole::Mediator, GlobalRole::Protection]
    );
    let by_role: Vec<(GlobalRole, &str)> = granted
        .role_grants
        .iter()
        .map(|g| (g.role, g.by.as_str()))
        .collect();
    assert_eq!(
        by_role,
        [
            (GlobalRole::Mediator, w.validator.id.as_str()),
            (GlobalRole::Protection, w.civil_defense.id.as_str())
        ]
    );
    let actions = audit_actions(&w.app.state.db).await;
    for action in ["role.grant.mediator", "role.grant.protection"] {
        assert!(actions.contains(&action.to_string()), "{action}");
    }
    // The grant shows in the target's own profile.
    let (_, me) = w.app.send("GET", "/v1/me", &w.target.secret, None).await;
    assert_eq!(me["global_roles"], json!(["mediator", "protection"]));

    // The Civil Defense revokes what the validator granted, and the other way round.
    assert_eq!(
        delete(&w, &w.civil_defense, &w.target.id, "mediator")
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        delete(&w, &w.validator, &w.target.id, "protection").await.0,
        StatusCode::NO_CONTENT
    );
    let revoked = doc(&w, &w.target.id).await;
    assert!(revoked.global_roles.is_empty() && revoked.role_grants.is_empty());
    let actions = audit_actions(&w.app.state.db).await;
    for action in ["role.revoke.mediator", "role.revoke.protection"] {
        assert!(actions.contains(&action.to_string()), "{action}");
    }
}

#[tokio::test]
async fn anyone_else_is_403_and_changes_nothing() {
    let w = world().await;
    assert_eq!(
        post(&w, &w.validator, &w.target.id, "mediator").await.0,
        StatusCode::OK
    );
    // A plain volunteer, a shelter admin and a mediator all get 403 (criterion 22).
    for who in [&w.volunteer, &w.admin, &w.target] {
        for role in ["mediator", "protection"] {
            let (status, body) = post(&w, who, &w.volunteer.id, role).await;
            assert_eq!(
                (status, body["error"].as_str()),
                (StatusCode::FORBIDDEN, Some("forbidden")),
                "{} {role}",
                who.id
            );
        }
        let (status, body) = delete(&w, who, &w.target.id, "mediator").await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::FORBIDDEN, Some("forbidden")),
            "{}",
            who.id
        );
    }
    assert!(doc(&w, &w.volunteer.id).await.global_roles.is_empty());
    assert_eq!(
        doc(&w, &w.target.id).await.global_roles,
        [GlobalRole::Mediator]
    );
    for (method, path) in [
        ("POST", "/v1/volunteers/x/roles"),
        ("DELETE", "/v1/volunteers/x/roles/mediator"),
    ] {
        let body = (method == "POST").then(|| json!({ "role": "mediator" }));
        assert_eq!(
            w.app.request(method, path, &[], body).await.0,
            StatusCode::UNAUTHORIZED,
            "{method}"
        );
    }
}

#[tokio::test]
async fn validator_and_civil_defense_cannot_be_changed_here() {
    let w = world().await;
    for role in ["validator", "civil_defense", "admin", ""] {
        for by in [&w.validator, &w.civil_defense] {
            let (status, body) = post(&w, by, &w.target.id, role).await;
            assert_eq!(
                (status, body["error"].as_str()),
                (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_role")),
                "{role}"
            );
        }
    }
    let (status, body) = delete(&w, &w.validator, &w.civil_defense.id, "civil_defense").await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_role"))
    );
    assert_eq!(
        doc(&w, &w.civil_defense.id).await.global_roles,
        [GlobalRole::CivilDefense]
    );
    assert!(doc(&w, &w.target.id).await.global_roles.is_empty());
}

#[tokio::test]
async fn repeating_a_change_or_naming_a_missing_volunteer() {
    let w = world().await;
    for _ in 0..2 {
        assert_eq!(
            post(&w, &w.validator, &w.target.id, "mediator").await.0,
            StatusCode::OK
        );
    }
    let granted = doc(&w, &w.target.id).await;
    assert_eq!(
        (granted.global_roles.len(), granted.role_grants.len()),
        (1, 1)
    );
    for _ in 0..2 {
        assert_eq!(
            delete(&w, &w.validator, &w.target.id, "mediator").await.0,
            StatusCode::NO_CONTENT
        );
    }
    let actions = audit_actions(&w.app.state.db).await;
    let count = |name: &str| actions.iter().filter(|a| *a == name).count();
    assert_eq!(
        (count("role.grant.mediator"), count("role.revoke.mediator")),
        (1, 1)
    );
    let (status, body) = post(&w, &w.validator, "nobody", "mediator").await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("volunteer_not_found"))
    );
    assert_eq!(
        delete(&w, &w.validator, "nobody", "mediator").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn concurrent_grants_of_two_roles_both_land() {
    let w = world().await;
    let (a, b) = tokio::join!(
        post(&w, &w.validator, &w.target.id, "mediator"),
        post(&w, &w.civil_defense, &w.target.id, "protection"),
    );
    assert_eq!((a.0, b.0), (StatusCode::OK, StatusCode::OK));
    let stored = doc(&w, &w.target.id).await;
    assert_eq!(stored.global_roles.len(), 2);
    assert_eq!(stored.role_grants.len(), 2);
}
