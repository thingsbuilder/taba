use super::*;
use crate::auth::Volunteer;
use crate::exits::fixtures::{child, load, person, put};
use crate::people::{ChildStatus, GuardianLink, Level, PersonDoc};
use crate::shelters::ShelterDoc;
use crate::store;
use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, new_id, north_of_origin};
use crate::volunteers::GlobalRole;
use axum::http::StatusCode;
use serde_json::{Value, json};

struct World {
    app: TestApp,
    unconfirmed: String,
    confirmed: String,
    /// (label, actor); the labels name the matrix columns.
    actors: Vec<(&'static str, Actor)>,
}

impl World {
    async fn new() -> Self {
        let app = TestApp::new().await;
        let admin_unconfirmed = app.actor("AdminUnconfirmed").await;
        let unconfirmed = app
            .create_shelter(&admin_unconfirmed, "Nao confirmado", ORIGIN)
            .await;
        let admin_confirmed = app.actor("AdminConfirmed").await;
        let confirmed = app
            .confirmed_shelter(&admin_confirmed, "Confirmado", north_of_origin(200_000.0))
            .await;
        let code = app
            .invite(&admin_unconfirmed, &unconfirmed, "qr", "volunteer")
            .await;
        let volunteer_unconfirmed = app.join(&code, "VolunteerUnconfirmed").await;
        let code = app
            .invite(&admin_confirmed, &confirmed, "qr", "volunteer")
            .await;
        let volunteer_confirmed = app.join(&code, "VolunteerConfirmed").await;
        let mut actors = vec![
            ("stranger", app.actor("Stranger").await),
            ("volunteer_unconfirmed", volunteer_unconfirmed),
            ("admin_unconfirmed", admin_unconfirmed),
            ("volunteer_confirmed", volunteer_confirmed),
            ("admin_confirmed", admin_confirmed),
        ];
        for (label, role) in [
            ("validator", GlobalRole::Validator),
            ("mediator", GlobalRole::Mediator),
            ("civil_defense", GlobalRole::CivilDefense),
            ("protection", GlobalRole::Protection),
        ] {
            let actor = app.actor(label).await;
            app.grant(&actor, role).await;
            actors.push((label, actor));
        }
        Self {
            app,
            unconfirmed,
            confirmed,
            actors,
        }
    }

    fn actor(&self, label: &str) -> &Actor {
        &self.actors.iter().find(|(l, _)| *l == label).unwrap().1
    }

    async fn guardian(
        &self,
        link: &str,
        as_who: Option<&str>,
        child_id: &str,
        guardian_id: &str,
    ) -> (StatusCode, Value) {
        let path = format!("/v1/people/{child_id}/guardians");
        let body = Some(json!({ "guardian_id": guardian_id, "link": link }));
        match as_who {
            Some(label) => {
                self.app
                    .send("POST", &path, &self.actor(label).secret, body)
                    .await
            }
            None => {
                let (status, _, json) = self.app.request("POST", &path, &[], body).await;
                (status, json)
            }
        }
    }

    async fn verified_guardian(&self) -> String {
        let id = new_id();
        put(&self.app, &id, &person(Level::Verified, None)).await;
        id
    }
}

fn code_of(body: &Value) -> &str {
    body["error"].as_str().unwrap_or("")
}

#[tokio::test]
async fn a_declared_guardian_is_recorded_without_who_checked() {
    let w = World::new().await;
    let guardian = w.verified_guardian().await;
    let kid = new_id();
    put(
        &w.app,
        &kid,
        &child(&w.unconfirmed, ChildStatus::WithGuardian, vec![]),
    )
    .await;
    let (status, body) = w
        .guardian("declared", Some("volunteer_unconfirmed"), &kid, &guardian)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = load(&w.app, &kid).await;
    assert_eq!(stored.guardians.len(), 1);
    assert_eq!(stored.guardians[0].person_id, guardian);
    assert_eq!(stored.guardians[0].link, GuardianLink::Declared);
    assert_eq!(stored.guardians[0].checked_by, None);
    assert!(stored.guardians[0].checked_at.is_none());
}

#[tokio::test]
async fn a_document_check_records_who_and_when_and_replaces_the_declared_link() {
    let w = World::new().await;
    let guardian = w.verified_guardian().await;
    let kid = new_id();
    put(
        &w.app,
        &kid,
        &child(&w.unconfirmed, ChildStatus::WithGuardian, vec![]),
    )
    .await;
    w.guardian("declared", Some("admin_unconfirmed"), &kid, &guardian)
        .await;
    let (status, body) = w
        .guardian(
            "document_checked",
            Some("volunteer_unconfirmed"),
            &kid,
            &guardian,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = load(&w.app, &kid).await;
    assert_eq!(stored.guardians.len(), 1);
    assert_eq!(stored.guardians[0].link, GuardianLink::DocumentChecked);
    assert_eq!(
        stored.guardians[0].checked_by.as_deref(),
        Some(w.actor("volunteer_unconfirmed").id.as_str())
    );
    assert!(stored.guardians[0].checked_at.is_some());
    assert_eq!(body["guardians"][0]["link"], "document_checked");
    assert!(
        all_docs(&w.app.state.db, "audit_log")
            .await
            .iter()
            .any(|e| e["action"] == "person.guardian" && e["legal_basis"] == "child_protection")
    );
}

#[tokio::test]
async fn a_guardian_must_exist_and_be_verified() {
    let w = World::new().await;
    let kid = new_id();
    put(
        &w.app,
        &kid,
        &child(&w.unconfirmed, ChildStatus::WithGuardian, vec![]),
    )
    .await;
    let assisted = new_id();
    put(&w.app, &assisted, &person(Level::Assisted, None)).await;
    let declined = new_id();
    put(&w.app, &declined, &person(Level::NoBiometrics, None)).await;
    for guardian in [assisted, declined, new_id()] {
        for link in ["declared", "document_checked"] {
            let (status, body) = w
                .guardian(link, Some("admin_unconfirmed"), &kid, &guardian)
                .await;
            assert_eq!(
                (status, code_of(&body)),
                (StatusCode::UNPROCESSABLE_ENTITY, "guardian_not_verified")
            );
        }
    }
    assert!(load(&w.app, &kid).await.guardians.is_empty());
}

#[tokio::test]
async fn guardians_are_for_children_and_not_for_themselves() {
    let w = World::new().await;
    let guardian = w.verified_guardian().await;
    let adult = new_id();
    put(
        &w.app,
        &adult,
        &person(Level::Verified, Some(&w.unconfirmed)),
    )
    .await;
    let (status, body) = w
        .guardian("declared", Some("admin_unconfirmed"), &adult, &guardian)
        .await;
    assert_eq!(
        (status, code_of(&body)),
        (StatusCode::UNPROCESSABLE_ENTITY, "not_a_child")
    );
    let kid = new_id();
    let mut doc = child(&w.unconfirmed, ChildStatus::WithGuardian, vec![]);
    doc.level = Level::Verified;
    put(&w.app, &kid, &doc).await;
    let (status, body) = w
        .guardian("declared", Some("admin_unconfirmed"), &kid, &kid)
        .await;
    assert_eq!(
        (status, code_of(&body)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_guardian")
    );
    let (status, _) = w
        .guardian("declared", Some("admin_unconfirmed"), &new_id(), &guardian)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn guardian_matrix() {
    let w = World::new().await;
    let guardian = w.verified_guardian().await;
    for shelter in [&w.unconfirmed, &w.confirmed] {
        let members: &[&str] = if *shelter == w.unconfirmed {
            &["volunteer_unconfirmed", "admin_unconfirmed"]
        } else {
            &["volunteer_confirmed", "admin_confirmed"]
        };
        for who in [
            None,
            Some("stranger"),
            Some("volunteer_unconfirmed"),
            Some("admin_unconfirmed"),
            Some("volunteer_confirmed"),
            Some("admin_confirmed"),
            Some("validator"),
            Some("mediator"),
            Some("civil_defense"),
            Some("protection"),
        ] {
            let kid = new_id();
            put(
                &w.app,
                &kid,
                &child(shelter, ChildStatus::WithGuardian, vec![]),
            )
            .await;
            let (status, body) = w.guardian("document_checked", who, &kid, &guardian).await;
            let expected = match who {
                None => StatusCode::UNAUTHORIZED,
                Some(label)
                    if members.contains(&label)
                        || label == "civil_defense"
                        || label == "protection" =>
                {
                    StatusCode::OK
                }
                Some(_) => StatusCode::FORBIDDEN,
            };
            assert_eq!(status, expected, "{who:?} in {shelter}: {body}");
        }
    }
}

// ---- child_separated (criterion 42) ----

async fn raise(w: &World, shelter_id: &str, kid: &str, status: ChildStatus) {
    let shelter: ShelterDoc = store::get(&w.app.state.db, crate::shelters::COLLECTION, shelter_id)
        .await
        .unwrap()
        .unwrap();
    store::transact(&w.app.state.db, |db, tx| {
        let (shelter_id, shelter, kid) = (shelter_id.to_string(), shelter.clone(), kid.to_string());
        Box::pin(async move {
            raise_child_separated(&db, tx, &kid, &shelter_id, &shelter, status).await
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn an_unaccompanied_child_alerts_with_the_reference_phones() {
    let w = World::new().await;
    let shelter: ShelterDoc =
        store::get(&w.app.state.db, crate::shelters::COLLECTION, &w.unconfirmed)
            .await
            .unwrap()
            .unwrap();
    let event_id = shelter.event_id.clone().unwrap();
    let (status, body) = w
        .app
        .send(
            "PATCH",
            &format!("/v1/events/{event_id}"),
            &w.actor("civil_defense").secret,
            Some(json!({ "reference_contacts": { "civil_defense_phone": "199", "child_protection_phone": "+55 51 3333-0000" } })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let kid = new_id();
    raise(&w, &w.unconfirmed, &kid, ChildStatus::WithGuardian).await;
    assert!(all_docs(&w.app.state.db, "alerts").await.is_empty());
    raise(&w, &w.unconfirmed, &kid, ChildStatus::Unaccompanied).await;
    raise(&w, &w.unconfirmed, &kid, ChildStatus::Separated).await;

    let alerts = all_docs(&w.app.state.db, "alerts").await;
    assert_eq!(alerts.len(), 2);
    let alert = alerts
        .iter()
        .find(|a| a["detail"]["child_status"] == "unaccompanied")
        .unwrap();
    assert_eq!(alert["kind"], "child_separated");
    assert_eq!(alert["target_id"], json!(kid));
    assert_eq!(alert["for_protection"], true);
    assert_eq!(alert["for_civil_defense"], true);
    assert_eq!(alert["informational"], false);
    assert_eq!(alert["shelter_ids"], json!([w.unconfirmed]));
    assert_eq!(
        alert["detail"]["reference_contacts"],
        json!({ "civil_defense_phone": "199", "child_protection_phone": "+55 51 3333-0000" })
    );

    // Who is told: protection, the Civil Defense and the admins of that shelter only.
    for (label, sees) in [
        ("protection", true),
        ("civil_defense", true),
        ("admin_unconfirmed", true),
        ("admin_confirmed", false),
    ] {
        let (status, listed) = w
            .app
            .send("GET", "/v1/alerts", &w.actor(label).secret, None)
            .await;
        if sees {
            assert_eq!(status, StatusCode::OK, "{label}");
            assert_eq!(listed["alerts"].as_array().unwrap().len(), 2, "{label}");
            assert_eq!(
                listed["alerts"][0]["detail"]["reference_contacts"]["child_protection_phone"],
                "+55 51 3333-0000"
            );
        } else {
            let empty = status == StatusCode::FORBIDDEN
                || listed["alerts"].as_array().is_some_and(Vec::is_empty);
            assert!(empty, "{label}: {listed}");
        }
    }
    // A plain volunteer opens the box (for their own search alerts) and finds it empty.
    let (status, listed) = w
        .app
        .send(
            "GET",
            "/v1/alerts",
            &w.actor("volunteer_unconfirmed").secret,
            None,
        )
        .await;
    assert_eq!(
        (status, listed["alerts"].as_array().map(Vec::len)),
        (StatusCode::OK, Some(0))
    );
}

#[tokio::test]
async fn without_reference_phones_the_alert_still_goes_out() {
    let w = World::new().await;
    raise(&w, &w.confirmed, &new_id(), ChildStatus::Separated).await;
    let alerts = all_docs(&w.app.state.db, "alerts").await;
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0]["detail"]["reference_contacts"], json!({}));
}

#[tokio::test]
async fn only_protection_and_the_civil_defense_may_change_the_status() {
    let w = World::new().await;
    let volunteer = |label: &str| Volunteer {
        id: w.actor(label).id.clone(),
        doc: crate::volunteers::VolunteerDoc::from_profile(
            &serde_json::from_value(
                json!({ "name": "x", "show_phone": false, "language": "pt-BR" }),
            )
            .unwrap(),
        )
        .unwrap(),
        memberships: vec![],
        session_id: String::new(),
    };
    let with_role = |role| {
        let mut v = volunteer("stranger");
        v.doc.global_roles.push(role);
        v
    };
    assert!(can_change_status(&with_role(GlobalRole::Protection)));
    assert!(can_change_status(&with_role(GlobalRole::CivilDefense)));
    assert!(!can_change_status(&with_role(GlobalRole::Validator)));
    assert!(!can_change_status(&with_role(GlobalRole::Mediator)));
    assert!(!can_change_status(&volunteer("stranger")));
}

// ---- the child state rule, without a database ----

fn adult() -> PersonDoc {
    person(Level::Assisted, Some("s"))
}

fn kid(status: ChildStatus) -> PersonDoc {
    child("s", status, vec![])
}

fn change(is_child: Option<bool>, child_status: Option<ChildStatus>) -> ChildChange {
    ChildChange {
        is_child,
        child_status,
    }
}

#[test]
fn anyone_may_declare_a_child_but_only_protection_lowers_or_changes_it() {
    use ChildStatus::*;
    // declaring raises the protection: allowed to anyone, alert owed for separated
    let mut p = adult();
    let alert = change_child_state(&mut p, change(Some(true), None), false).unwrap();
    assert_eq!((p.is_child, p.child_status, alert), (true, None, None));
    // a status for a person who had none is a change of status: protection only
    let error = change_child_state(&mut p, change(None, Some(Separated)), false).unwrap_err();
    assert_eq!(error.code, "protection_role_required");
    assert_eq!(p.child_status, None);
    let alert = change_child_state(&mut p, change(None, Some(Separated)), true).unwrap();
    assert_eq!(alert, Some(Separated));
    // clearing the condition, or changing the status, is refused to everyone else...
    for ask in [change(Some(false), None), change(None, Some(WithGuardian))] {
        let mut p = kid(Separated);
        let error = change_child_state(&mut p, ask, false).unwrap_err();
        assert_eq!(error.code, "protection_role_required", "{ask:?}");
        assert_eq!((p.is_child, p.child_status), (true, Some(Separated)));
    }
    // ...and allowed to protection, the two fields moving together
    let mut p = kid(Separated);
    assert_eq!(
        change_child_state(&mut p, change(Some(false), None), true).unwrap(),
        None
    );
    assert_eq!((p.is_child, p.child_status), (false, None));
    let mut p = kid(Separated);
    assert_eq!(
        change_child_state(&mut p, change(None, Some(WithGuardian)), true).unwrap(),
        None
    );
    assert_eq!((p.is_child, p.child_status), (true, Some(WithGuardian)));
}

#[test]
fn repeating_a_child_state_changes_nothing_and_owes_no_alert() {
    use ChildStatus::*;
    let mut p = kid(Unaccompanied);
    for may_alter in [false, true] {
        let same = change(Some(true), Some(Unaccompanied));
        assert_eq!(change_child_state(&mut p, same, may_alter).unwrap(), None);
        assert_eq!(
            (p.is_child, p.child_status),
            (true, Some(Unaccompanied)),
            "{may_alter}"
        );
    }
    // clearing what is not set is not a lowering
    let mut p = adult();
    assert_eq!(
        change_child_state(&mut p, change(Some(false), None), false).unwrap(),
        None
    );
    // a status and "not a child" together is a contradiction
    let error = change_child_state(&mut p, change(Some(false), Some(Separated)), true).unwrap_err();
    assert_eq!(error.code, "invalid_person");
}

#[test]
fn a_fusion_never_lowers_the_protection() {
    use ChildStatus::*;
    let alert = |new: PersonDoc, old: PersonDoc| {
        let mut new = new;
        let alert = merge_child_state(&mut new, &old);
        (new.is_child, new.child_status, alert)
    };
    assert_eq!(
        alert(adult(), kid(Separated)),
        (true, Some(Separated), Some(Separated))
    );
    assert_eq!(
        alert(kid(WithGuardian), kid(Unaccompanied)),
        (true, Some(Unaccompanied), Some(Unaccompanied))
    );
    // the more protected record wins whichever side it was on, and nothing new is owed
    assert_eq!(
        alert(kid(Separated), kid(WithGuardian)),
        (true, Some(Separated), None)
    );
    assert_eq!(
        alert(kid(Separated), adult()),
        (true, Some(Separated), None)
    );
    assert_eq!(alert(adult(), adult()), (false, None, None));
}
