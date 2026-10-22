//! The permission matrix of spec 2.3 for every route phase 2 exposes, against an unconfirmed
//! and a confirmed shelter. One row per operation: who may do it, and everyone else gets 403
//! (401 without a session). Operations that change the state the next row depends on are only
//! tried by the people who may NOT do them; the affirmative side has its own tests.

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::test_support::{Actor, ORIGIN, TestApp, north_of_origin};
use crate::volunteers::GlobalRole;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Who {
    Anonymous,
    Stranger,
    VolunteerUnconfirmed,
    AdminUnconfirmed,
    VolunteerConfirmed,
    AdminConfirmed,
    Validator,
    CivilDefense,
}

const EVERYONE: [Who; 8] = [
    Who::Anonymous,
    Who::Stranger,
    Who::VolunteerUnconfirmed,
    Who::AdminUnconfirmed,
    Who::VolunteerConfirmed,
    Who::AdminConfirmed,
    Who::Validator,
    Who::CivilDefense,
];

struct World {
    app: TestApp,
    unconfirmed: String,
    confirmed: String,
    actors: Vec<(Who, Actor)>,
    /// A volunteer of each shelter that the rows may promote, remove or leave alone.
    target_unconfirmed: Actor,
    target_confirmed: Actor,
}

async fn world() -> World {
    let app = TestApp::new().await;
    let admin_unconfirmed = app.actor("AdminUnconfirmed").await;
    let unconfirmed = app
        .create_shelter(&admin_unconfirmed, "Nao confirmado", ORIGIN)
        .await;
    let admin_confirmed = app.actor("AdminConfirmed").await;
    let confirmed = app
        .confirmed_shelter(&admin_confirmed, "Confirmado", north_of_origin(200_000.0))
        .await;
    let volunteer = |shelter: String, admin: Actor, name: &'static str| {
        let app = app.clone();
        async move {
            app.join(&app.invite(&admin, &shelter, "qr", "volunteer").await, name)
                .await
        }
    };
    let volunteer_unconfirmed = volunteer(
        unconfirmed.clone(),
        admin_unconfirmed.clone(),
        "VolunteerUnconfirmed",
    )
    .await;
    let volunteer_confirmed = volunteer(
        confirmed.clone(),
        admin_confirmed.clone(),
        "VolunteerConfirmed",
    )
    .await;
    let target_unconfirmed = volunteer(
        unconfirmed.clone(),
        admin_unconfirmed.clone(),
        "TargetUnconfirmed",
    )
    .await;
    let target_confirmed = volunteer(
        confirmed.clone(),
        admin_confirmed.clone(),
        "TargetConfirmed",
    )
    .await;
    let validator = app.actor("Validator").await;
    app.grant(&validator, GlobalRole::Validator).await;
    let civil_defense = app.actor("CivilDefense").await;
    app.grant(&civil_defense, GlobalRole::CivilDefense).await;
    let stranger = app.actor("Stranger").await;
    let actors = vec![
        (Who::Stranger, stranger),
        (Who::VolunteerUnconfirmed, volunteer_unconfirmed),
        (Who::AdminUnconfirmed, admin_unconfirmed),
        (Who::VolunteerConfirmed, volunteer_confirmed),
        (Who::AdminConfirmed, admin_confirmed),
        (Who::Validator, validator),
        (Who::CivilDefense, civil_defense),
    ];
    World {
        app,
        unconfirmed,
        confirmed,
        actors,
        target_unconfirmed,
        target_confirmed,
    }
}

impl World {
    fn actor(&self, who: Who) -> Option<&Actor> {
        self.actors.iter().find(|(w, _)| *w == who).map(|(_, a)| a)
    }

    async fn call(&self, who: Who, method: &str, path: &str, body: &Option<Value>) -> StatusCode {
        match self.actor(who) {
            Some(actor) => {
                self.app
                    .send(method, path, &actor.secret, body.clone())
                    .await
                    .0
            }
            None => self.app.request(method, path, &[], body.clone()).await.0,
        }
    }

    /// Checks one row: `allowed` succeed (unless the row only tests refusals), the rest are refused.
    async fn row(
        &self,
        label: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
        allowed: &[Who],
        refusals_only: bool,
    ) {
        for who in EVERYONE {
            let is_allowed = allowed.contains(&who);
            if is_allowed && refusals_only {
                continue;
            }
            let status = self.call(who, method, path, &body).await;
            if is_allowed {
                assert!(
                    status.is_success(),
                    "{label}: {who:?} should be allowed, got {status}"
                );
            } else if who == Who::Anonymous {
                assert_eq!(status, StatusCode::UNAUTHORIZED, "{label}: {who:?}");
            } else {
                assert_eq!(status, StatusCode::FORBIDDEN, "{label}: {who:?}");
            }
        }
    }
}

use Who::*;

#[tokio::test]
async fn a_shelter_is_edited_only_by_its_admin_and_its_capacity_also_by_the_civil_defense() {
    let w = world().await;
    for (shelter, admin) in [
        (&w.unconfirmed, AdminUnconfirmed),
        (&w.confirmed, AdminConfirmed),
    ] {
        let path = format!("/v1/shelters/{shelter}");
        w.row(
            "name",
            "PATCH",
            &path,
            Some(json!({ "name": "Outro nome" })),
            &[admin],
            false,
        )
        .await;
        w.row(
            "address",
            "PATCH",
            &path,
            Some(json!({ "address": { "city": "Canoas", "state": "RS" } })),
            &[admin],
            false,
        )
        .await;
        w.row(
            "capacity",
            "PATCH",
            &path,
            Some(json!({ "capacity": 120 })),
            &[admin, CivilDefense],
            false,
        )
        .await;
    }
}

#[tokio::test]
async fn inviting_and_listing_members_belong_to_the_admin_and_listing_also_to_members_and_the_civil_defense()
 {
    let w = world().await;
    for (shelter, admin, volunteer) in [
        (&w.unconfirmed, AdminUnconfirmed, VolunteerUnconfirmed),
        (&w.confirmed, AdminConfirmed, VolunteerConfirmed),
    ] {
        let invites = format!("/v1/shelters/{shelter}/invites");
        w.row(
            "invite a volunteer",
            "POST",
            &invites,
            Some(json!({ "kind": "qr", "role": "volunteer" })),
            &[admin],
            false,
        )
        .await;
        w.row(
            "invite an admin",
            "POST",
            &invites,
            Some(json!({ "kind": "qr", "role": "admin" })),
            &[admin],
            false,
        )
        .await;
        w.row(
            "invite by link",
            "POST",
            &invites,
            Some(json!({ "kind": "link", "role": "volunteer" })),
            &[admin],
            false,
        )
        .await;
        w.row("list invites", "GET", &invites, None, &[admin], false)
            .await;
        w.row(
            "list members",
            "GET",
            &format!("/v1/shelters/{shelter}/members"),
            None,
            &[volunteer, admin, CivilDefense],
            false,
        )
        .await;
    }
}

#[tokio::test]
async fn promoting_removing_and_leaving_follow_the_matrix() {
    let w = world().await;
    for (shelter, admin, target) in [
        (&w.unconfirmed, AdminUnconfirmed, &w.target_unconfirmed),
        (&w.confirmed, AdminConfirmed, &w.target_confirmed),
    ] {
        let member = format!("/v1/shelters/{shelter}/members/{}", target.id);
        w.row(
            "promote",
            "PATCH",
            &member,
            Some(json!({ "role": "admin" })),
            &[admin],
            true,
        )
        .await;
        w.row(
            "remove a volunteer",
            "DELETE",
            &member,
            None,
            &[admin, CivilDefense],
            true,
        )
        .await;
        // Everyone who is not a member is refused when trying to leave.
        let leave = format!("/v1/shelters/{shelter}/leave");
        let members: &[Who] = if shelter == &w.unconfirmed {
            &[VolunteerUnconfirmed, AdminUnconfirmed]
        } else {
            &[VolunteerConfirmed, AdminConfirmed]
        };
        w.row("leave", "POST", &leave, None, members, true).await;
    }
}

#[tokio::test]
async fn closing_confirming_and_revoking_follow_the_matrix() {
    let w = world().await;
    for (shelter, admin) in [
        (&w.unconfirmed, AdminUnconfirmed),
        (&w.confirmed, AdminConfirmed),
    ] {
        w.row(
            "close",
            "POST",
            &format!("/v1/shelters/{shelter}/close"),
            None,
            &[admin, CivilDefense],
            true,
        )
        .await;
        w.row(
            "confirm",
            "POST",
            &format!("/v1/shelters/{shelter}/confirm"),
            None,
            &[Validator, CivilDefense],
            true,
        )
        .await;
        w.row(
            "revoke",
            "POST",
            &format!("/v1/shelters/{shelter}/revoke"),
            Some(json!({ "reason": "teste" })),
            &[Validator, CivilDefense],
            true,
        )
        .await;
    }
}

#[tokio::test]
async fn the_event_is_renamed_by_an_admin_of_it_or_the_civil_defense_and_its_contacts_by_a_validator_or_the_civil_defense()
 {
    let w = world().await;
    for (shelter, admin) in [
        (&w.unconfirmed, AdminUnconfirmed),
        (&w.confirmed, AdminConfirmed),
    ] {
        let stored: crate::shelters::ShelterDoc =
            crate::store::get(&w.app.state.db, crate::shelters::COLLECTION, shelter)
                .await
                .unwrap()
                .unwrap();
        let path = format!("/v1/events/{}", stored.event_id.unwrap());
        w.row(
            "rename the event",
            "PATCH",
            &path,
            Some(json!({ "name": "Cheia" })),
            &[admin, CivilDefense],
            false,
        )
        .await;
        w.row(
            "event contacts",
            "PATCH",
            &path,
            Some(json!({ "reference_contacts": { "civil_defense_phone": "199" } })),
            &[Validator, CivilDefense],
            false,
        )
        .await;
    }
}

#[tokio::test]
async fn in_person_endorsement_is_for_admins_and_only_a_confirmed_shelter_can_endorse() {
    let w = world().await;
    // The code is made by the admin of the shelter to be confirmed.
    let code_path = format!("/v1/shelters/{}/endorsements/in-person/code", w.unconfirmed);
    let here = north_of_origin(0.0);
    w.row(
        "make a code",
        "POST",
        &code_path,
        Some(json!({ "lat": here.0, "lon": here.1, "accuracy": 10.0 })),
        &[AdminUnconfirmed],
        false,
    )
    .await;

    // Reading it: the admin of the unconfirmed shelter cannot endorse, the confirmed one's can.
    let (_, code) = w
        .app
        .send(
            "POST",
            &code_path,
            &w.actor(AdminUnconfirmed).unwrap().secret,
            Some(json!({ "lat": here.0, "lon": here.1, "accuracy": 10.0 })),
        )
        .await;
    let endorse = |from: &str| {
        Some(
            json!({ "code": code["code"], "endorser_shelter_id": from, "lat": here.0, "lon": here.1, "accuracy": 10.0 }),
        )
    };
    for (who, from) in [
        (Stranger, &w.confirmed),
        (VolunteerConfirmed, &w.confirmed),
        (AdminUnconfirmed, &w.unconfirmed),
        (VolunteerUnconfirmed, &w.unconfirmed),
        (Validator, &w.confirmed),
        (CivilDefense, &w.confirmed),
    ] {
        let status = w
            .call(who, "POST", "/v1/endorsements/in-person", &endorse(from))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{who:?}");
    }
    assert_eq!(
        w.call(
            Anonymous,
            "POST",
            "/v1/endorsements/in-person",
            &endorse(&w.confirmed)
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        w.call(
            AdminConfirmed,
            "POST",
            "/v1/endorsements/in-person",
            &endorse(&w.confirmed)
        )
        .await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn alerts_and_bans_follow_the_matrix() {
    let w = world().await;
    w.row(
        "read alerts",
        "GET",
        "/v1/alerts",
        None,
        // Members of a shelter open the box too: they get the alerts about their own searches.
        &[
            VolunteerUnconfirmed,
            AdminUnconfirmed,
            VolunteerConfirmed,
            AdminConfirmed,
            Validator,
            CivilDefense,
        ],
        false,
    )
    .await;
    let ban = format!("/v1/volunteers/{}/ban", w.target_unconfirmed.id);
    w.row(
        "ban",
        "POST",
        &ban,
        Some(json!({ "reason": "teste" })),
        &[CivilDefense],
        true,
    )
    .await;
}

#[tokio::test]
async fn anyone_signed_in_creates_a_shelter_and_reads_the_basics_of_any() {
    let w = world().await;
    for (n, who) in [
        Stranger,
        VolunteerUnconfirmed,
        AdminConfirmed,
        Validator,
        CivilDefense,
    ]
    .into_iter()
    .enumerate()
    {
        let body = crate::test_support::shelter_body(
            &crate::test_support::new_id(),
            "Novo",
            north_of_origin(1_000_000.0 + 50_000.0 * n as f64),
        );
        assert_eq!(
            w.call(who, "POST", "/v1/shelters", &Some(body)).await,
            StatusCode::CREATED,
            "{who:?}"
        );
    }
    let path = format!("/v1/shelters/{}", w.unconfirmed);
    for who in EVERYONE.into_iter().filter(|w| *w != Anonymous) {
        assert_eq!(
            w.call(who, "GET", &path, &None).await,
            StatusCode::OK,
            "{who:?}"
        );
    }
    assert_eq!(
        w.call(Anonymous, "GET", &path, &None).await,
        StatusCode::UNAUTHORIZED
    );
}

// ---- Phase 6 (stage 0): every route is registered and asks for a session ----
// Each owner replaces the rows of their routes with the real matrix of spec 2.3 (who may, who
// gets 403) in the file of their module, and removes them from here.

#[tokio::test]
async fn phase_6_routes_are_registered_and_ask_for_a_session() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    for (method, path) in [
        ("GET", "/v1/dashboard/events"),
        ("GET", "/v1/dashboard/events/e1"),
        ("GET", "/v1/dashboard/events/e1/daily.csv"),
        ("GET", "/v1/shelters/s1/export.csv"),
    ] {
        let (status, _, answer) = app.request(method, path, &[], None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_eq!(answer["error"], "unauthorized", "{method} {path}");
        let (status, answer) = app.send(method, path, &ana.secret, None).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{method} {path}");
        assert_eq!(answer["error"], "not_implemented", "{method} {path}");
    }
}
