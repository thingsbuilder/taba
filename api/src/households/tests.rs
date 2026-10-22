use super::*;
use crate::audit::LegalBasis;
use crate::people::{self, PersonDoc};
use crate::people::{Level, Origin};
use crate::store::{self, now};
use crate::test_support::{Actor, ORIGIN, TestApp, new_id, north_of_origin};
use crate::volunteers::GlobalRole;
use axum::http::StatusCode;
use serde_json::{Value, json};

/// Shelter `a` is unconfirmed, shelter `b` is confirmed (matrix 2.3 columns NC and C).
pub struct World {
    pub app: TestApp,
    pub a: String,
    pub b: String,
    pub admin_a: Actor,
    pub vol_a: Actor,
    pub admin_b: Actor,
    pub vol_b: Actor,
    pub outsider: Actor,
    pub validator: Actor,
    pub civil_defense: Actor,
}

pub async fn world() -> World {
    let app = TestApp::new().await;
    let admin_a = app.actor("AdminA").await;
    let a = app.create_shelter(&admin_a, "Abrigo A", ORIGIN).await;
    let admin_b = app.actor("AdminB").await;
    let b = app
        .confirmed_shelter(&admin_b, "Abrigo B", north_of_origin(200_000.0))
        .await;
    let vol_a = app
        .join(&app.invite(&admin_a, &a, "qr", "volunteer").await, "VolA")
        .await;
    let vol_b = app
        .join(&app.invite(&admin_b, &b, "qr", "volunteer").await, "VolB")
        .await;
    let validator = app.actor("Validator").await;
    app.grant(&validator, GlobalRole::Validator).await;
    let civil_defense = app.actor("CivilDefense").await;
    app.grant(&civil_defense, GlobalRole::CivilDefense).await;
    let outsider = app.actor("Outsider").await;
    World {
        app,
        a,
        b,
        admin_a,
        vol_a,
        admin_b,
        vol_b,
        outsider,
        validator,
        civil_defense,
    }
}

/// A person written straight to the store: the people routes are another module's.
pub async fn person(app: &TestApp, name: &str, shelter: Option<&str>) -> String {
    let id = new_id();
    let at = now();
    let doc = PersonDoc {
        id: None,
        level: Level::NoBiometrics,
        biometrics_declined: true,
        former_ids: vec![],
        name: Some(name.to_string()),
        social_name: None,
        wristband: None,
        birth_year: None,
        is_child: false,
        child_status: None,
        guardians: vec![],
        guardian_ids: vec![],
        needs: Default::default(),
        vulnerabilities: Default::default(),
        household_id: None,
        current_shelter_id: shelter.map(str::to_string),
        stays: vec![],
        event_ids: vec![],
        rescue_ids: vec![],
        thumbnail_encrypted: None,
        origin: Origin::Checkin,
        legal_basis: LegalBasis::Consent,
        created_at: at,
        updated_at: at,
        client_edited_at: None,
    };
    store::replace(&app.state.db, people::COLLECTION, &id, &doc)
        .await
        .unwrap();
    id
}

pub async fn stored_person(app: &TestApp, id: &str) -> PersonDoc {
    store::get(&app.state.db, people::COLLECTION, id)
        .await
        .unwrap()
        .unwrap()
}

pub fn household_body(id: &str, members: &[&str]) -> Value {
    json!({ "id": id, "member_ids": members, "client_created_at": "2026-10-21T10:00:00Z" })
}

/// Creates a household in `shelter` as `who`; returns its id.
pub async fn household(app: &TestApp, who: &Actor, shelter: &str, members: &[&str]) -> String {
    let id = new_id();
    let (status, body) = app
        .send(
            "POST",
            &format!("/v1/shelters/{shelter}/households"),
            &who.secret,
            Some(household_body(&id, members)),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    id
}

async fn post(w: &World, who: &Actor, shelter: &str, body: Value) -> (StatusCode, Value) {
    w.app
        .send(
            "POST",
            &format!("/v1/shelters/{shelter}/households"),
            &who.secret,
            Some(body),
        )
        .await
}

async fn patch(w: &World, who: &Actor, id: &str, body: Value) -> (StatusCode, Value) {
    w.app
        .send(
            "PATCH",
            &format!("/v1/households/{id}"),
            &who.secret,
            Some(body),
        )
        .await
}

async fn get(w: &World, who: &Actor, id: &str) -> (StatusCode, Value) {
    w.app
        .send("GET", &format!("/v1/households/{id}"), &who.secret, None)
        .await
}

fn error_of(body: &Value) -> Option<&str> {
    body["error"].as_str()
}

#[tokio::test]
async fn creates_a_household_and_links_the_members() {
    let w = world().await;
    let (ana, beto) = (
        person(&w.app, "Ana", Some(&w.a)).await,
        person(&w.app, "Beto", Some(&w.a)).await,
    );
    let id = new_id();
    let mut body = household_body(&id, &[&ana, &beto]);
    body["head_id"] = json!(ana);
    body["pets"] = json!([{ "species": " cachorro ", "count": 2 }]);
    let (status, created) = post(&w, &w.vol_a, &w.a, body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["household"]["id"], json!(id));
    for person_id in [&ana, &beto] {
        let stored = stored_person(&w.app, person_id).await;
        assert_eq!(stored.household_id.as_deref(), Some(id.as_str()));
    }
    let (status, view) = get(&w, &w.admin_a, &id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["head_id"], json!(ana));
    assert_eq!(view["pets"], json!([{ "species": "cachorro", "count": 2 }]));
    assert_eq!(view["members"].as_array().unwrap().len(), 2);
    assert_eq!(view["members"][0]["name"], json!("Ana"));
    assert_eq!(view["members"][0]["level"], json!("no_biometrics"));
    assert_eq!(view["card_code"], Value::Null);
    let actions = crate::test_support::audit_actions(&w.app.state.db).await;
    assert!(actions.contains(&"household.create".to_string()));
}

// Criterion 35: no current shelter, no history.
#[tokio::test]
async fn reading_a_household_shows_no_shelter_and_no_history() {
    let w = world().await;
    let ana = person(&w.app, "Ana", Some(&w.a)).await;
    let id = household(&w.app, &w.vol_a, &w.a, &[&ana]).await;
    let (_, view) = get(&w, &w.vol_a, &id).await;
    let text = view.to_string();
    for hidden in [w.a.as_str(), "shelter", "stays", "history"] {
        assert!(!text.contains(hidden), "{hidden} in {text}");
    }
    let member = view["members"][0].as_object().unwrap();
    let mut keys: Vec<_> = member.keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["level", "name", "person_id"]);
}

// Matrix 2.3: household in the own shelter, unconfirmed and confirmed alike.
#[tokio::test]
async fn matrix_volunteers_and_admins_of_both_trust_levels_act_in_their_own_shelter() {
    let w = world().await;
    for (who, shelter) in [
        (&w.vol_a, &w.a),
        (&w.admin_a, &w.a),
        (&w.vol_b, &w.b),
        (&w.admin_b, &w.b),
    ] {
        let p = person(&w.app, "P", Some(shelter)).await;
        let id = household(&w.app, who, shelter, &[&p]).await;
        assert_eq!(get(&w, who, &id).await.0, StatusCode::OK);
        let q = person(&w.app, "Q", Some(shelter)).await;
        let (status, body) = patch(&w, who, &id, json!({ "add_member_ids": [q] })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
}

#[tokio::test]
async fn matrix_everyone_else_is_refused() {
    let w = world().await;
    let ana = person(&w.app, "Ana", Some(&w.a)).await;
    let id = household(&w.app, &w.vol_a, &w.a, &[&ana]).await;
    let other = person(&w.app, "Beto", Some(&w.a)).await;
    let refused = [
        &w.outsider,
        &w.validator,
        &w.civil_defense,
        &w.vol_b,
        &w.admin_b,
    ];
    for who in refused {
        let body = household_body(&new_id(), &[&other]);
        let (status, _) = post(&w, who, &w.a, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(get(&w, who, &id).await.0, StatusCode::FORBIDDEN);
        let (status, _) = patch(&w, who, &id, json!({ "pets": [] })).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    // Without a session nothing is answered.
    let (status, _, _) = w
        .app
        .request("GET", &format!("/v1/households/{id}"), &[], None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(stored_person(&w.app, &other).await.household_id, None);
}

#[tokio::test]
async fn a_person_in_another_household_is_a_conflict_and_nothing_is_written() {
    let w = world().await;
    let (ana, beto) = (
        person(&w.app, "Ana", Some(&w.a)).await,
        person(&w.app, "Beto", Some(&w.a)).await,
    );
    let first = household(&w.app, &w.vol_a, &w.a, &[&ana]).await;
    let body = household_body(&new_id(), &[&beto, &ana]);
    let (status, body) = post(&w, &w.vol_a, &w.a, body).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_of(&body), Some("person_in_other_household"));
    assert_eq!(body["detail"]["household_id"], json!(first));
    assert_eq!(stored_person(&w.app, &beto).await.household_id, None);
    let all = crate::test_support::all_docs(&w.app.state.db, COLLECTION).await;
    assert_eq!(all.len(), 1);
}

#[tokio::test]
async fn invalid_requests_are_refused_with_the_field() {
    let w = world().await;
    let ana = person(&w.app, "Ana", Some(&w.a)).await;
    type Edit = Box<dyn Fn(&mut Value)>;
    let cases: Vec<(&str, Edit)> = vec![
        ("id", Box::new(|b| b["id"] = json!("not-a-uuid"))),
        ("member_ids", Box::new(|b| b["member_ids"] = json!([]))),
        (
            "member_ids",
            Box::new(|b| b["member_ids"] = json!(["x", "x"])),
        ),
        ("member_ids", Box::new(|b| b["member_ids"] = json!(["a/b"]))),
        ("head_id", Box::new(|b| b["head_id"] = json!("stranger"))),
        (
            "pets",
            Box::new(|b| b["pets"] = json!([{ "species": " ", "count": 1 }])),
        ),
        (
            "pets",
            Box::new(|b| b["pets"] = json!([{ "species": "gato", "count": 0 }])),
        ),
        (
            "client_created_at",
            Box::new(|b| b["client_created_at"] = json!("ontem")),
        ),
    ];
    for (field, edit) in cases {
        let mut body = household_body(&new_id(), &[&ana]);
        edit(&mut body);
        let (status, body) = post(&w, &w.vol_a, &w.a, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field} {body}");
        assert_eq!(body["detail"]["field"], json!(field), "{body}");
    }
    let body = household_body(&new_id(), &["ghost"]);
    let (status, body) = post(&w, &w.vol_a, &w.a, body).await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::NOT_FOUND, Some("person_not_found"))
    );
    assert_eq!(stored_person(&w.app, &ana).await.household_id, None);
}

#[tokio::test]
async fn the_same_request_again_gets_the_same_answer_but_not_from_someone_else() {
    let w = world().await;
    let ana = person(&w.app, "Ana", Some(&w.a)).await;
    let body = household_body(&new_id(), &[&ana]);
    let first = post(&w, &w.vol_a, &w.a, body.clone()).await;
    assert_eq!(first.0, StatusCode::CREATED);
    let again = post(&w, &w.vol_a, &w.a, body.clone()).await;
    assert_eq!(again.0, StatusCode::CREATED);
    let (status, other) = post(&w, &w.admin_a, &w.a, body).await;
    assert_eq!(
        (status, error_of(&other)),
        (StatusCode::CONFLICT, Some("household_exists"))
    );
}

#[tokio::test]
async fn unknown_household_is_a_404() {
    let w = world().await;
    let (status, body) = get(&w, &w.vol_a, &new_id()).await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::NOT_FOUND, Some("household_not_found"))
    );
}

#[tokio::test]
async fn patch_adds_removes_and_moves_the_head_and_the_pets() {
    let w = world().await;
    let (ana, beto, caio) = (
        person(&w.app, "Ana", Some(&w.a)).await,
        person(&w.app, "Beto", Some(&w.a)).await,
        person(&w.app, "Caio", Some(&w.a)).await,
    );
    let id = household(&w.app, &w.vol_a, &w.a, &[&ana, &beto]).await;
    let (status, view) = patch(
        &w,
        &w.vol_a,
        &id,
        json!({ "add_member_ids": [caio], "head_id": caio, "pets": [{ "species": "gato", "count": 1 }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["members"].as_array().unwrap().len(), 3);
    assert_eq!(view["head_id"], json!(caio));
    assert_eq!(view["pets"][0]["species"], json!("gato"));
    let stored = stored_person(&w.app, &caio).await;
    assert_eq!(stored.household_id.as_deref(), Some(id.as_str()));

    // Removing the head clears it; the person is free to join another household.
    let body = json!({ "remove_member_ids": [caio] });
    let (status, view) = patch(&w, &w.vol_a, &id, body).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["head_id"], Value::Null);
    assert_eq!(view["members"].as_array().unwrap().len(), 2);
    assert_eq!(stored_person(&w.app, &caio).await.household_id, None);
    household(&w.app, &w.vol_a, &w.a, &[&caio]).await;
    let stored: HouseholdDoc = store::get(&w.app.state.db, COLLECTION, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.member_ids, vec![ana, beto]);
}

#[tokio::test]
async fn patch_refuses_what_does_not_hold() {
    let w = world().await;
    let (ana, beto, caio) = (
        person(&w.app, "Ana", Some(&w.a)).await,
        person(&w.app, "Beto", Some(&w.a)).await,
        person(&w.app, "Caio", Some(&w.a)).await,
    );
    let id = household(&w.app, &w.vol_a, &w.a, &[&ana]).await;
    let other = household(&w.app, &w.vol_a, &w.a, &[&beto]).await;
    let body = json!({ "add_member_ids": [caio, beto] });
    let (status, body) = patch(&w, &w.vol_a, &id, body).await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::CONFLICT, Some("person_in_other_household"))
    );
    assert_eq!(body["detail"]["household_id"], json!(other));
    assert_eq!(stored_person(&w.app, &caio).await.household_id, None);
    for (field, edit) in [
        ("head_id", json!({ "head_id": caio })),
        (
            "remove_member_ids",
            json!({ "add_member_ids": [caio], "remove_member_ids": [caio] }),
        ),
        (
            "pets",
            json!({ "pets": [{ "species": "gato", "count": 100 }] }),
        ),
    ] {
        let (status, body) = patch(&w, &w.vol_a, &id, edit).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["detail"]["field"], json!(field));
    }
    let (status, _) = patch(&w, &w.vol_a, &new_id(), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// Criterion 34: one person in two households, concurrent.
#[tokio::test]
async fn of_many_households_racing_for_one_person_exactly_one_is_created() {
    let w = world().await;
    let ana = person(&w.app, "Ana", Some(&w.a)).await;
    let mut handles = Vec::new();
    for _ in 0..8 {
        let (app, secret, shelter, ana) = (
            w.app.clone(),
            w.vol_a.secret.clone(),
            w.a.clone(),
            ana.clone(),
        );
        handles.push(tokio::spawn(async move {
            app.send(
                "POST",
                &format!("/v1/shelters/{shelter}/households"),
                &secret,
                Some(household_body(&new_id(), &[&ana])),
            )
            .await
        }));
    }
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    let created = results
        .iter()
        .filter(|r| r.0 == StatusCode::CREATED)
        .count();
    let conflicts = results
        .iter()
        .filter(|r| error_of(&r.1) == Some("person_in_other_household"))
        .count();
    assert_eq!((created, conflicts), (1, 7), "{results:?}");
    let all = crate::test_support::all_docs(&w.app.state.db, COLLECTION).await;
    assert_eq!(all.len(), 1);
    let winner = results.iter().find(|r| r.0 == StatusCode::CREATED).unwrap();
    assert_eq!(
        stored_person(&w.app, &ana).await.household_id.as_deref(),
        winner.1["household"]["id"].as_str()
    );
}

#[tokio::test]
async fn of_many_patches_racing_for_one_person_exactly_one_adds_it() {
    let w = world().await;
    let caio = person(&w.app, "Caio", Some(&w.a)).await;
    let mut ids = Vec::new();
    for n in 0..6 {
        let p = person(&w.app, &format!("P{n}"), Some(&w.a)).await;
        ids.push(household(&w.app, &w.vol_a, &w.a, &[&p]).await);
    }
    let mut handles = Vec::new();
    for id in ids {
        let (app, secret, caio) = (w.app.clone(), w.vol_a.secret.clone(), caio.clone());
        handles.push(tokio::spawn(async move {
            app.send(
                "PATCH",
                &format!("/v1/households/{id}"),
                &secret,
                Some(json!({ "add_member_ids": [caio] })),
            )
            .await
        }));
    }
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    let added = results.iter().filter(|r| r.0 == StatusCode::OK).count();
    let conflicts = results
        .iter()
        .filter(|r| r.0 == StatusCode::CONFLICT)
        .count();
    assert_eq!((added, conflicts), (1, 5), "{results:?}");
    let holders = crate::test_support::all_docs(&w.app.state.db, COLLECTION)
        .await
        .iter()
        .filter(|h| h["member_ids"].to_string().contains(&caio))
        .count();
    assert_eq!(holders, 1);
}
