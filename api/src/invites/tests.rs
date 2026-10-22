use super::domain::QR_TTL;
use super::model::{InviteDoc, InviteKind, InviteStatus};
use super::*;
use crate::members::{self, MembershipDoc, MembershipStatus, ShelterRole, Via, membership_id};
use crate::store;
use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, audit_actions, new_id};
use crate::volunteers::GlobalRole;
use axum::http::StatusCode;
use firestore::jiff::SignedDuration;
use serde_json::{Value, json};

// ---- the invite and its membership (no database) ----

fn invite_of(kind: InviteKind, role: ShelterRole) -> InviteDoc {
    let now = store::now();
    InviteDoc {
        shelter_id: "s1".into(),
        kind,
        role,
        created_by: "admin".into(),
        created_at: now,
        expires_at: now + QR_TTL,
        status: InviteStatus::Active,
        used_by: None,
        used_at: None,
    }
}

/// A replay does not widen access: an invite is spent once, and a spent, canceled or expired
/// one grants nothing, to the same person or to another.
#[test]
fn an_invite_is_spent_once() {
    let now = store::now();
    let mut invite = invite_of(InviteKind::Qr, ShelterRole::Volunteer);
    invite.redeem("p1", now).unwrap();
    assert_eq!(invite.status, InviteStatus::Used);
    assert_eq!(invite.used_by.as_deref(), Some("p1"));
    for person in ["p1", "p2"] {
        assert_eq!(
            invite.redeem(person, now).unwrap_err().code,
            "invite_invalid"
        );
    }
    assert_eq!(invite.used_by.as_deref(), Some("p1"));
    assert!(invite.cancel(now).is_err());

    let mut canceled = invite_of(InviteKind::Link, ShelterRole::Volunteer);
    canceled.cancel(now).unwrap();
    assert!(canceled.redeem("p1", now).is_err());
    assert!(
        canceled.cancel(now).is_err(),
        "canceling twice changes nothing"
    );

    let mut expired = invite_of(InviteKind::Qr, ShelterRole::Volunteer);
    let later = expired.expires_at;
    assert!(expired.redeem("p1", later).is_err());
    assert_eq!(expired.status, InviteStatus::Active);
}

#[test]
fn who_may_come_in_by_which_invite() {
    use MembershipStatus::*;
    let qr = invite_of(InviteKind::Qr, ShelterRole::Volunteer);
    let link = invite_of(InviteKind::Link, ShelterRole::Volunteer);
    // (previous membership, by qr, by link)
    let cases = [
        (None, None, None),
        (Some(Left), None, None),
        (Some(Removed), None, Some("removed_from_shelter")),
        (Some(Active), Some("already_member"), Some("already_member")),
    ];
    for (previous, by_qr, by_link) in cases {
        assert_eq!(
            qr.admits(previous).err().map(|e| e.code),
            by_qr,
            "{previous:?}"
        );
        assert_eq!(
            link.admits(previous).err().map(|e| e.code),
            by_link,
            "{previous:?}"
        );
    }
}

#[test]
fn the_membership_carries_the_role_and_the_way_in() {
    let now = store::now();
    let admin_by_qr = invite_of(InviteKind::Qr, ShelterRole::Admin).membership("c1", "p1", now);
    assert_eq!(admin_by_qr.role, ShelterRole::Admin);
    assert_eq!(admin_by_qr.via, Via::Qr);
    assert_eq!(admin_by_qr.status, MembershipStatus::Active);
    assert_eq!(admin_by_qr.invite_code.as_deref(), Some("c1"));
    assert_eq!(admin_by_qr.invited_by.as_deref(), Some("admin"));
    let by_link = invite_of(InviteKind::Link, ShelterRole::Volunteer).membership("c2", "p1", now);
    assert_eq!(by_link.via, Via::Link);
}

fn error_of(body: &Value) -> Option<&str> {
    body["error"].as_str()
}

async fn invite_doc(app: &TestApp, code: &str) -> InviteDoc {
    store::get(&app.state.db, COLLECTION, code)
        .await
        .unwrap()
        .unwrap()
}

/// Moves the invite's expiry `minutes` into the past (or future, if negative).
async fn expired_for(app: &TestApp, code: &str, minutes: i64) {
    let mut invite = invite_doc(app, code).await;
    invite.expires_at = store::now() - SignedDuration::from_mins(minutes);
    store::replace(&app.state.db, COLLECTION, code, &invite)
        .await
        .unwrap();
}

async fn setup() -> (TestApp, Actor, String) {
    let app = TestApp::new().await;
    let admin = app.actor("Admin").await;
    let shelter = app.create_shelter(&admin, "Ginásio", ORIGIN).await;
    (app, admin, shelter)
}

// ---- creating ----

#[tokio::test]
async fn qr_invites_last_30_minutes_and_links_24_hours() {
    let (app, admin, shelter) = setup().await;
    for (kind, role, minutes) in [
        ("qr", "volunteer", 30),
        ("qr", "admin", 30),
        ("link", "volunteer", 24 * 60),
    ] {
        let (status, body) = app
            .send(
                "POST",
                &format!("/v1/shelters/{shelter}/invites"),
                &admin.secret,
                Some(json!({ "kind": kind, "role": role })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{kind} {role}: {body}");
        let code = body["code"].as_str().unwrap();
        assert_eq!(
            body["url"],
            json!(format!("https://taba.example.org/invite/{code}"))
        );
        // 128 random bits in base64url: 22 characters.
        assert_eq!(code.len(), 22);
        let invite = invite_doc(&app, code).await;
        assert_eq!(
            (
                invite.kind,
                invite.role,
                invite.status,
                invite.created_by.as_str(),
                invite.shelter_id.as_str()
            ),
            (
                if kind == "qr" {
                    InviteKind::Qr
                } else {
                    InviteKind::Link
                },
                if role == "admin" {
                    ShelterRole::Admin
                } else {
                    ShelterRole::Volunteer
                },
                InviteStatus::Active,
                admin.id.as_str(),
                shelter.as_str()
            )
        );
        assert_eq!(
            invite.expires_at.as_second() - invite.created_at.as_second(),
            minutes * 60
        );
        assert_eq!(body["expires_at"], json!(invite.expires_at.to_string()));
    }
    assert_eq!(
        audit_actions(&app.state.db)
            .await
            .iter()
            .filter(|a| *a == "invite.create")
            .count(),
        3
    );
}

#[tokio::test]
async fn a_link_cannot_make_an_admin() {
    let (app, admin, shelter) = setup().await;
    let (status, body) = app
        .send(
            "POST",
            &format!("/v1/shelters/{shelter}/invites"),
            &admin.secret,
            Some(json!({ "kind": "link", "role": "admin" })),
        )
        .await;
    assert_eq!(
        (status, error_of(&body)),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("link_volunteer_only")
        )
    );
    assert!(all_docs(&app.state.db, COLLECTION).await.is_empty());
}

#[tokio::test]
async fn only_an_admin_of_the_shelter_invites() {
    let (app, admin, shelter) = setup().await;
    let volunteer = app
        .join(
            &app.invite(&admin, &shelter, "qr", "volunteer").await,
            "Vol",
        )
        .await;
    let stranger = app.actor("Ze").await;
    let civil_defense = app.actor("DC").await;
    app.grant(&civil_defense, GlobalRole::CivilDefense).await;
    let other_admin = app.actor("Other").await;
    app.create_shelter(
        &other_admin,
        "Outro",
        crate::test_support::north_of_origin(5_000.0),
    )
    .await;
    for who in [&volunteer, &stranger, &civil_defense, &other_admin] {
        let (status, _) = app
            .send(
                "POST",
                &format!("/v1/shelters/{shelter}/invites"),
                &who.secret,
                Some(json!({ "kind": "qr", "role": "volunteer" })),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{}", who.id);
    }
    let (status, _, _) = app
        .request(
            "POST",
            &format!("/v1/shelters/{shelter}/invites"),
            &[],
            Some(json!({ "kind": "qr", "role": "volunteer" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ---- looking ----

#[tokio::test]
async fn the_invite_page_shows_the_shelter_only_while_the_invite_works() {
    let (app, admin, shelter) = setup().await;
    let code = app.invite(&admin, &shelter, "qr", "admin").await;
    let (status, _, seen) = app
        .request("GET", &format!("/v1/invites/{code}"), &[], None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        seen,
        json!({ "valid": true, "shelter_name": "Ginásio", "role": "admin", "kind": "qr", "confirmed": false })
    );

    let unusable = json!({ "valid": false, "shelter_name": null, "role": null, "kind": null, "confirmed": null });
    let (_, _, unknown) = app.request("GET", "/v1/invites/nope", &[], None).await;
    assert_eq!(unknown, unusable);
    expired_for(&app, &code, 1).await;
    let (_, _, expired) = app
        .request("GET", &format!("/v1/invites/{code}"), &[], None)
        .await;
    assert_eq!(expired, unusable);
    let link = app.invite(&admin, &shelter, "link", "volunteer").await;
    app.send(
        "DELETE",
        &format!("/v1/invites/{link}"),
        &admin.secret,
        None,
    )
    .await;
    let (_, _, canceled) = app
        .request("GET", &format!("/v1/invites/{link}"), &[], None)
        .await;
    assert_eq!(canceled, unusable);
}

#[tokio::test]
async fn the_invite_page_says_when_the_shelter_is_confirmed() {
    let app = TestApp::new().await;
    let admin = app.actor("Admin").await;
    let shelter = app.confirmed_shelter(&admin, "Ginásio", ORIGIN).await;
    let code = app.invite(&admin, &shelter, "link", "volunteer").await;
    let (_, _, seen) = app
        .request("GET", &format!("/v1/invites/{code}"), &[], None)
        .await;
    assert_eq!(
        (&seen["confirmed"], &seen["kind"]),
        (&json!(true), &json!("link"))
    );
}

#[tokio::test]
async fn the_admin_lists_active_invites_and_who_came_in_by_link() {
    let (app, admin, shelter) = setup().await;
    let qr = app.invite(&admin, &shelter, "qr", "volunteer").await;
    let link_used = app.invite(&admin, &shelter, "link", "volunteer").await;
    let link_open = app.invite(&admin, &shelter, "link", "volunteer").await;
    let canceled = app.invite(&admin, &shelter, "qr", "volunteer").await;
    let expired = app.invite(&admin, &shelter, "qr", "volunteer").await;
    app.send(
        "DELETE",
        &format!("/v1/invites/{canceled}"),
        &admin.secret,
        None,
    )
    .await;
    expired_for(&app, &expired, 1).await;
    let joiner = app.join(&link_used, "Bia").await;

    let (status, listing) = app
        .send(
            "GET",
            &format!("/v1/shelters/{shelter}/invites"),
            &admin.secret,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{listing}");
    let mut codes: Vec<&str> = listing["invites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap())
        .collect();
    codes.sort();
    let mut want = vec![qr.as_str(), link_open.as_str()];
    want.sort();
    assert_eq!(codes, want);
    let first = &listing["invites"][0];
    assert!(
        first["url"]
            .as_str()
            .unwrap()
            .starts_with("https://taba.example.org/invite/")
    );
    assert!(
        first["kind"].is_string()
            && first["role"].is_string()
            && first["expires_at"].is_string()
            && first["created_at"].is_string()
    );
    assert_eq!(
        listing["joined_by_link"],
        json!([{ "volunteer_id": joiner.id, "name": "Bia", "invite_code": link_used, "joined_at": listing["joined_by_link"][0]["joined_at"] }])
    );
    // A QR does not show up there, and a volunteer cannot list.
    let by_qr = app.join(&qr, "Caio").await;
    let (_, listing) = app
        .send(
            "GET",
            &format!("/v1/shelters/{shelter}/invites"),
            &admin.secret,
            None,
        )
        .await;
    assert_eq!(listing["joined_by_link"].as_array().unwrap().len(), 1);
    assert_eq!(
        app.send(
            "GET",
            &format!("/v1/shelters/{shelter}/invites"),
            &by_qr.secret,
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

// ---- cancelling ----

#[tokio::test]
async fn an_admin_cancels_and_a_cancelled_invite_cannot_be_used() {
    let (app, admin, shelter) = setup().await;
    let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
    let stranger = app.actor("Ze").await;
    assert_eq!(
        app.send(
            "DELETE",
            &format!("/v1/invites/{code}"),
            &stranger.secret,
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.send(
            "DELETE",
            &format!("/v1/invites/{code}"),
            &admin.secret,
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(invite_doc(&app, &code).await.status, InviteStatus::Canceled);
    let (status, body) = app.accept(&code, "dev-bia", "tok-bia", "Bia").await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::GONE, Some("invite_invalid"))
    );
    // Cancelling it again, or something that never existed, is a 410.
    for gone in [code.as_str(), "nope"] {
        let (status, body) = app
            .send(
                "DELETE",
                &format!("/v1/invites/{gone}"),
                &admin.secret,
                None,
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::GONE, Some("invite_invalid"))
        );
    }
}

// ---- accepting ----

#[tokio::test]
async fn accepting_makes_a_member_a_session_and_uses_the_invite() {
    let (app, admin, shelter) = setup().await;
    let code = app.invite(&admin, &shelter, "qr", "admin").await;
    let (status, body) = app.accept(&code, "dev-bia", "tok-bia", "Bia").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["membership"],
        json!({ "shelter_id": shelter, "name": "Ginásio", "role": "admin", "confirmation": "unconfirmed" })
    );
    assert_eq!(body["volunteer"]["name"], "Bia");
    assert!(body["expires_at"].is_string());

    // The session works and shows the shelter.
    let secret = body["session"].as_str().unwrap();
    let (_, me) = app.get_authed("/v1/me", secret).await;
    assert_eq!(me["memberships"][0]["role"], "admin");

    let invite = invite_doc(&app, &code).await;
    assert_eq!(
        (invite.status, invite.used_by.as_deref()),
        (InviteStatus::Used, body["volunteer"]["id"].as_str())
    );
    assert!(invite.used_at.is_some());
    let member: MembershipDoc = store::get(
        &app.state.db,
        members::COLLECTION,
        &membership_id(&shelter, body["volunteer"]["id"].as_str().unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        (
            member.role,
            member.status,
            member.via,
            member.invite_code.as_deref(),
            member.invited_by.as_deref()
        ),
        (
            ShelterRole::Admin,
            MembershipStatus::Active,
            Via::Qr,
            Some(code.as_str()),
            Some(admin.id.as_str())
        )
    );
    let actions = audit_actions(&app.state.db).await;
    for expected in ["invite.accept", "session.create", "volunteer.create"] {
        assert!(
            actions.contains(&expected.to_string()),
            "{expected} in {actions:?}"
        );
    }
}

#[tokio::test]
async fn a_new_person_must_give_a_profile_and_keeps_the_capture_meanwhile() {
    let (app, admin, shelter) = setup().await;
    let code = app.invite(&admin, &shelter, "link", "volunteer").await;
    let capture_id = app.invite_capture("dev-bia", "tok-bia").await;
    let path = format!("/v1/invites/{code}/accept");
    let (status, body) = app
        .post(&path, "dev-bia", json!({ "capture_id": capture_id }))
        .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("profile_required"))
    );
    assert_eq!(invite_doc(&app, &code).await.status, InviteStatus::Active);

    let asked = app.unico.get_count();
    let profile = json!({ "name": "Bia", "show_phone": false, "language": "en" });
    let (status, body) = app
        .post(
            &path,
            "dev-bia",
            json!({ "capture_id": capture_id, "profile": profile }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        app.unico.get_count(),
        asked,
        "Unico is not asked again for a finished capture"
    );
    assert_eq!(body["volunteer"]["language"], "en");
}

#[tokio::test]
async fn someone_who_already_has_a_profile_only_does_the_selfie() {
    let (app, admin, shelter) = setup().await;
    let bia = app.actor("Bia").await;
    let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
    let capture_id = app.invite_capture(&bia.device, &bia.token).await;
    let (status, body) = app
        .post(
            &format!("/v1/invites/{code}/accept"),
            &bia.device,
            json!({ "capture_id": capture_id }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["volunteer"]["id"], json!(bia.id));
    // Her earlier session still works, and she is now in the shelter.
    let (_, me) = app.get_authed("/v1/me", &bia.secret).await;
    assert_eq!(me["memberships"][0]["shelter_id"], json!(shelter));
}

#[tokio::test]
async fn a_qr_used_twice_is_gone_the_second_time() {
    let (app, admin, shelter) = setup().await;
    let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
    let (first, _) = app.accept(&code, "dev-bia", "tok-bia", "Bia").await;
    assert_eq!(first, StatusCode::CREATED);
    let capture_id = app.invite_capture("dev-caio", "tok-caio").await;
    let profile = json!({ "name": "Caio", "show_phone": false, "language": "pt-BR" });
    let (status, body) = app
        .post(
            &format!("/v1/invites/{code}/accept"),
            "dev-caio",
            json!({ "capture_id": capture_id, "profile": profile }),
        )
        .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::GONE, Some("invite_invalid"))
    );
    // The refused person keeps an unused capture and no profile.
    let capture: crate::auth::CaptureDoc = store::get(&app.state.db, "captures", &capture_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!capture.consumed);
    assert_eq!(
        all_docs(&app.state.db, "volunteers").await.len(),
        2,
        "the admin and Bia"
    );
}

#[tokio::test]
async fn invites_expire_after_30_minutes_for_a_qr_and_24_hours_for_a_link() {
    let (app, admin, shelter) = setup().await;
    let qr = app.invite(&admin, &shelter, "qr", "volunteer").await;
    let link = app.invite(&admin, &shelter, "link", "volunteer").await;
    // 29 minutes in, the QR still works; 31 minutes in it is gone.
    let fresh = app.invite(&admin, &shelter, "qr", "volunteer").await;
    expired_for(&app, &fresh, -1).await;
    assert_eq!(
        app.accept(&fresh, "dev-ana", "tok-ana", "Ana").await.0,
        StatusCode::CREATED
    );
    expired_for(&app, &qr, 1).await;
    let (status, body) = app.accept(&qr, "dev-bia", "tok-bia", "Bia").await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::GONE, Some("invite_invalid"))
    );
    expired_for(&app, &link, 1).await;
    assert_eq!(
        app.accept(&link, "dev-caio", "tok-caio", "Caio").await.0,
        StatusCode::GONE
    );
}

#[tokio::test]
async fn of_many_people_racing_for_one_qr_exactly_one_gets_in() {
    let (app, admin, shelter) = setup().await;
    let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
    let mut handles = Vec::new();
    for n in 0..6 {
        let (app, code) = (app.clone(), code.clone());
        handles.push(tokio::spawn(async move {
            let (device, token) = (format!("dev-{n}"), format!("tok-{n}"));
            app.accept(&code, &device, &token, &format!("Pessoa {n}"))
                .await
        }));
    }
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    let joined = results
        .iter()
        .filter(|r| r.0 == StatusCode::CREATED)
        .count();
    let gone = results.iter().filter(|r| r.0 == StatusCode::GONE).count();
    assert_eq!((joined, gone), (1, 5), "{results:?}");
    assert_eq!(
        members::active_in_shelter(&app.state.db, &shelter)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        all_docs(&app.state.db, "sessions").await.len(),
        2,
        "the admin's and the winner's"
    );
}

#[tokio::test]
async fn the_same_person_racing_with_two_captures_gets_one_membership() {
    let (app, admin, shelter) = setup().await;
    let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
    let (a, b) = tokio::join!(
        app.accept(&code, "dev-bia", "tok-bia", "Bia"),
        app.accept(&code, "dev-bia2", "tok-bia", "Bia"),
    );
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::CREATED, StatusCode::GONE]);
}

#[tokio::test]
async fn someone_already_in_the_shelter_is_told_so_and_the_invite_stays() {
    let (app, admin, shelter) = setup().await;
    let first = app.invite(&admin, &shelter, "qr", "volunteer").await;
    let bia = app.join(&first, "Bia").await;
    let second = app.invite(&admin, &shelter, "qr", "admin").await;
    let capture_id = app.invite_capture(&bia.device, &bia.token).await;
    let (status, body) = app
        .post(
            &format!("/v1/invites/{second}/accept"),
            &bia.device,
            json!({ "capture_id": capture_id }),
        )
        .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::CONFLICT, Some("already_member"))
    );
    assert_eq!(invite_doc(&app, &second).await.status, InviteStatus::Active);
    // The admin themself too: the creator is a member already.
    let capture_id = app.invite_capture(&admin.device, &admin.token).await;
    let (status, _) = app
        .post(
            &format!("/v1/invites/{second}/accept"),
            &admin.device,
            json!({ "capture_id": capture_id }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_banned_person_cannot_accept_and_the_invite_is_not_spent() {
    let (app, admin, shelter) = setup().await;
    let bia = app.actor("Bia").await;
    let civil_defense = app.actor("DC").await;
    app.grant(&civil_defense, GlobalRole::CivilDefense).await;
    let (status, _) = app
        .send(
            "POST",
            &format!("/v1/volunteers/{}/ban", bia.id),
            &civil_defense.secret,
            Some(json!({ "reason": "fraude" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
    let capture_id = app.invite_capture(&bia.device, &bia.token).await;
    let (status, body) = app
        .post(
            &format!("/v1/invites/{code}/accept"),
            &bia.device,
            json!({ "capture_id": capture_id }),
        )
        .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::FORBIDDEN, Some("banned"))
    );
    assert_eq!(invite_doc(&app, &code).await.status, InviteStatus::Active);
    assert_eq!(
        members::active_in_shelter(&app.state.db, &shelter)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn someone_removed_returns_by_qr_but_not_by_link() {
    let (app, admin, shelter) = setup().await;
    let first = app.invite(&admin, &shelter, "qr", "volunteer").await;
    let bia = app.join(&first, "Bia").await;
    let (status, _) = app
        .send(
            "DELETE",
            &format!("/v1/shelters/{shelter}/members/{}", bia.id),
            &admin.secret,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let link = app.invite(&admin, &shelter, "link", "volunteer").await;
    let capture_id = app.invite_capture(&bia.device, &bia.token).await;
    let (status, body) = app
        .post(
            &format!("/v1/invites/{link}/accept"),
            &bia.device,
            json!({ "capture_id": capture_id }),
        )
        .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::FORBIDDEN, Some("removed_from_shelter"))
    );
    assert_eq!(
        invite_doc(&app, &link).await.status,
        InviteStatus::Active,
        "the link is not spent"
    );

    // A QR in person brings her back, this time as an admin if the QR says so.
    let qr = app.invite(&admin, &shelter, "qr", "admin").await;
    let capture_id = app.invite_capture(&bia.device, &bia.token).await;
    let (status, body) = app
        .post(
            &format!("/v1/invites/{qr}/accept"),
            &bia.device,
            json!({ "capture_id": capture_id }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let member: MembershipDoc = store::get(
        &app.state.db,
        members::COLLECTION,
        &membership_id(&shelter, &bia.id),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        (
            member.status,
            member.role,
            member.removed_by,
            member.removed_at
        ),
        (MembershipStatus::Active, ShelterRole::Admin, None, None)
    );
}

#[tokio::test]
async fn someone_who_left_can_return_by_link() {
    let (app, admin, shelter) = setup().await;
    let first = app.invite(&admin, &shelter, "qr", "volunteer").await;
    let bia = app.join(&first, "Bia").await;
    assert_eq!(
        app.send(
            "POST",
            &format!("/v1/shelters/{shelter}/leave"),
            &bia.secret,
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let link = app.invite(&admin, &shelter, "link", "volunteer").await;
    let capture_id = app.invite_capture(&bia.device, &bia.token).await;
    let (status, _) = app
        .post(
            &format!("/v1/invites/{link}/accept"),
            &bia.device,
            json!({ "capture_id": capture_id }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn the_capture_must_be_an_unused_invite_capture_of_this_device() {
    let (app, admin, shelter) = setup().await;
    let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
    let path = format!("/v1/invites/{code}/accept");
    let profile = json!({ "name": "Bia", "show_phone": false, "language": "pt-BR" });

    // A login capture does not do.
    let (_, login_capture) = app
        .post("/v1/captures", "dev-bia", json!({ "purpose": "login" }))
        .await;
    let login_id = login_capture["capture_id"].as_str().unwrap();
    let (status, body) = app
        .post(
            &path,
            "dev-bia",
            json!({ "capture_id": login_id, "profile": profile }),
        )
        .await;
    assert_eq!(
        (status, error_of(&body)),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("wrong_capture_purpose")
        )
    );

    // Another device's does not do either, nor an unknown one, nor a missing device header.
    let capture_id = app.invite_capture("dev-bia", "tok-bia").await;
    let (status, body) = app
        .post(
            &path,
            "dev-other",
            json!({ "capture_id": capture_id, "profile": profile }),
        )
        .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::FORBIDDEN, Some("capture_wrong_device"))
    );
    let (status, body) = app
        .post(
            &path,
            "dev-bia",
            json!({ "capture_id": new_id(), "profile": profile }),
        )
        .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::NOT_FOUND, Some("capture_not_found"))
    );
    let (status, _, body) = app
        .request(
            "POST",
            &path,
            &[],
            Some(json!({ "capture_id": capture_id })),
        )
        .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::BAD_REQUEST, Some("device_id_required"))
    );

    // Still pending at Unico: the app tries again later.
    let pending = app
        .post("/v1/captures", "dev-caio", json!({ "purpose": "invite" }))
        .await
        .1;
    let (status, body) = app
        .post(
            &path,
            "dev-caio",
            json!({ "capture_id": pending["capture_id"], "profile": profile }),
        )
        .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::ACCEPTED, Some("capture_pending"))
    );

    // And once used, it is used.
    let (status, _) = app
        .post(
            &path,
            "dev-bia",
            json!({ "capture_id": capture_id, "profile": profile }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let second = app.invite(&admin, &shelter, "qr", "volunteer").await;
    let (status, body) = app
        .post(
            &format!("/v1/invites/{second}/accept"),
            "dev-bia",
            json!({ "capture_id": capture_id }),
        )
        .await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::CONFLICT, Some("capture_consumed"))
    );
}

#[tokio::test]
async fn an_invite_capture_cannot_open_a_session_by_login() {
    let app = TestApp::new().await;
    let capture_id = app.invite_capture("dev-bia", "tok-bia").await;
    let profile = json!({ "name": "Bia", "show_phone": false, "language": "pt-BR" });
    let (status, body) = app
        .post(
            "/v1/sessions",
            "dev-bia",
            json!({ "capture_id": capture_id, "profile": profile }),
        )
        .await;
    assert_eq!(
        (status, error_of(&body)),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("wrong_capture_purpose")
        )
    );
    assert!(all_docs(&app.state.db, "volunteers").await.is_empty());
}
