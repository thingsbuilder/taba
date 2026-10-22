use super::domain::*;
use super::model::*;
use super::persist::*;
use crate::error::ApiError;
use crate::matches::{self, MatchDoc, MatchStatus, MediatorKind};
use crate::matching;
use crate::people::{self, PersonDoc};
use crate::store;
use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, audit_actions, new_id};
use crate::volunteers::GlobalRole;
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use firestore::FirestoreInstant;
use serde_json::{Value, json};
use std::sync::Arc;

fn photo() -> Value {
    json!(STANDARD.encode(b"a face"))
}

fn finished(token: &str) -> (u16, Value) {
    (
        200,
        json!({ "id": "img", "state": "PROCESS_STATE_FINISHED", "data": { "humanToken": { "value": token } } }),
    )
}

/// The next photos sent to Unico come back as these tokens, in order.
fn tokens(app: &TestApp, tokens: &[&str]) {
    app.unico.with(|s| {
        tokens
            .iter()
            .for_each(|t| s.post_replies.push_back(finished(t)))
    });
}

fn refuse_next(app: &TestApp) {
    app.unico.with(|s| {
        s.post_replies
            .push_back((400, json!({ "code": 3, "message": "INVALID_REQUEST" })))
    });
}

fn body(id: &str, photos: usize) -> Value {
    json!({
        "id": id,
        "photos_base64": (0..photos).map(|_| photo()).collect::<Vec<_>>(),
        "missing_person": { "name": "Joao", "nickname": "Jo", "notes": "camisa azul" },
        "family": { "name": "Maria", "phone": "+5551999999999", "relationship": "mae", "language": "pt-BR" },
    })
}

async fn search(app: &TestApp, who: &Actor, body: Value) -> (StatusCode, Value) {
    app.send("POST", "/v1/searches", &who.secret, Some(body))
        .await
}

fn person_id(app: &TestApp, token: &str) -> String {
    app.state.person_id_for_token(token)
}

/// A check-in by photo of whoever `token` is, as `who` in `shelter`.
async fn checkin(app: &TestApp, who: &Actor, shelter: &str, token: &str, extra: Value) {
    tokens(app, &[token]);
    let mut request = json!({
        "id": new_id(),
        "legal_basis": "consent",
        "client_created_at": store::now().to_string(),
        "data": {},
        "photo_base64": photo(),
    });
    for (key, value) in extra.as_object().unwrap() {
        request[key] = value.clone();
    }
    let (status, answer) = app
        .send(
            "POST",
            &format!("/v1/shelters/{shelter}/checkins"),
            &who.secret,
            Some(request),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
}

async fn matches_of(app: &TestApp) -> Vec<MatchDoc> {
    app.state
        .db
        .fluent()
        .select()
        .from(matches::COLLECTION)
        .obj()
        .query()
        .await
        .unwrap()
}

async fn stored_search(app: &TestApp, id: &str) -> SearchDoc {
    store::get(&app.state.db, COLLECTION, id)
        .await
        .unwrap()
        .unwrap()
}

/// Ana, admin of a confirmed shelter.
async fn world() -> (TestApp, Actor, String) {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let shelter = app.confirmed_shelter(&ana, "Ginasio", ORIGIN).await;
    (app, ana, shelter)
}

// ---- who may search (spec 3.7 item 1) ----

#[tokio::test]
async fn who_may_register_a_search() {
    let (app, ana, _) = world().await;
    // A volunteer of a confirmed shelter.
    tokens(&app, &["t1"]);
    let (status, answer) = search(&app, &ana, body(&new_id(), 1)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{answer}");

    // An unconfirmed shelter's admin, a volunteer with no shelter and a validator alone: 403.
    let bia = app.actor("Bia").await;
    app.create_shelter(&bia, "Escola", (ORIGIN.0 + 0.5, ORIGIN.1))
        .await;
    let caio = app.actor("Caio").await;
    let validator = app.actor("Val").await;
    app.grant(&validator, GlobalRole::Validator).await;
    for who in [&bia, &caio, &validator] {
        let (status, answer) = search(&app, who, body(&new_id(), 1)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
        assert_eq!(answer["error"], "forbidden");
    }

    // Civil defense, mediator and protection outside any shelter.
    for (name, role) in [
        ("Dora", GlobalRole::CivilDefense),
        ("Eli", GlobalRole::Mediator),
        ("Fabi", GlobalRole::Protection),
    ] {
        let who = app.actor(name).await;
        app.grant(&who, role).await;
        tokens(&app, &["t-role"]);
        let id = new_id();
        let (status, answer) = search(&app, &who, body(&id, 1)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{answer}");
        assert_eq!(stored_search(&app, &id).await.origin_shelter_id, None);
    }

    // No session.
    let (status, _, _) = app
        .request("POST", "/v1/searches", &[], Some(body(&new_id(), 1)))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_bad_body_is_refused_before_unico_is_asked() {
    let (app, ana, _) = world().await;
    let before = app.unico.post_count();
    let mut cases = vec![
        ("no photos", body(&new_id(), 0)),
        ("six photos", body(&new_id(), 6)),
    ];
    let mut not_base64 = body(&new_id(), 1);
    not_base64["photos_base64"] = json!(["***"]);
    cases.push(("not base64", not_base64));
    let mut blank_name = body(&new_id(), 1);
    blank_name["missing_person"]["name"] = json!("  ");
    cases.push(("blank name", blank_name));
    let mut language = body(&new_id(), 1);
    language["family"]["language"] = json!("fr");
    cases.push(("language", language));
    let mut not_uuid = body("x", 1);
    not_uuid["id"] = json!("x");
    cases.push(("id", not_uuid));
    for (what, case) in cases {
        let (status, answer) = search(&app, &ana, case).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{what}: {answer}");
    }
    let mut big = body(&new_id(), 1);
    big["photos_base64"] = json!([STANDARD.encode(vec![0u8; MAX_PHOTO_BYTES + 1])]);
    let (status, answer) = search(&app, &ana, big).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{answer}");
    assert_eq!(app.unico.post_count(), before);
    assert!(all_docs(&app.state.db, COLLECTION).await.is_empty());
}

// ---- what is kept ----

#[tokio::test]
async fn only_the_derived_person_ids_are_kept_and_usable_photos_counts_the_tokens() {
    let (app, ana, shelter) = world().await;
    // Three photos: a token, a refusal, the same person again. Two photos gave a token.
    tokens(&app, &["tok-joao"]);
    refuse_next(&app);
    tokens(&app, &["tok-joao"]);
    let id = new_id();
    let (status, answer) = search(&app, &ana, body(&id, 3)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{answer}");
    assert_eq!(
        answer,
        json!({ "message": "search_registered", "usable_photos": 2 })
    );
    let doc = stored_search(&app, &id).await;
    assert_eq!(doc.person_ids, vec![person_id(&app, "tok-joao")]);
    assert_eq!((doc.photos_sent, doc.usable_photos), (3, 2));
    assert_eq!(
        (doc.created_by.as_str(), doc.via, doc.status),
        (ana.id.as_str(), SearchVia::Volunteer, SearchStatus::Active)
    );
    assert_eq!(doc.origin_shelter_id.as_deref(), Some(shelter.as_str()));
    let raw = all_docs(&app.state.db, COLLECTION).await;
    let raw = raw[0].to_string();
    assert!(!raw.contains("tok-joao") && !raw.contains(&STANDARD.encode(b"a face")));

    // Who searched is audited, under vital interest.
    let audits = all_docs(&app.state.db, "audit_log").await;
    let entry = audits
        .iter()
        .find(|e| e["action"] == "search.create")
        .unwrap();
    assert_eq!(entry["volunteer_id"], json!(ana.id));
    assert_eq!(entry["legal_basis"], "vital_interest");
    assert_eq!(entry["target_id"], json!(id));
}

// ---- the answer never tells (criterion 47) ----

#[tokio::test]
async fn the_answer_is_the_same_with_and_without_a_match() {
    let (app, ana, shelter) = world().await;
    checkin(&app, &ana, &shelter, "tok-found", json!({})).await;
    let (without, with) = (new_id(), new_id());
    tokens(&app, &["tok-nobody"]);
    let (status_a, a) = search(&app, &ana, body(&without, 1)).await;
    tokens(&app, &["tok-found"]);
    let (status_b, b) = search(&app, &ana, body(&with, 1)).await;
    assert_eq!(
        (status_a, status_b),
        (StatusCode::ACCEPTED, StatusCode::ACCEPTED)
    );
    assert_eq!(a.to_string(), b.to_string());
    // The match exists, and the answer says nothing about it.
    let all = matches_of(&app).await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].search_id, with);
    assert!(!b.to_string().contains("match"));
}

// ---- matching (criteria 51, 50, 46) ----

#[tokio::test]
async fn a_later_check_in_creates_one_match_and_repeating_it_creates_no_other() {
    let (app, ana, shelter) = world().await;
    let (search_id, found) = (new_id(), person_id(&app, "tok-found"));
    tokens(&app, &["tok-found"]);
    search(&app, &ana, body(&search_id, 1)).await;
    assert!(matches_of(&app).await.is_empty());

    checkin(&app, &ana, &shelter, "tok-found", json!({})).await;
    let all = matches_of(&app).await;
    assert_eq!(all.len(), 1);
    let m = &all[0];
    assert_eq!(
        m.id.as_deref(),
        Some(matching::match_id(&search_id, &found).as_str())
    );
    assert_eq!(
        (m.search_id.as_str(), m.person_id.as_str()),
        (search_id.as_str(), found.as_str())
    );
    assert_eq!(
        (
            m.mediator_kind,
            m.mediator_shelter_id.as_deref(),
            m.status,
            m.is_child
        ),
        (
            MediatorKind::Shelter,
            Some(shelter.as_str()),
            MatchStatus::Open,
            false
        )
    );

    checkin(&app, &ana, &shelter, "tok-found", json!({})).await;
    assert_eq!(matches_of(&app).await.len(), 1);
}

#[tokio::test]
async fn a_search_is_compared_with_the_people_already_registered() {
    let (app, ana, shelter) = world().await;
    checkin(&app, &ana, &shelter, "tok-found", json!({})).await;
    tokens(&app, &["tok-other", "tok-found"]);
    let id = new_id();
    search(&app, &ana, body(&id, 2)).await;
    let all = matches_of(&app).await;
    assert_eq!(all.len(), 1, "a match in any photo counts");
    assert_eq!(all[0].person_id, person_id(&app, "tok-found"));
    // Searching again with the same id changes nothing.
    let (status, _) = search(&app, &ana, body(&id, 2)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(matches_of(&app).await.len(), 1);
}

#[tokio::test]
async fn a_closed_pair_never_gets_a_new_match() {
    let (app, ana, shelter) = world().await;
    tokens(&app, &["tok-found"]);
    search(&app, &ana, body(&new_id(), 1)).await;
    checkin(&app, &ana, &shelter, "tok-found", json!({})).await;
    for status in [MatchStatus::NotThePerson, MatchStatus::Declined] {
        let mut m = matches_of(&app).await.remove(0);
        let id = m.id.take().unwrap();
        m.status = status;
        store::replace(&app.state.db, matches::COLLECTION, &id, &m)
            .await
            .unwrap();
        checkin(&app, &ana, &shelter, "tok-found", json!({})).await;
        let all = matches_of(&app).await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].status, status, "the closed match was left alone");
    }
}

#[tokio::test]
async fn the_queue_follows_the_person_found() {
    let (app, ana, confirmed) = world().await;
    let bia = app.actor("Bia").await;
    let unconfirmed = app
        .create_shelter(&bia, "Escola", (ORIGIN.0 + 0.5, ORIGIN.1))
        .await;
    for t in ["tok-child", "tok-unconfirmed", "tok-confirmed"] {
        tokens(&app, &[t]);
        search(&app, &ana, body(&new_id(), 1)).await;
    }
    checkin(
        &app,
        &ana,
        &confirmed,
        "tok-child",
        json!({ "legal_basis": "child_protection", "data": { "is_child": true, "child_status": "unaccompanied" } }),
    )
    .await;
    checkin(&app, &bia, &unconfirmed, "tok-unconfirmed", json!({})).await;
    checkin(&app, &ana, &confirmed, "tok-confirmed", json!({})).await;
    let all = matches_of(&app).await;
    assert_eq!(all.len(), 3);
    let by_person = |token: &str| {
        let id = person_id(&app, token);
        all.iter().find(|m| m.person_id == id).unwrap().clone()
    };
    let child = by_person("tok-child");
    assert_eq!(
        (
            child.mediator_kind,
            child.mediator_shelter_id,
            child.is_child
        ),
        (MediatorKind::Protection, None, true)
    );
    let other = by_person("tok-unconfirmed");
    assert_eq!(
        (other.mediator_kind, other.mediator_shelter_id),
        (MediatorKind::Central, None)
    );
    let here = by_person("tok-confirmed");
    assert_eq!(
        (here.mediator_kind, here.mediator_shelter_id),
        (MediatorKind::Shelter, Some(confirmed))
    );
}

#[tokio::test]
async fn a_closed_search_is_not_compared() {
    let (app, ana, shelter) = world().await;
    let id = new_id();
    tokens(&app, &["tok-found"]);
    search(&app, &ana, body(&id, 1)).await;
    let mut doc = stored_search(&app, &id).await;
    doc.status = SearchStatus::Closed;
    store::replace(&app.state.db, COLLECTION, &id, &doc)
        .await
        .unwrap();
    checkin(&app, &ana, &shelter, "tok-found", json!({})).await;
    assert!(matches_of(&app).await.is_empty());
}

// ---- the daily limit (criterion 47) ----

#[tokio::test]
async fn the_daily_limit_answers_429_before_unico_is_asked_and_repeats_do_not_count() {
    let (mut app, ana, _) = world().await;
    Arc::make_mut(&mut app.state.config).searches_per_day = 3;
    let first = new_id();
    for id in [first.clone(), new_id(), new_id()] {
        tokens(&app, &["t"]);
        let (status, answer) = search(&app, &ana, body(&id, 1)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{answer}");
    }
    // A repeat of one already written is not a new search.
    let paid = app.unico.post_count();
    let (status, _) = search(&app, &ana, body(&first, 1)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(app.unico.post_count(), paid);

    let (status, answer) = search(&app, &ana, body(&new_id(), 1)).await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("search_limit"))
    );
    assert_eq!(app.unico.post_count(), paid, "Unico was not asked");
    assert_eq!(all_docs(&app.state.db, COLLECTION).await.len(), 3);

    // The limit is per volunteer.
    let bia = app.actor("Bia").await;
    app.grant(&bia, GlobalRole::Mediator).await;
    tokens(&app, &["t"]);
    let (status, _) = search(&app, &bia, body(&new_id(), 1)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[test]
fn the_quota_day_follows_the_pilot_time_zone_not_utc() {
    let at = |text: &str| text.parse::<FirestoreInstant>().unwrap();
    let brt = "America/Sao_Paulo";
    // 21:00 in Brazil is already the next day in UTC, and the quota does not restart there.
    assert_eq!(quota_day(at("2026-10-22T00:00:00Z"), brt), "2026-10-21");
    assert_eq!(quota_day(at("2026-10-22T02:59:59Z"), brt), "2026-10-21");
    // Midnight in Brazil it does.
    assert_eq!(quota_day(at("2026-10-22T03:00:00Z"), brt), "2026-10-22");
    assert_eq!(quota_day(at("2026-10-22T00:00:00Z"), "UTC"), "2026-10-22");
    assert_ne!(quota_id("v", brt), quota_id("w", brt));
}

async fn take(app: &TestApp, volunteer_id: &str, limit: u32) -> Result<(), ApiError> {
    let id = volunteer_id.to_string();
    store::transact(&app.state.db, |db, tx| {
        let id = id.clone();
        Box::pin(async move { take_daily_slot(&db, tx, &id, limit, "America/Sao_Paulo").await })
    })
    .await
}

#[tokio::test]
async fn the_slot_is_taken_in_the_callers_transaction_and_concurrent_takers_cannot_pass_the_limit()
{
    let (app, ana, _) = world().await;
    assert!(take(&app, &ana.id, 2).await.is_ok());
    assert!(take(&app, &ana.id, 2).await.is_ok());
    assert_eq!(
        take(&app, &ana.id, 2).await.unwrap_err().code,
        "search_limit"
    );
    // Eleven concurrent takers of ten slots: exactly ten win.
    let bia = app.actor("Bia").await;
    let mut handles = Vec::new();
    for _ in 0..11 {
        let (app, id) = (app.clone(), bia.id.clone());
        handles.push(tokio::spawn(async move { take(&app, &id, 10).await }));
    }
    let mut won = 0;
    for handle in handles {
        match handle.await.unwrap() {
            Ok(()) => won += 1,
            Err(e) => assert_eq!(e.code, "search_limit"),
        }
    }
    assert_eq!(won, 10);
}

// ---- degraded mode ----

#[tokio::test]
async fn in_degraded_mode_nothing_is_written_and_no_slot_is_taken() {
    let (app, ana, _) = world().await;
    crate::degraded::enter_limit(&app.state.db).await.unwrap();
    let (status, answer) = search(&app, &ana, body(&new_id(), 2)).await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("unico_degraded"))
    );
    assert!(all_docs(&app.state.db, COLLECTION).await.is_empty());
    let quota = all_docs(&app.state.db, QUOTA_COLLECTION).await;
    assert!(quota.iter().all(|d| d.get("count").is_none()));
}

#[tokio::test]
async fn a_retry_with_the_same_key_does_not_pay_the_photos_twice() {
    let (app, ana, _) = world().await;
    // The first photo is paid; the second fails for good (Unico down), so nothing is written.
    tokens(&app, &["t1"]);
    app.unico
        .with(|s| s.post_replies.extend((0..5).map(|_| (500, json!({})))));
    let (id, key) = (new_id(), new_id());
    let (status, _) = app
        .send_keyed(
            "POST",
            "/v1/searches",
            &ana.secret,
            &key,
            Some(body(&id, 2)),
        )
        .await;
    assert!(!status.is_success());
    assert!(all_docs(&app.state.db, COLLECTION).await.is_empty());
    let paid = app.unico.post_count();
    app.unico.with(|s| s.post_replies.clear());
    tokens(&app, &["t2"]);
    let (status, answer) = app
        .send_keyed(
            "POST",
            "/v1/searches",
            &ana.secret,
            &key,
            Some(body(&id, 2)),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{answer}");
    assert_eq!(
        app.unico.post_count(),
        paid + 1,
        "only the second photo was paid again"
    );
    assert_eq!(stored_search(&app, &id).await.usable_photos, 2);
}

// ---- concurrency ----

/// What `checkin_in` does for a new person, without the photo: write the person and look
/// for searches in one transaction.
async fn register_person(app: &TestApp, template: &PersonDoc, id: &str, shelter: &str) {
    let shelter: crate::shelters::ShelterDoc =
        store::get(&app.state.db, crate::shelters::COLLECTION, shelter)
            .await
            .unwrap()
            .unwrap();
    store::transact(&app.state.db, |db, tx| {
        let (person, id, shelter) = (template.clone(), id.to_string(), shelter.clone());
        Box::pin(async move {
            store::tx_replace(&db, tx, people::COLLECTION, &id, &person)?;
            matching::on_person_registered(&db, tx, &person, &id, Some(&shelter)).await
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn concurrent_searches_and_registrations_of_one_person_make_one_match_per_search() {
    let (app, ana, shelter) = world().await;
    checkin(&app, &ana, &shelter, "tok-template", json!({})).await;
    let template: PersonDoc = store::get(
        &app.state.db,
        people::COLLECTION,
        &person_id(&app, "tok-template"),
    )
    .await
    .unwrap()
    .unwrap();
    let owner = SearchOwner {
        volunteer_id: ana.id.clone(),
        device_id: None,
        origin_shelter_id: None,
        event_id: None,
        via: SearchVia::Volunteer,
        key: None,
    };
    let target = person_id(&app, "tok-target");
    let search_ids: Vec<String> = (0..3).map(|_| new_id()).collect();
    let mut handles = Vec::new();
    for search_id in &search_ids {
        let (db, owner, target) = (app.state.db.clone(), owner.clone(), target.clone());
        let input = SearchInput {
            id: search_id.clone(),
            photos: vec![],
            missing_person: MissingPerson {
                name: "Joao".into(),
                nickname: None,
                notes: String::new(),
            },
            family: Family {
                name: "Maria".into(),
                phone: "1".into(),
                relationship: "mae".into(),
                language: "en".into(),
            },
        };
        handles.push(tokio::spawn(async move {
            commit_search(&db, &owner, &input, &[target], 1, None, "UTC")
                .await
                .unwrap();
        }));
    }
    for _ in 0..6 {
        let (app, template, target, shelter) = (
            app.clone(),
            template.clone(),
            target.clone(),
            shelter.clone(),
        );
        handles.push(tokio::spawn(async move {
            register_person(&app, &template, &target, &shelter).await
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }
    let mut ids: Vec<String> = matches_of(&app)
        .await
        .iter()
        .filter_map(|m| m.id.clone())
        .collect();
    ids.sort();
    let mut expected: Vec<String> = search_ids
        .iter()
        .map(|s| matching::match_id(s, &target))
        .collect();
    expected.sort();
    assert_eq!(ids, expected, "one match for each search, none twice");
    let created = audit_actions(&app.state.db)
        .await
        .iter()
        .filter(|a| *a == "match.create")
        .count();
    assert_eq!(created, 3);
}
