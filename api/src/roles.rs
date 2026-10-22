//! Global roles granted from the app (spec 2.1, 8.4): `mediator` and `protection`, by a
//! validator or the Civil Defense. `validator` and `civil_defense` stay with `taba role`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::store::{self, tx_replace};
use crate::volunteers::{COLLECTION as VOLUNTEERS, GlobalRole, VolunteerDoc};

#[derive(Debug, Deserialize)]
pub struct RoleRequest {
    role: String,
}

fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

/// Only the two roles the app may change; the others are the operator's.
fn grantable(role: &str) -> Result<GlobalRole, ApiError> {
    match role {
        "mediator" => Ok(GlobalRole::Mediator),
        "protection" => Ok(GlobalRole::Protection),
        _ => {
            let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_role");
            error.detail = json!({ "field": "role" });
            Err(error)
        }
    }
}

/// Grants or revokes `role` on the target in one transaction with its audit entry. Asking for
/// the state the target already is in changes and records nothing.
async fn change(
    state: &AppState,
    by: &Volunteer,
    target: &str,
    role: GlobalRole,
    grant: bool,
) -> Result<(), ApiError> {
    store::transact(&state.db, |db, tx| {
        let (target, by) = (target.to_string(), by.id.clone());
        Box::pin(async move {
            let mut doc: VolunteerDoc = store::get(&db, VOLUNTEERS, &target)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "volunteer_not_found"))?;
            if !doc.set_role(role, grant, &by, store::now()) {
                return Ok(());
            }
            tx_replace(&db, tx, VOLUNTEERS, &target, &doc)?;
            let verb = if grant { "grant" } else { "revoke" };
            audit::add(
                &db,
                tx,
                &AuditEntry::new(format!("role.{verb}.{}", role.name()), "volunteer", &target)
                    .by(&by)
                    .basis(LegalBasis::PublicPolicy),
            )
        })
    })
    .await
}

/// `POST /v1/volunteers/{id}/roles`: `{role: mediator|protection}`; records the grant in `role_grants`.
pub async fn post_role(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(target): Path<String>,
    Json(request): Json<RoleRequest>,
) -> Result<Json<Value>, ApiError> {
    if !volunteer.is_validator_or_civil_defense() {
        return Err(forbidden());
    }
    let role = grantable(&request.role)?;
    change(&state, &volunteer, &target, role, true).await?;
    Ok(Json(json!({})))
}

/// `DELETE /v1/volunteers/{id}/roles/{role}`.
pub async fn delete_role(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path((target, role)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    if !volunteer.is_validator_or_civil_defense() {
        return Err(forbidden());
    }
    let role = grantable(&role)?;
    change(&state, &volunteer, &target, role, false).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Actor, ORIGIN, TestApp, audit_actions};

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
}
