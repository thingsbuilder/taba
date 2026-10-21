//! Global roles (spec 2.1). Phase 1 only has the operator's side: `taba role grant|revoke`
//! for `validator` and `civil_defense`. `mediator` and `protection` are granted through the
//! app from phase 4.

use axum::http::StatusCode;
use firestore::errors::BackoffError;
use firestore::{FirestoreDb, FirestoreInstant};

use crate::audit::{self, AuditEntry};
use crate::error::ApiError;
use crate::store::{self, fs_error, tx_replace};
use crate::volunteers::{COLLECTION, GlobalRole, RoleGrant, VolunteerDoc};

/// Who the operator's grants are recorded as in `role_grants.by`.
const OPERATOR: &str = "cli";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Grant,
    Revoke,
}

impl GlobalRole {
    pub fn name(self) -> &'static str {
        match self {
            Self::Validator => "validator",
            Self::CivilDefense => "civil_defense",
            Self::Mediator => "mediator",
            Self::Protection => "protection",
        }
    }
}

/// The roles the operator may change from the command line.
pub fn parse_cli_role(name: &str) -> Option<GlobalRole> {
    match name {
        "validator" => Some(GlobalRole::Validator),
        "civil_defense" => Some(GlobalRole::CivilDefense),
        _ => None,
    }
}

/// Applies the change to the profile; returns whether anything changed.
pub fn apply(
    doc: &mut VolunteerDoc,
    role: GlobalRole,
    change: Change,
    now: FirestoreInstant,
) -> bool {
    let has = doc.global_roles.contains(&role);
    match (change, has) {
        (Change::Grant, false) => {
            doc.global_roles.push(role);
            doc.role_grants.push(RoleGrant {
                role,
                by: OPERATOR.into(),
                at: now,
            });
            true
        }
        (Change::Revoke, true) => {
            doc.global_roles.retain(|r| *r != role);
            doc.role_grants.retain(|g| g.role != role);
            true
        }
        _ => false,
    }
}

/// Grants or revokes a role in one transaction with its audit entry. `Ok(false)` means the
/// volunteer already was in the requested state, so nothing was written.
pub async fn change_role(
    db: &FirestoreDb,
    volunteer_id: &str,
    role: GlobalRole,
    change: Change,
) -> Result<bool, ApiError> {
    let volunteer_id = volunteer_id.to_string();
    db.run_transaction(|db, tx| {
        let volunteer_id = volunteer_id.clone();
        Box::pin(async move {
            let run = async {
                let mut doc: VolunteerDoc = store::get(&db, COLLECTION, &volunteer_id)
                    .await?
                    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "volunteer_not_found"))?;
                if !apply(&mut doc, role, change, FirestoreInstant::now()) {
                    return Ok(false);
                }
                tx_replace(&db, tx, COLLECTION, &volunteer_id, &doc)?;
                let verb = if change == Change::Grant {
                    "grant"
                } else {
                    "revoke"
                };
                let action = format!("role.{verb}.{}", role.name());
                audit::add(
                    &db,
                    tx,
                    &AuditEntry::new(action, "volunteer", &volunteer_id),
                )?;
                Ok(true)
            };
            run.await.map_err(|e: ApiError| BackoffError::permanent(e))
        })
    })
    .await
    .map_err(fs_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestApp, all_docs, audit_actions};
    use serde_json::json;

    fn doc() -> VolunteerDoc {
        VolunteerDoc::from_profile(
            &serde_json::from_value(
                json!({ "name": "Ana", "show_phone": false, "language": "en" }),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn the_command_line_only_handles_validator_and_civil_defense() {
        assert_eq!(parse_cli_role("validator"), Some(GlobalRole::Validator));
        assert_eq!(
            parse_cli_role("civil_defense"),
            Some(GlobalRole::CivilDefense)
        );
        for other in ["mediator", "protection", "admin", "", "Validator"] {
            assert_eq!(parse_cli_role(other), None, "{other}");
        }
    }

    #[test]
    fn grant_adds_the_role_once_and_records_who_and_when() {
        let mut d = doc();
        let now = FirestoreInstant::now();
        assert!(apply(&mut d, GlobalRole::Validator, Change::Grant, now));
        assert_eq!(d.global_roles, [GlobalRole::Validator]);
        assert_eq!(
            d.role_grants,
            [RoleGrant {
                role: GlobalRole::Validator,
                by: "cli".into(),
                at: now
            }]
        );
        assert!(
            !apply(&mut d, GlobalRole::Validator, Change::Grant, now),
            "granting twice changes nothing"
        );
        assert_eq!((d.global_roles.len(), d.role_grants.len()), (1, 1));
    }

    #[test]
    fn revoke_removes_only_that_role() {
        let mut d = doc();
        let now = FirestoreInstant::now();
        apply(&mut d, GlobalRole::Validator, Change::Grant, now);
        apply(&mut d, GlobalRole::CivilDefense, Change::Grant, now);
        assert!(apply(&mut d, GlobalRole::Validator, Change::Revoke, now));
        assert_eq!(d.global_roles, [GlobalRole::CivilDefense]);
        assert_eq!(d.role_grants.len(), 1);
        assert!(
            !apply(&mut d, GlobalRole::Validator, Change::Revoke, now),
            "revoking a role not held changes nothing"
        );
    }

    #[tokio::test]
    async fn granted_role_shows_up_in_me_and_is_audited() {
        let app = TestApp::new().await;
        let (secret, id) = app.login("dev", "tok", "Ana").await;
        assert_eq!(
            app.get_authed("/v1/me", &secret).await.1["global_roles"],
            json!([])
        );

        assert!(
            change_role(&app.state.db, &id, GlobalRole::CivilDefense, Change::Grant)
                .await
                .unwrap()
        );
        let (_, me) = app.get_authed("/v1/me", &secret).await;
        assert_eq!(me["global_roles"], json!(["civil_defense"]));

        assert!(
            change_role(&app.state.db, &id, GlobalRole::CivilDefense, Change::Revoke)
                .await
                .unwrap()
        );
        assert_eq!(
            app.get_authed("/v1/me", &secret).await.1["global_roles"],
            json!([])
        );

        let entries = all_docs(&app.state.db, "audit_log").await;
        let grant = entries
            .iter()
            .find(|e| e["action"] == "role.grant.civil_defense")
            .unwrap();
        assert_eq!(
            (&grant["target_kind"], &grant["target_id"]),
            (&json!("volunteer"), &json!(id))
        );
        assert!(
            grant.get("volunteer_id").is_none(),
            "the operator is not a volunteer"
        );
        assert!(
            audit_actions(&app.state.db)
                .await
                .contains(&"role.revoke.civil_defense".to_string())
        );
    }

    #[tokio::test]
    async fn repeating_a_change_writes_nothing_more() {
        let app = TestApp::new().await;
        let (_, id) = app.login("dev", "tok", "Ana").await;
        let db = &app.state.db;
        let before = audit_actions(db).await.len();
        assert!(
            change_role(db, &id, GlobalRole::Validator, Change::Grant)
                .await
                .unwrap()
        );
        assert!(
            !change_role(db, &id, GlobalRole::Validator, Change::Grant)
                .await
                .unwrap()
        );
        assert!(
            !change_role(db, &id, GlobalRole::CivilDefense, Change::Revoke)
                .await
                .unwrap()
        );
        assert_eq!(audit_actions(db).await.len(), before + 1);
    }

    #[tokio::test]
    async fn unknown_volunteer_is_an_error() {
        let app = TestApp::new().await;
        let err = change_role(
            &app.state.db,
            "nobody",
            GlobalRole::Validator,
            Change::Grant,
        )
        .await
        .unwrap_err();
        assert_eq!(
            (err.status.as_u16(), err.code),
            (404, "volunteer_not_found")
        );
        assert!(all_docs(&app.state.db, "audit_log").await.is_empty());
    }
}
