use super::domain::*;
use super::model::*;
use super::persist::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::alerts;
use crate::error::ApiError;
use crate::searches::{Family, MissingPerson};
use crate::searches::{RegisterOutcome, SearchInput, SearchVia};
use crate::shelters::{self, ShelterDoc};
use crate::store;
use crate::volunteers::GlobalRole;
use axum::Json;
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use firestore::jiff::SignedDuration;
use serde_json::{Value, json};
const MAX_IMAGE_BYTES: usize = 800 * 1024;
use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, audit_actions, new_id};

fn photo(bytes: usize) -> String {
    STANDARD.encode(vec![0xFFu8; bytes])
}

fn input(code: &str) -> SearchInput {
    SearchInput {
        id: search_id_of(code),
        photos: vec![vec![1, 2, 3]],
        missing_person: MissingPerson {
            name: "Maria".into(),
            nickname: None,
            notes: "secret notes".into(),
        },
        family: Family {
            name: "Ana".into(),
            phone: "5551999990000".into(),
            relationship: "daughter".into(),
            language: "pt-BR".into(),
        },
    }
}

fn body(photos: Vec<String>) -> Value {
    json!({
        "photos_base64": photos,
        "missing_person": { "name": "Maria", "notes": "" },
        "family": { "name": "Ana", "phone": "5551999990000", "relationship": "daughter", "language": "pt-BR" },
    })
}

async fn setup() -> (TestApp, Actor, String) {
    let app = TestApp::new().await;
    let admin = app.actor("Admin").await;
    let shelter = app.confirmed_shelter(&admin, "Ginásio", ORIGIN).await;
    (app, admin, shelter)
}

async fn make_link(app: &TestApp, who: &Actor, shelter: &str) -> String {
    let (status, body) = app
        .send(
            "POST",
            "/v1/search-links",
            &who.secret,
            Some(json!({ "shelter_id": shelter })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["code"].as_str().unwrap().to_string()
}

async fn create_status(app: &TestApp, who: &Actor, shelter: &str) -> StatusCode {
    let body = Some(json!({ "shelter_id": shelter }));
    app.send("POST", "/v1/search-links", &who.secret, body)
        .await
        .0
}

async fn link_doc(app: &TestApp, code: &str) -> SearchLinkDoc {
    store::get(&app.state.db, COLLECTION, code)
        .await
        .unwrap()
        .unwrap()
}

async fn public(
    app: &TestApp,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let xff = format!("test-{}", new_id());
    let (status, _, json) = app
        .request(method, path, &[("x-forwarded-for", xff.as_str())], body)
        .await;
    (status, json)
}

fn ok(usable: u32) -> Result<RegisterOutcome, ApiError> {
    Ok(RegisterOutcome {
        usable_photos: usable,
    })
}

async fn submit(app: &TestApp, code: &str) -> Result<(StatusCode, Json<Value>), ApiError> {
    run_submit(&app.state, code, None, input(code), |_, _| async { ok(2) }).await
}

// ---- create ----

#[tokio::test]
async fn create_makes_a_12_hour_link_in_the_volunteers_name_and_audits_it() {
    let (app, admin, shelter) = setup().await;
    let (status, body) = app
        .send(
            "POST",
            "/v1/search-links",
            &admin.secret,
            Some(json!({ "shelter_id": shelter })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let code = body["code"].as_str().unwrap();
    assert_eq!(URL_SAFE_NO_PAD.decode(code).unwrap().len(), 16);
    assert_eq!(
        body["url"],
        format!("https://{}/f/{code}", crate::test_support::TEST_DOMAIN)
    );
    let link = link_doc(&app, code).await;
    assert_eq!(link.created_by, admin.id);
    assert_eq!(link.shelter_id, shelter);
    assert_eq!(link.status, LinkStatus::Active);
    assert_eq!(link.attempts, 0);
    assert_eq!(link.expires_at.duration_since(link.created_at), TTL);
    assert_eq!(body["expires_at"], link.expires_at.to_string());
    assert!(
        audit_actions(&app.state.db)
            .await
            .contains(&"search_link.create".to_string())
    );
}

#[tokio::test]
async fn create_permissions() {
    let (app, _admin, shelter) = setup().await;
    // A member of an unconfirmed shelter.
    let other = app.actor("Other").await;
    let unconfirmed = app
        .create_shelter(
            &other,
            "Galpão",
            crate::test_support::north_of_origin(5000.0),
        )
        .await;
    assert_eq!(
        create_status(&app, &other, &unconfirmed).await,
        StatusCode::FORBIDDEN
    );
    // Not a member of the confirmed shelter.
    assert_eq!(
        create_status(&app, &other, &shelter).await,
        StatusCode::FORBIDDEN
    );
    // Unknown shelter.
    assert_eq!(
        create_status(&app, &other, "nope").await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // Global roles are not members: they name a confirmed shelter of an active event, and
    // only that one (an unknown or unconfirmed one is 422, a closed event 409).
    for role in [
        GlobalRole::CivilDefense,
        GlobalRole::Mediator,
        GlobalRole::Protection,
    ] {
        let who = app.actor(&format!("g-{role:?}")).await;
        app.grant(&who, role).await;
        assert_eq!(
            create_status(&app, &who, &shelter).await,
            StatusCode::CREATED,
            "{role:?}"
        );
        for refused in [unconfirmed.as_str(), "nope"] {
            let (status, body) = app
                .send(
                    "POST",
                    "/v1/search-links",
                    &who.secret,
                    Some(json!({ "shelter_id": refused })),
                )
                .await;
            assert_eq!(
                (status, body["error"].as_str()),
                (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_shelter")),
                "{role:?} {refused}"
            );
        }
    }
    let stored: ShelterDoc = store::get(&app.state.db, shelters::COLLECTION, &shelter)
        .await
        .unwrap()
        .unwrap();
    let event_id = stored.event_id.unwrap();
    let mut event: crate::events::EventDoc =
        store::get(&app.state.db, crate::events::COLLECTION, &event_id)
            .await
            .unwrap()
            .unwrap();
    event.status = crate::events::EventStatus::Closed;
    store::replace(&app.state.db, crate::events::COLLECTION, &event_id, &event)
        .await
        .unwrap();
    let dc = app.actor("g-CivilDefense").await;
    assert_eq!(
        create_status(&app, &dc, &shelter).await,
        StatusCode::CONFLICT
    );
    // Validator alone is not enough.
    let validator = app.actor("Validator").await;
    app.grant(&validator, GlobalRole::Validator).await;
    assert_eq!(
        create_status(&app, &validator, &shelter).await,
        StatusCode::FORBIDDEN
    );
    // No session.
    let (status, _, _) = app
        .request(
            "POST",
            "/v1/search-links",
            &[],
            Some(json!({ "shelter_id": shelter })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_counts_in_the_daily_search_limit() {
    let (mut app, admin, shelter) = setup().await;
    std::sync::Arc::make_mut(&mut app.state.config).searches_per_day = 2;
    make_link(&app, &admin, &shelter).await;
    make_link(&app, &admin, &shelter).await;
    let (status, body) = app
        .send(
            "POST",
            "/v1/search-links",
            &admin.secret,
            Some(json!({ "shelter_id": shelter })),
        )
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("search_limit"))
    );
    assert_eq!(all_docs(&app.state.db, COLLECTION).await.len(), 2);
}

// ---- cancel ----

#[tokio::test]
async fn cancel_by_creator_or_civil_defense_only() {
    let (app, admin, shelter) = setup().await;
    let code = make_link(&app, &admin, &shelter).await;
    let path = format!("/v1/search-links/{code}");
    let stranger = app.actor("Stranger").await;
    assert_eq!(
        app.send("DELETE", &path, &stranger.secret, None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.send("DELETE", &path, &admin.secret, None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(link_doc(&app, &code).await.status, LinkStatus::Canceled);
    assert_eq!(
        app.send("DELETE", &path, &admin.secret, None).await.0,
        StatusCode::GONE
    );
    assert!(
        audit_actions(&app.state.db)
            .await
            .contains(&"search_link.cancel".to_string())
    );

    let code = make_link(&app, &admin, &shelter).await;
    let dc = app.actor("DC").await;
    app.grant(&dc, GlobalRole::CivilDefense).await;
    let status = app
        .send(
            "DELETE",
            &format!("/v1/search-links/{code}"),
            &dc.secret,
            None,
        )
        .await
        .0;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

// ---- public get ----

#[tokio::test]
async fn get_says_only_whether_the_link_is_valid() {
    let (app, admin, shelter) = setup().await;
    let code = make_link(&app, &admin, &shelter).await;
    let path = format!("/v1/search-links/{code}");
    let (status, body) = public(&app, "GET", &path, None).await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "valid": true })));

    let gone = json!({ "error": "search_link_invalid", "detail": {} });
    // Unknown.
    assert_eq!(
        public(&app, "GET", "/v1/search-links/nope", None).await,
        (StatusCode::GONE, gone.clone())
    );
    // Expired.
    let mut link = link_doc(&app, &code).await;
    link.expires_at = store::now() - SignedDuration::from_mins(1);
    store::replace(&app.state.db, COLLECTION, &code, &link)
        .await
        .unwrap();
    assert_eq!(
        public(&app, "GET", &path, None).await,
        (StatusCode::GONE, gone.clone())
    );
    // Used.
    let used = make_link(&app, &admin, &shelter).await;
    drop(submit(&app, &used).await.unwrap());
    assert_eq!(
        public(&app, "GET", &format!("/v1/search-links/{used}"), None).await,
        (StatusCode::GONE, gone.clone())
    );
    // Canceled.
    let canceled = make_link(&app, &admin, &shelter).await;
    app.send(
        "DELETE",
        &format!("/v1/search-links/{canceled}"),
        &admin.secret,
        None,
    )
    .await;
    assert_eq!(
        public(&app, "GET", &format!("/v1/search-links/{canceled}"), None).await,
        (StatusCode::GONE, gone)
    );
}

// ---- submit ----

#[tokio::test]
async fn submit_answers_like_a_search_uses_the_link_once_and_warns_only_its_creator() {
    let (app, admin, shelter) = setup().await;
    let code = make_link(&app, &admin, &shelter).await;
    let owner = Arc::new(Mutex::new(None));
    let seen = owner.clone();
    let (status, Json(answer)) = run_submit(
        &app.state,
        &code,
        Some("dev-x".into()),
        input(&code),
        |o, i| async move {
            *seen.lock().unwrap() = Some((o, i.id));
            ok(3)
        },
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        answer,
        json!({ "message": "search_registered", "usable_photos": 3 })
    );

    let (o, id) = owner.lock().unwrap().take().unwrap();
    assert_eq!(
        (o.volunteer_id.as_str(), o.via),
        (admin.id.as_str(), SearchVia::FamilyLink)
    );
    assert_eq!(o.origin_shelter_id.as_deref(), Some(shelter.as_str()));
    assert_eq!(o.key.as_deref(), Some(code.as_str()));
    let link = link_doc(&app, &code).await;
    assert_eq!(
        (link.status, link.search_id.as_deref()),
        (LinkStatus::Used, Some(id.as_str()))
    );
    assert!(link.used_at.is_some() && link.processing_until.is_none());

    // The alert: informational, for the creator, no content.
    let alerts = all_docs(&app.state.db, alerts::COLLECTION).await;
    let alert = alerts
        .iter()
        .find(|a| a["kind"] == "family_search_received")
        .unwrap();
    assert_eq!(alert["informational"], true);
    assert_eq!(alert["volunteer_ids"], json!([admin.id]));
    assert_eq!(alert["for_civil_defense"], false);
    assert_eq!(alert["detail"], json!({ "usable_photos": 3 }));
    let text = alert.to_string();
    assert!(
        !text.contains("Maria")
            && !text.contains("secret notes")
            && !text.contains("5551999990000")
    );
    assert!(
        audit_actions(&app.state.db)
            .await
            .contains(&"search_link.submit".to_string())
    );

    // Used: the pipeline does not run again.
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let again = run_submit(&app.state, &code, None, input(&code), |_, _| async move {
        counted.fetch_add(1, Ordering::SeqCst);
        ok(1)
    })
    .await
    .unwrap_err();
    assert_eq!(
        (again.status, again.code),
        (StatusCode::GONE, "search_link_invalid")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn expired_and_canceled_links_do_not_process() {
    let (app, admin, shelter) = setup().await;
    let expired = make_link(&app, &admin, &shelter).await;
    let mut link = link_doc(&app, &expired).await;
    link.expires_at = store::now() - SignedDuration::from_mins(1);
    store::replace(&app.state.db, COLLECTION, &expired, &link)
        .await
        .unwrap();
    let canceled = make_link(&app, &admin, &shelter).await;
    app.send(
        "DELETE",
        &format!("/v1/search-links/{canceled}"),
        &admin.secret,
        None,
    )
    .await;

    let calls = Arc::new(AtomicUsize::new(0));
    for code in [expired.as_str(), canceled.as_str(), "unknown"] {
        let counted = calls.clone();
        let error = run_submit(&app.state, code, None, input(code), |_, _| async move {
            counted.fetch_add(1, Ordering::SeqCst);
            ok(1)
        })
        .await
        .unwrap_err();
        assert_eq!(error.code, "search_link_invalid");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn degraded_unico_keeps_the_link_and_spends_no_attempt() {
    let (app, admin, shelter) = setup().await;
    let code = make_link(&app, &admin, &shelter).await;
    for _ in 0..(MAX_ATTEMPTS + 2) {
        let error = run_submit(&app.state, &code, None, input(&code), |_, _| async {
            Err(ApiError::unico_degraded())
        })
        .await
        .unwrap_err();
        assert_eq!(
            (error.status, error.code),
            (StatusCode::SERVICE_UNAVAILABLE, "unico_degraded")
        );
    }
    let link = link_doc(&app, &code).await;
    assert_eq!((link.status, link.attempts), (LinkStatus::Active, 0));
    assert!(link.processing_until.is_none());
    assert_eq!(
        public(&app, "GET", &format!("/v1/search-links/{code}"), None)
            .await
            .0,
        StatusCode::OK
    );
    assert!(submit(&app, &code).await.is_ok());
}

#[tokio::test]
async fn the_link_gives_up_after_max_failed_attempts() {
    let (app, admin, shelter) = setup().await;
    let code = make_link(&app, &admin, &shelter).await;
    for n in 1..=MAX_ATTEMPTS {
        let error = run_submit(&app.state, &code, None, input(&code), |_, _| async {
            Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "no_face"))
        })
        .await
        .unwrap_err();
        assert_eq!(error.code, "no_face");
        assert_eq!(link_doc(&app, &code).await.attempts, n);
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let error = run_submit(&app.state, &code, None, input(&code), |_, _| async move {
        counted.fetch_add(1, Ordering::SeqCst);
        ok(1)
    })
    .await
    .unwrap_err();
    assert_eq!(
        (error.status, error.code),
        (StatusCode::GONE, "search_link_invalid")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        public(&app, "GET", &format!("/v1/search-links/{code}"), None)
            .await
            .0,
        StatusCode::GONE
    );
}

#[tokio::test]
async fn concurrent_submits_give_exactly_one_success() {
    let (app, admin, shelter) = setup().await;
    let code = make_link(&app, &admin, &shelter).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let (state, code, calls) = (app.state.clone(), code.clone(), calls.clone());
            tokio::spawn(async move {
                run_submit(&state, &code, None, input(&code), |_, _| async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    ok(1)
                })
                .await
            })
        })
        .collect();
    let mut successes = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => successes += 1,
            Err(e) => assert!(
                matches!(e.code, "search_link_invalid" | store::CONTENTION),
                "unexpected {}",
                e.code
            ),
        }
    }
    assert_eq!(successes, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(link_doc(&app, &code).await.status, LinkStatus::Used);
    let alerts = all_docs(&app.state.db, alerts::COLLECTION).await;
    assert_eq!(
        alerts
            .iter()
            .filter(|a| a["kind"] == "family_search_received")
            .count(),
        1
    );
}

#[tokio::test]
async fn the_search_id_and_key_are_the_same_on_a_retry() {
    let (app, admin, shelter) = setup().await;
    let code = make_link(&app, &admin, &shelter).await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    for fail in [true, false] {
        let seen = seen.clone();
        let _ = run_submit(&app.state, &code, None, input(&code), |o, i| async move {
            seen.lock().unwrap().push((i.id, o.key));
            if fail {
                Err(ApiError::unico_degraded())
            } else {
                ok(1)
            }
        })
        .await;
    }
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0], seen[1]);
    assert_eq!(seen[0].1.as_deref(), Some(code.as_str()));
}

// ---- limits of the public route (refused before Unico and before the link is touched) ----

#[tokio::test]
async fn an_image_above_800_kb_is_refused_without_touching_the_link_or_unico() {
    let (app, admin, shelter) = setup().await;
    let logins = app.unico.post_count();
    let code = make_link(&app, &admin, &shelter).await;
    let path = format!("/v1/search-links/{code}/submit");
    let (status, answer) = public(
        &app,
        "POST",
        &path,
        Some(body(vec![photo(MAX_IMAGE_BYTES + 1)])),
    )
    .await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, Some("image_too_large"))
    );
    let link = link_doc(&app, &code).await;
    assert_eq!((link.status, link.attempts), (LinkStatus::Active, 0));
    assert_eq!(app.unico.post_count(), logins);
}

#[tokio::test]
async fn the_body_is_validated() {
    let (app, admin, shelter) = setup().await;
    let logins = app.unico.post_count();
    let code = make_link(&app, &admin, &shelter).await;
    let path = format!("/v1/search-links/{code}/submit");
    let bad = |photos: Vec<String>| body(photos);
    for (what, request) in [
        ("no photos", bad(vec![])),
        ("six photos", bad(vec![photo(10); 6])),
        ("not base64", bad(vec!["***".into()])),
    ] {
        let (status, _) = public(&app, "POST", &path, Some(request)).await;
        assert!(
            status == StatusCode::UNPROCESSABLE_ENTITY,
            "{what}: {status}"
        );
    }
    let mut request = body(vec![photo(10)]);
    request["family"]["language"] = json!("fr");
    assert_eq!(
        public(&app, "POST", &path, Some(request)).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut request = body(vec![photo(10)]);
    request["missing_person"]["name"] = json!("  ");
    assert_eq!(
        public(&app, "POST", &path, Some(request)).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(link_doc(&app, &code).await.attempts, 0);
    assert_eq!(app.unico.post_count(), logins);
}

#[tokio::test]
async fn a_dead_link_answers_410_through_the_route() {
    let (app, _, _) = setup().await;
    let (status, answer) = public(
        &app,
        "POST",
        "/v1/search-links/nope/submit",
        Some(body(vec![photo(10)])),
    )
    .await;
    // Until the search module lands `register_photos` answers 501, but a dead link never gets there.
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::GONE, Some("search_link_invalid"))
    );
}

#[tokio::test]
async fn submits_are_limited_per_device_and_per_ip() {
    let (app, _, _) = setup().await;
    let path = "/v1/search-links/nope/submit";
    let ip = format!("ip-{}", new_id());
    let mut statuses = Vec::new();
    for _ in 0..=MAX_PER_DEVICE_HOUR {
        let (status, _, _) = app
            .request(
                "POST",
                path,
                &[("x-forwarded-for", ip.as_str()), ("x-device-id", "d1")],
                Some(body(vec![photo(10)])),
            )
            .await;
        statuses.push(status);
    }
    assert!(
        statuses[..MAX_PER_DEVICE_HOUR]
            .iter()
            .all(|s| *s == StatusCode::GONE)
    );
    assert_eq!(statuses[MAX_PER_DEVICE_HOUR], StatusCode::TOO_MANY_REQUESTS);
    // Another device on the same IP still gets through (until the IP's own ceiling).
    let (status, _, _) = app
        .request(
            "POST",
            path,
            &[("x-forwarded-for", ip.as_str()), ("x-device-id", "d2")],
            Some(body(vec![photo(10)])),
        )
        .await;
    assert_eq!(status, StatusCode::GONE);
    // A client-made first entry of X-Forwarded-For does not dodge the limit: the last one counts.
    let spoof = format!("1.2.3.{}, {ip}", 4);
    let (status, _, _) = app
        .request(
            "POST",
            path,
            &[("x-forwarded-for", spoof.as_str()), ("x-device-id", "d1")],
            Some(body(vec![photo(10)])),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}
