use super::domain::apply;
use super::persist::membership;
use super::*;
use crate::store;
use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, audit_actions, north_of_origin};
use crate::volunteers::COLLECTION as VOLUNTEERS;
use crate::volunteers::{GlobalRole, RoleGrant, VolunteerDoc};
use axum::http::StatusCode;
use serde_json::{Value, json};

fn doc() -> VolunteerDoc {
    VolunteerDoc::from_profile(
        &serde_json::from_value(json!({ "name": "Ana", "show_phone": false, "language": "en" }))
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

#[tokio::test]
async fn a_ban_never_leaves_a_shelter_without_an_admin() {
    let c = crew().await;
    let civil_defense = c.app.actor("DC").await;
    c.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
    // Two admins: banning one is fine; the other is then the last and cannot be banned.
    let first = format!("/v1/volunteers/{}/ban", c.second_admin.id);
    assert_eq!(
        c.app
            .send(
                "POST",
                &first,
                &civil_defense.secret,
                Some(json!({ "reason": "x" }))
            )
            .await
            .0,
        StatusCode::OK
    );
    let last = format!("/v1/volunteers/{}/ban", c.admin.id);
    let (status, body) = c
        .app
        .send(
            "POST",
            &last,
            &civil_defense.secret,
            Some(json!({ "reason": "x" })),
        )
        .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::CONFLICT, Some("last_admin"))
    );
    let profile: VolunteerDoc = store::get(&c.app.state.db, VOLUNTEERS, &c.admin.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!profile.banned);
    assert_eq!(
        status_of(&c.app, &c.shelter, &c.admin).await,
        MembershipStatus::Active
    );
    assert_eq!(
        c.app.get_authed("/v1/me", &c.admin.secret).await.0,
        StatusCode::OK,
        "nothing was undone"
    );
    // Once another admin exists, the ban goes through.
    c.app
        .send(
            "PATCH",
            &format!("{}/{}", members_path(&c.shelter), c.volunteer.id),
            &c.admin.secret,
            Some(json!({ "role": "admin" })),
        )
        .await;
    assert_eq!(
        c.app
            .send(
                "POST",
                &last,
                &civil_defense.secret,
                Some(json!({ "reason": "x" }))
            )
            .await
            .0,
        StatusCode::OK
    );
}
