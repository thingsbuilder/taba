//! Global roles and shelter memberships (spec 2.1, 2.3, 3.3, 4.4). The operator's side of the
//! roles is `taba role grant|revoke` for `validator` and `civil_defense`; `mediator` and
//! `protection` are granted through the app from phase 4. Members, removal, leaving and
//! banning are the routes of spec 8.4.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use firestore::errors::BackoffError;
use firestore::{FirestoreDb, FirestoreInstant, path};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::alerts::{self, AlertDoc, AlertKind};
use crate::audit::{self, AuditEntry};
use crate::auth::{AppState, Membership, Volunteer};
use crate::error::ApiError;
use crate::shelters::{self, ShelterDoc};
use crate::store::{self, fs_error, tx_replace};
use crate::volunteers::{COLLECTION as VOLUNTEERS, GlobalRole, RoleGrant, VolunteerDoc};

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
                let mut doc: VolunteerDoc = store::get(&db, VOLUNTEERS, &volunteer_id)
                    .await?
                    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "volunteer_not_found"))?;
                if !apply(&mut doc, role, change, crate::store::now()) {
                    return Ok(false);
                }
                tx_replace(&db, tx, VOLUNTEERS, &volunteer_id, &doc)?;
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

// ---- memberships ----

pub const COLLECTION: &str = "memberships";
const MAX_REASON_CHARS: usize = 500;

pub fn membership_id(shelter_id: &str, volunteer_id: &str) -> String {
    format!("{shelter_id}_{volunteer_id}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShelterRole {
    Admin,
    Volunteer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipStatus {
    Active,
    Removed,
    Left,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    Creator,
    Qr,
    Link,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MembershipDoc {
    pub shelter_id: String,
    pub volunteer_id: String,
    pub role: ShelterRole,
    pub status: MembershipStatus,
    pub via: Via,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invited_by: Option<String>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub joined_at: FirestoreInstant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_by: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub removed_at: Option<FirestoreInstant>,
}

impl MembershipDoc {
    /// The creator of a shelter is its first admin (spec 3.2.7).
    pub fn creator(shelter_id: &str, volunteer_id: &str, now: FirestoreInstant) -> Self {
        Self {
            shelter_id: shelter_id.into(),
            volunteer_id: volunteer_id.into(),
            role: ShelterRole::Admin,
            status: MembershipStatus::Active,
            via: Via::Creator,
            invite_code: None,
            invited_by: None,
            joined_at: now,
            removed_by: None,
            removed_at: None,
        }
    }

    fn remove(&mut self, by: &str, now: FirestoreInstant) {
        self.status = MembershipStatus::Removed;
        self.removed_by = Some(by.into());
        self.removed_at = Some(now);
    }
}

async fn active_where(
    db: &FirestoreDb,
    field: String,
    id: &str,
) -> Result<Vec<MembershipDoc>, ApiError> {
    db.fluent()
        .select()
        .from(COLLECTION)
        .filter(|q| {
            q.for_all([
                q.field(&field).eq(id),
                q.field(path!(MembershipDoc::status))
                    .eq(MembershipStatus::Active),
            ])
        })
        .obj()
        .query()
        .await
        .map_err(fs_error)
}

pub async fn active_of_volunteer(
    db: &FirestoreDb,
    volunteer_id: &str,
) -> Result<Vec<MembershipDoc>, ApiError> {
    active_where(db, path!(MembershipDoc::volunteer_id), volunteer_id).await
}

pub async fn active_in_shelter(
    db: &FirestoreDb,
    shelter_id: &str,
) -> Result<Vec<MembershipDoc>, ApiError> {
    active_where(db, path!(MembershipDoc::shelter_id), shelter_id).await
}

/// The active memberships of a volunteer with the shelter's name and confirmation, as the
/// `Volunteer` extractor and `GET /v1/me` need them.
pub async fn memberships_of(
    db: &FirestoreDb,
    volunteer_id: &str,
) -> Result<Vec<Membership>, ApiError> {
    let mut memberships = Vec::new();
    for doc in active_of_volunteer(db, volunteer_id).await? {
        let Some(shelter) =
            store::get::<ShelterDoc>(db, shelters::COLLECTION, &doc.shelter_id).await?
        else {
            continue;
        };
        memberships.push(Membership {
            shelter_id: doc.shelter_id,
            name: shelter.name,
            role: doc.role,
            confirmation: shelter.confirmation,
            event_id: shelter.event_id,
        });
    }
    memberships.sort_by(|a, b| a.shelter_id.cmp(&b.shelter_id));
    Ok(memberships)
}

fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

fn member_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "member_not_found")
}

fn last_admin() -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "last_admin")
}

async fn membership(
    db: &FirestoreDb,
    shelter_id: &str,
    volunteer_id: &str,
) -> Result<Option<MembershipDoc>, ApiError> {
    store::get(db, COLLECTION, &membership_id(shelter_id, volunteer_id)).await
}

/// The membership if it exists and is active.
async fn active_membership(
    db: &FirestoreDb,
    shelter_id: &str,
    volunteer_id: &str,
) -> Result<MembershipDoc, ApiError> {
    membership(db, shelter_id, volunteer_id)
        .await?
        .filter(|m| m.status == MembershipStatus::Active)
        .ok_or_else(member_not_found)
}

fn member_row(doc: &MembershipDoc, volunteer: &VolunteerDoc) -> Value {
    let mut row = json!({
        "volunteer_id": doc.volunteer_id,
        "name": volunteer.name,
        "role": doc.role,
        "via": doc.via,
        "joined_at": doc.joined_at.to_string(),
    });
    if volunteer.show_phone {
        row["phone"] = json!(volunteer.phone);
    }
    row
}

// ---- GET /v1/shelters/{id}/members ----

pub async fn list_members(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if volunteer.role_in(&shelter_id).is_none() && !volunteer.has_global(GlobalRole::CivilDefense) {
        return Err(forbidden());
    }
    let mut rows = Vec::new();
    for doc in active_in_shelter(&state.db, &shelter_id).await? {
        if let Some(profile) =
            store::get::<VolunteerDoc>(&state.db, VOLUNTEERS, &doc.volunteer_id).await?
        {
            rows.push((doc.joined_at, member_row(&doc, &profile)));
        }
    }
    rows.sort_by_key(|(joined_at, _)| *joined_at);
    let members: Vec<Value> = rows.into_iter().map(|(_, row)| row).collect();
    Ok(Json(json!({ "members": members })))
}

// ---- PATCH /v1/shelters/{id}/members/{volunteer_id} ----

#[derive(Debug, Deserialize)]
pub struct Promote {
    role: ShelterRole,
}

pub async fn promote_member(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path((shelter_id, target)): Path<(String, String)>,
    Json(request): Json<Promote>,
) -> Result<Json<Value>, ApiError> {
    if !volunteer.is_admin_of(&shelter_id) {
        return Err(forbidden());
    }
    if request.role != ShelterRole::Admin {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_role",
        ));
    }
    let (doc, profile) = store::transact(&state.db, |db, tx| {
        let (shelter_id, target, by) = (shelter_id.clone(), target.clone(), volunteer.id.clone());
        Box::pin(async move {
            let mut doc = active_membership(&db, &shelter_id, &target).await?;
            if doc.role != ShelterRole::Admin {
                doc.role = ShelterRole::Admin;
                tx_replace(
                    &db,
                    tx,
                    COLLECTION,
                    &membership_id(&shelter_id, &target),
                    &doc,
                )?;
                audit::add(
                    &db,
                    tx,
                    &AuditEntry::new(
                        "member.promote",
                        "membership",
                        &membership_id(&shelter_id, &target),
                    )
                    .by(&by),
                )?;
            }
            let profile: VolunteerDoc = store::get(&db, VOLUNTEERS, &target)
                .await?
                .ok_or_else(member_not_found)?;
            Ok((doc, profile))
        })
    })
    .await?;
    touch_shelter_event(&state, &shelter_id).await;
    Ok(Json(json!({ "member": member_row(&doc, &profile) })))
}

/// Records activity on the event of a shelter after a write to it (spec 4.3.3).
pub async fn touch_shelter_event(state: &AppState, shelter_id: &str) {
    if let Ok(Some(shelter)) =
        store::get::<ShelterDoc>(&state.db, shelters::COLLECTION, shelter_id).await
    {
        crate::events::touch(state, shelter.event_id.as_deref()).await;
    }
}

// ---- DELETE /v1/shelters/{id}/members/{volunteer_id} ----

pub async fn remove_member(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path((shelter_id, target)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let civil_defense = volunteer.has_global(GlobalRole::CivilDefense);
    if !volunteer.is_admin_of(&shelter_id) && !civil_defense {
        return Err(forbidden());
    }
    store::transact(&state.db, |db, tx| {
        let (shelter_id, target, by) = (shelter_id.clone(), target.clone(), volunteer.id.clone());
        Box::pin(async move {
            let mut doc = active_membership(&db, &shelter_id, &target).await?;
            if doc.role == ShelterRole::Admin {
                // An admin removes volunteers only; the Civil Defense may remove an admin
                // as long as another one stays (spec 2.3).
                if !civil_defense {
                    return Err(ApiError::new(
                        StatusCode::FORBIDDEN,
                        "admin_cannot_remove_admin",
                    ));
                }
                if admin_count(&db, &shelter_id).await? <= 1 {
                    return Err(last_admin());
                }
            }
            doc.remove(&by, store::now());
            let id = membership_id(&shelter_id, &target);
            tx_replace(&db, tx, COLLECTION, &id, &doc)?;
            // The person's admins elsewhere hear about it (spec 4.4).
            let elsewhere: Vec<String> = active_of_volunteer(&db, &target)
                .await?
                .into_iter()
                .filter(|m| m.shelter_id != shelter_id)
                .map(|m| m.shelter_id)
                .collect();
            if !elsewhere.is_empty() {
                alerts::add(
                    &db,
                    tx,
                    &AlertDoc::new(
                        AlertKind::VolunteerRemoved,
                        "volunteer",
                        &target,
                        json!({ "removed_from_shelter_id": shelter_id }),
                    )
                    .for_shelters(elsewhere)
                    .about_volunteers(vec![target.clone()]),
                )?;
            }
            audit::add(
                &db,
                tx,
                &AuditEntry::new("member.remove", "membership", &id).by(&by),
            )
        })
    })
    .await?;
    touch_shelter_event(&state, &shelter_id).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn admin_count(db: &FirestoreDb, shelter_id: &str) -> Result<usize, ApiError> {
    Ok(active_in_shelter(db, shelter_id)
        .await?
        .iter()
        .filter(|m| m.role == ShelterRole::Admin)
        .count())
}

// ---- POST /v1/shelters/{id}/leave ----

pub async fn leave_shelter(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if volunteer.role_in(&shelter_id).is_none() {
        return Err(forbidden());
    }
    store::transact(&state.db, |db, tx| {
        let (shelter_id, who) = (shelter_id.clone(), volunteer.id.clone());
        Box::pin(async move {
            let mut doc = active_membership(&db, &shelter_id, &who).await?;
            if doc.role == ShelterRole::Admin && admin_count(&db, &shelter_id).await? <= 1 {
                return Err(last_admin());
            }
            doc.status = MembershipStatus::Left;
            let id = membership_id(&shelter_id, &who);
            tx_replace(&db, tx, COLLECTION, &id, &doc)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new("member.leave", "membership", &id).by(&who),
            )
        })
    })
    .await?;
    touch_shelter_event(&state, &shelter_id).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- POST /v1/volunteers/{id}/ban ----

#[derive(Debug, Deserialize)]
pub struct Ban {
    reason: String,
}

/// Only the id of a session document is needed to delete it.
#[derive(Deserialize)]
struct SessionRef {
    #[serde(alias = "_firestore_id")]
    id: String,
}

/// Bans a volunteer from the network (spec 4.4): marked banned, removed from every shelter,
/// sessions deleted. Sessions that outlive a failed delete are refused anyway (`Volunteer`).
pub async fn ban_volunteer(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(target): Path<String>,
    Json(request): Json<Ban>,
) -> Result<Json<Value>, ApiError> {
    if !volunteer.has_global(GlobalRole::CivilDefense) {
        return Err(forbidden());
    }
    let reason = request.reason.trim().to_string();
    if reason.is_empty() || reason.chars().count() > MAX_REASON_CHARS {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_reason",
        ));
    }
    store::transact(&state.db, |db, tx| {
        let (target, by, reason) = (target.clone(), volunteer.id.clone(), reason.clone());
        Box::pin(async move {
            let mut doc: VolunteerDoc = store::get(&db, VOLUNTEERS, &target)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "volunteer_not_found"))?;
            if doc.banned {
                return Ok(());
            }
            let now = store::now();
            doc.banned = true;
            doc.banned_by = Some(by.clone());
            doc.banned_at = Some(now);
            doc.flag_reasons.push(format!("banned: {reason}"));
            tx_replace(&db, tx, VOLUNTEERS, &target, &doc)?;
            for mut membership in active_of_volunteer(&db, &target).await? {
                membership.remove(&by, now);
                let id = membership_id(&membership.shelter_id, &target);
                tx_replace(&db, tx, COLLECTION, &id, &membership)?;
            }
            let sessions: Vec<SessionRef> = db
                .fluent()
                .select()
                .from(crate::auth::SESSIONS)
                .filter(|q| q.for_all([q.field("volunteer_id").eq(target.as_str())]))
                .obj()
                .query()
                .await
                .map_err(fs_error)?;
            for session in sessions {
                store::tx_delete(&db, tx, crate::auth::SESSIONS, &session.id)?;
            }
            audit::add(
                &db,
                tx,
                &AuditEntry::new("volunteer.ban", "volunteer", &target).by(&by),
            )
        })
    })
    .await?;
    Ok(Json(json!({ "volunteer_id": target, "banned": true })))
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
        let now = crate::store::now();
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
        let now = crate::store::now();
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

    #[tokio::test]
    async fn concurrent_changes_to_one_volunteer_all_land() {
        let app = TestApp::new().await;
        let (_, id) = app.login("dev", "tok", "Ana").await;
        let db = &app.state.db;
        let (a, b, c) = tokio::join!(
            change_role(db, &id, GlobalRole::Validator, Change::Grant),
            change_role(db, &id, GlobalRole::CivilDefense, Change::Grant),
            change_role(db, &id, GlobalRole::Validator, Change::Grant),
        );
        // The two grants of `validator` race: exactly one of them changes anything.
        assert!(b.unwrap());
        assert_eq!([a.unwrap(), c.unwrap()].iter().filter(|x| **x).count(), 1);
        let doc: VolunteerDoc = store::get(db, VOLUNTEERS, &id).await.unwrap().unwrap();
        assert_eq!(doc.global_roles.len(), 2);
        assert_eq!(doc.role_grants.len(), 2);
    }
}

#[cfg(test)]
mod membership_tests {
    use super::*;
    use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, audit_actions, north_of_origin};

    fn error_of(body: &Value) -> Option<&str> {
        body["error"].as_str()
    }

    /// A shelter with its admin, one volunteer and a second admin.
    struct Crew {
        app: TestApp,
        shelter: String,
        admin: Actor,
        volunteer: Actor,
        second_admin: Actor,
    }

    async fn crew() -> Crew {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.create_shelter(&admin, "Ginásio", ORIGIN).await;
        let volunteer = app
            .join(
                &app.invite(&admin, &shelter, "qr", "volunteer").await,
                "Vol",
            )
            .await;
        let second_admin = app
            .join(&app.invite(&admin, &shelter, "qr", "admin").await, "Second")
            .await;
        Crew {
            app,
            shelter,
            admin,
            volunteer,
            second_admin,
        }
    }

    async fn status_of(app: &TestApp, shelter: &str, who: &Actor) -> MembershipStatus {
        membership(&app.state.db, shelter, &who.id)
            .await
            .unwrap()
            .unwrap()
            .status
    }

    fn members_path(shelter: &str) -> String {
        format!("/v1/shelters/{shelter}/members")
    }

    // ---- list ----

    #[tokio::test]
    async fn members_list_shows_roles_and_the_phone_only_when_allowed() {
        let c = crew().await;
        c.app
            .send(
                "PATCH",
                "/v1/me",
                &c.volunteer.secret,
                Some(json!({ "phone": "123", "show_phone": true })),
            )
            .await;
        c.app
            .send(
                "PATCH",
                "/v1/me",
                &c.admin.secret,
                Some(json!({ "phone": "456", "show_phone": false })),
            )
            .await;
        let (status, body) = c
            .app
            .send("GET", &members_path(&c.shelter), &c.volunteer.secret, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let rows = body["members"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        let row_of = |id: &str| {
            rows.iter()
                .find(|r| r["volunteer_id"] == json!(id))
                .unwrap()
        };
        assert_eq!(
            (
                &row_of(&c.admin.id)["role"],
                &row_of(&c.admin.id)["via"],
                &row_of(&c.admin.id)["name"]
            ),
            (&json!("admin"), &json!("creator"), &json!("Admin"))
        );
        assert_eq!(
            (
                &row_of(&c.volunteer.id)["role"],
                &row_of(&c.volunteer.id)["via"],
                &row_of(&c.volunteer.id)["phone"]
            ),
            (&json!("volunteer"), &json!("qr"), &json!("123"))
        );
        assert!(row_of(&c.admin.id).get("phone").is_none());
        assert_eq!(rows[0]["volunteer_id"], json!(c.admin.id), "oldest first");
    }

    #[tokio::test]
    async fn only_members_and_the_civil_defense_list_the_members() {
        let c = crew().await;
        let stranger = c.app.actor("Ze").await;
        let validator = c.app.actor("Val").await;
        c.app.grant(&validator, GlobalRole::Validator).await;
        let civil_defense = c.app.actor("DC").await;
        c.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        for who in [&stranger, &validator] {
            assert_eq!(
                c.app
                    .send("GET", &members_path(&c.shelter), &who.secret, None)
                    .await
                    .0,
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            c.app
                .send(
                    "GET",
                    &members_path(&c.shelter),
                    &civil_defense.secret,
                    None
                )
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            c.app
                .request("GET", &members_path(&c.shelter), &[], None)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }

    // ---- promote ----

    #[tokio::test]
    async fn an_admin_promotes_a_volunteer() {
        let c = crew().await;
        let path = format!("{}/{}", members_path(&c.shelter), c.volunteer.id);
        let (status, body) = c
            .app
            .send(
                "PATCH",
                &path,
                &c.admin.secret,
                Some(json!({ "role": "admin" })),
            )
            .await;
        assert_eq!(
            (status, &body["member"]["role"]),
            (StatusCode::OK, &json!("admin")),
            "{body}"
        );
        let (_, me) = c.app.get_authed("/v1/me", &c.volunteer.secret).await;
        assert_eq!(me["memberships"][0]["role"], "admin");
        // Again: nothing changes, nothing more is audited.
        let audits = audit_actions(&c.app.state.db)
            .await
            .iter()
            .filter(|a| *a == "member.promote")
            .count();
        assert_eq!(
            c.app
                .send(
                    "PATCH",
                    &path,
                    &c.admin.secret,
                    Some(json!({ "role": "admin" }))
                )
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            audit_actions(&c.app.state.db)
                .await
                .iter()
                .filter(|a| *a == "member.promote")
                .count(),
            audits
        );
    }

    #[tokio::test]
    async fn only_an_admin_promotes_and_only_to_admin_and_only_a_member() {
        let c = crew().await;
        let civil_defense = c.app.actor("DC").await;
        c.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let path = format!("{}/{}", members_path(&c.shelter), c.volunteer.id);
        let body = Some(json!({ "role": "admin" }));
        for who in [&c.volunteer, &civil_defense] {
            assert_eq!(
                c.app
                    .send("PATCH", &path, &who.secret, body.clone())
                    .await
                    .0,
                StatusCode::FORBIDDEN
            );
        }
        let (status, answer) = c
            .app
            .send(
                "PATCH",
                &path,
                &c.admin.secret,
                Some(json!({ "role": "volunteer" })),
            )
            .await;
        assert_eq!(
            (status, error_of(&answer)),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_role"))
        );
        let stranger = c.app.actor("Ze").await;
        let (status, answer) = c
            .app
            .send(
                "PATCH",
                &format!("{}/{}", members_path(&c.shelter), stranger.id),
                &c.admin.secret,
                body,
            )
            .await;
        assert_eq!(
            (status, error_of(&answer)),
            (StatusCode::NOT_FOUND, Some("member_not_found"))
        );
    }

    // ---- remove ----

    #[tokio::test]
    async fn an_admin_removes_a_volunteer_but_not_another_admin() {
        let c = crew().await;
        let volunteer_path = format!("{}/{}", members_path(&c.shelter), c.volunteer.id);
        let admin_path = format!("{}/{}", members_path(&c.shelter), c.second_admin.id);
        let (status, body) = c
            .app
            .send("DELETE", &admin_path, &c.admin.secret, None)
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::FORBIDDEN, Some("admin_cannot_remove_admin"))
        );
        assert_eq!(
            status_of(&c.app, &c.shelter, &c.second_admin).await,
            MembershipStatus::Active
        );
        // A volunteer removes no one; the second admin removes the volunteer.
        assert_eq!(
            c.app
                .send("DELETE", &admin_path, &c.volunteer.secret, None)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            c.app
                .send("DELETE", &volunteer_path, &c.second_admin.secret, None)
                .await
                .0,
            StatusCode::NO_CONTENT
        );
        let removed = membership(&c.app.state.db, &c.shelter, &c.volunteer.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (removed.status, removed.removed_by.as_deref()),
            (MembershipStatus::Removed, Some(c.second_admin.id.as_str()))
        );
        assert!(removed.removed_at.is_some());
        // The removed volunteer no longer works in the shelter, and is not listed.
        let (_, me) = c.app.get_authed("/v1/me", &c.volunteer.secret).await;
        assert_eq!(me["memberships"], json!([]));
        assert_eq!(
            c.app
                .send("GET", &members_path(&c.shelter), &c.volunteer.secret, None)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        // Removing someone twice, or who never was a member, is a 404.
        let (status, body) = c
            .app
            .send("DELETE", &volunteer_path, &c.admin.secret, None)
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::NOT_FOUND, Some("member_not_found"))
        );
        assert_eq!(
            c.app
                .send(
                    "DELETE",
                    &format!("{}/nobody", members_path(&c.shelter)),
                    &c.admin.secret,
                    None
                )
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn an_admin_cannot_remove_themself_and_a_stranger_removes_no_one() {
        let c = crew().await;
        let own = format!("{}/{}", members_path(&c.shelter), c.admin.id);
        let (status, body) = c.app.send("DELETE", &own, &c.admin.secret, None).await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::FORBIDDEN, Some("admin_cannot_remove_admin"))
        );
        let stranger = c.app.actor("Ze").await;
        let validator = c.app.actor("Val").await;
        c.app.grant(&validator, GlobalRole::Validator).await;
        let target = format!("{}/{}", members_path(&c.shelter), c.volunteer.id);
        for who in [&stranger, &validator] {
            assert_eq!(
                c.app.send("DELETE", &target, &who.secret, None).await.0,
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            c.app.request("DELETE", &target, &[], None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn the_civil_defense_removes_an_admin_but_never_the_last_one() {
        let c = crew().await;
        let civil_defense = c.app.actor("DC").await;
        c.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let second = format!("{}/{}", members_path(&c.shelter), c.second_admin.id);
        let first = format!("{}/{}", members_path(&c.shelter), c.admin.id);
        assert_eq!(
            c.app
                .send("DELETE", &second, &civil_defense.secret, None)
                .await
                .0,
            StatusCode::NO_CONTENT
        );
        let (status, body) = c
            .app
            .send("DELETE", &first, &civil_defense.secret, None)
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::CONFLICT, Some("last_admin"))
        );
        assert_eq!(
            status_of(&c.app, &c.shelter, &c.admin).await,
            MembershipStatus::Active
        );
    }

    #[tokio::test]
    async fn a_removal_alerts_the_admins_of_the_other_shelters_of_that_person() {
        let c = crew().await;
        // The volunteer also belongs to a second shelter, whose admin should hear about it.
        let other_admin = c.app.actor("Other").await;
        let other_shelter = c
            .app
            .create_shelter(&other_admin, "Escola", north_of_origin(5_000.0))
            .await;
        c.app
            .join_again(
                &c.app
                    .invite(&other_admin, &other_shelter, "qr", "volunteer")
                    .await,
                &c.volunteer,
            )
            .await;

        let path = format!("{}/{}", members_path(&c.shelter), c.volunteer.id);
        assert_eq!(
            c.app.send("DELETE", &path, &c.admin.secret, None).await.0,
            StatusCode::NO_CONTENT
        );
        let alerts = all_docs(&c.app.state.db, "alerts").await;
        assert_eq!(alerts.len(), 1);
        assert_eq!(
            (
                &alerts[0]["kind"],
                &alerts[0]["shelter_ids"],
                &alerts[0]["volunteer_ids"],
                &alerts[0]["informational"]
            ),
            (
                &json!("volunteer_removed"),
                &json!([other_shelter]),
                &json!([c.volunteer.id]),
                &json!(false)
            )
        );
        assert_eq!(
            alerts[0]["detail"]["removed_from_shelter_id"],
            json!(c.shelter)
        );
        // The other shelter's admin sees it; this shelter's admin does not.
        let (_, theirs) = c
            .app
            .send("GET", "/v1/alerts", &other_admin.secret, None)
            .await;
        assert_eq!(theirs["alerts"].as_array().unwrap().len(), 1);
        let (_, ours) = c.app.send("GET", "/v1/alerts", &c.admin.secret, None).await;
        assert!(ours["alerts"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn removing_someone_with_no_other_shelter_raises_no_alert() {
        let c = crew().await;
        c.app
            .send(
                "DELETE",
                &format!("{}/{}", members_path(&c.shelter), c.volunteer.id),
                &c.admin.secret,
                None,
            )
            .await;
        assert!(all_docs(&c.app.state.db, "alerts").await.is_empty());
    }

    // ---- leave ----

    #[tokio::test]
    async fn anyone_leaves_and_an_admin_leaves_while_another_remains() {
        let c = crew().await;
        let path = format!("/v1/shelters/{}/leave", c.shelter);
        assert_eq!(
            c.app.send("POST", &path, &c.volunteer.secret, None).await.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            status_of(&c.app, &c.shelter, &c.volunteer).await,
            MembershipStatus::Left
        );
        assert_eq!(
            c.app
                .send("POST", &path, &c.second_admin.secret, None)
                .await
                .0,
            StatusCode::NO_CONTENT
        );
        // Not a member any more: the shelter does not let them leave twice.
        assert_eq!(
            c.app.send("POST", &path, &c.volunteer.secret, None).await.0,
            StatusCode::FORBIDDEN
        );
        let stranger = c.app.actor("Ze").await;
        assert_eq!(
            c.app.send("POST", &path, &stranger.secret, None).await.0,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn the_last_admin_cannot_leave_until_another_is_promoted() {
        let c = crew().await;
        let path = format!("/v1/shelters/{}/leave", c.shelter);
        c.app
            .send("POST", &path, &c.second_admin.secret, None)
            .await;
        let (status, body) = c.app.send("POST", &path, &c.admin.secret, None).await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::CONFLICT, Some("last_admin"))
        );
        c.app
            .send(
                "PATCH",
                &format!("{}/{}", members_path(&c.shelter), c.volunteer.id),
                &c.admin.secret,
                Some(json!({ "role": "admin" })),
            )
            .await;
        assert_eq!(
            c.app.send("POST", &path, &c.admin.secret, None).await.0,
            StatusCode::NO_CONTENT
        );
    }

    #[tokio::test]
    async fn two_admins_leaving_at_once_leave_one_behind() {
        let c = crew().await;
        // Remove the volunteer so only the two admins are left.
        c.app
            .send(
                "DELETE",
                &format!("{}/{}", members_path(&c.shelter), c.volunteer.id),
                &c.admin.secret,
                None,
            )
            .await;
        let path = format!("/v1/shelters/{}/leave", c.shelter);
        let (a, b) = tokio::join!(
            c.app.send("POST", &path, &c.admin.secret, None),
            c.app.send("POST", &path, &c.second_admin.secret, None),
        );
        let mut statuses = [a.0, b.0];
        statuses.sort();
        assert_eq!(
            statuses,
            [StatusCode::NO_CONTENT, StatusCode::CONFLICT],
            "{a:?} {b:?}"
        );
        let admins = active_in_shelter(&c.app.state.db, &c.shelter)
            .await
            .unwrap();
        assert_eq!(admins.len(), 1);
    }

    #[tokio::test]
    async fn the_civil_defense_removing_an_admin_while_the_other_leaves_keeps_one() {
        let c = crew().await;
        c.app
            .send(
                "DELETE",
                &format!("{}/{}", members_path(&c.shelter), c.volunteer.id),
                &c.admin.secret,
                None,
            )
            .await;
        let civil_defense = c.app.actor("DC").await;
        c.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let remove_path = format!("{}/{}", members_path(&c.shelter), c.second_admin.id);
        let leave_path = format!("/v1/shelters/{}/leave", c.shelter);
        let (removed, left) = tokio::join!(
            c.app
                .send("DELETE", &remove_path, &civil_defense.secret, None),
            c.app.send("POST", &leave_path, &c.admin.secret, None),
        );
        let mut statuses = [removed.0, left.0];
        statuses.sort();
        assert_eq!(statuses, [StatusCode::NO_CONTENT, StatusCode::CONFLICT]);
        assert_eq!(
            active_in_shelter(&c.app.state.db, &c.shelter)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    // ---- ban ----

    #[tokio::test]
    async fn only_the_civil_defense_bans_and_needs_a_reason() {
        let c = crew().await;
        let validator = c.app.actor("Val").await;
        c.app.grant(&validator, GlobalRole::Validator).await;
        let path = format!("/v1/volunteers/{}/ban", c.volunteer.id);
        for who in [&c.admin, &c.volunteer, &validator] {
            assert_eq!(
                c.app
                    .send("POST", &path, &who.secret, Some(json!({ "reason": "x" })))
                    .await
                    .0,
                StatusCode::FORBIDDEN
            );
        }
        let civil_defense = c.app.actor("DC").await;
        c.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let (status, body) = c
            .app
            .send(
                "POST",
                &path,
                &civil_defense.secret,
                Some(json!({ "reason": "  " })),
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_reason"))
        );
        let (status, body) = c
            .app
            .send(
                "POST",
                "/v1/volunteers/nobody/ban",
                &civil_defense.secret,
                Some(json!({ "reason": "x" })),
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::NOT_FOUND, Some("volunteer_not_found"))
        );
        assert_eq!(
            c.app
                .request("POST", &path, &[], Some(json!({ "reason": "x" })))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn a_ban_marks_the_profile_removes_every_membership_and_ends_the_sessions() {
        let c = crew().await;
        let other_admin = c.app.actor("Other").await;
        let other_shelter = c
            .app
            .create_shelter(&other_admin, "Escola", north_of_origin(5_000.0))
            .await;
        c.app
            .join_again(
                &c.app
                    .invite(&other_admin, &other_shelter, "qr", "volunteer")
                    .await,
                &c.volunteer,
            )
            .await;
        // A second session on another device.
        let (second_session, _) = c
            .app
            .login(&c.volunteer.device, &c.volunteer.token, "Vol")
            .await;
        let civil_defense = c.app.actor("DC").await;
        c.app.grant(&civil_defense, GlobalRole::CivilDefense).await;

        let path = format!("/v1/volunteers/{}/ban", c.volunteer.id);
        let (status, body) = c
            .app
            .send(
                "POST",
                &path,
                &civil_defense.secret,
                Some(json!({ "reason": "se passou por outra pessoa" })),
            )
            .await;
        assert_eq!(
            (status, &body["banned"]),
            (StatusCode::OK, &json!(true)),
            "{body}"
        );

        let profile: VolunteerDoc = store::get(&c.app.state.db, VOLUNTEERS, &c.volunteer.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (profile.banned, profile.banned_by.as_deref()),
            (true, Some(civil_defense.id.as_str()))
        );
        assert!(profile.banned_at.is_some());
        assert_eq!(profile.flag_reasons, ["banned: se passou por outra pessoa"]);
        for shelter in [&c.shelter, &other_shelter] {
            assert_eq!(
                status_of(&c.app, shelter, &c.volunteer).await,
                MembershipStatus::Removed
            );
        }
        // Both sessions are gone; the ban does not leave the other people's alone.
        for session in [&c.volunteer.secret, &second_session] {
            assert_eq!(
                c.app.get_authed("/v1/me", session).await.0,
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(
            c.app.get_authed("/v1/me", &c.admin.secret).await.0,
            StatusCode::OK
        );
        // And they cannot log in again.
        let (device, token) = (c.volunteer.device.clone(), c.volunteer.token.clone());
        let (_, capture) = c
            .app
            .post("/v1/captures", &device, json!({ "purpose": "login" }))
            .await;
        let process = c.app.unico.with(|s| format!("proc-{}", s.posts.len()));
        c.app.unico.finish_with_token(&process, &token);
        let (status, body) = c
            .app
            .post(
                "/v1/sessions",
                &device,
                json!({ "capture_id": capture["capture_id"] }),
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::FORBIDDEN, Some("banned"))
        );
        assert!(
            audit_actions(&c.app.state.db)
                .await
                .contains(&"volunteer.ban".to_string())
        );
    }

    #[tokio::test]
    async fn banning_twice_changes_nothing() {
        let c = crew().await;
        let civil_defense = c.app.actor("DC").await;
        c.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let path = format!("/v1/volunteers/{}/ban", c.volunteer.id);
        c.app
            .send(
                "POST",
                &path,
                &civil_defense.secret,
                Some(json!({ "reason": "um" })),
            )
            .await;
        c.app
            .send(
                "POST",
                &path,
                &civil_defense.secret,
                Some(json!({ "reason": "dois" })),
            )
            .await;
        let profile: VolunteerDoc = store::get(&c.app.state.db, VOLUNTEERS, &c.volunteer.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(profile.flag_reasons, ["banned: um"]);
        assert_eq!(
            audit_actions(&c.app.state.db)
                .await
                .iter()
                .filter(|a| *a == "volunteer.ban")
                .count(),
            1
        );
    }
}
