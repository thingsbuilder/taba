use super::domain::{is_write, key_of};
use super::model::{COLLECTION, IdempotencyDoc};
use axum::http::{HeaderMap, Method, StatusCode};
use firestore::jiff::SignedDuration;

use super::model::{ClaimState, TTL};
use super::*;
use crate::store;
use crate::test_support::{
    ORIGIN, TestApp, all_docs, audit_actions, new_id, north_of_origin, shelter_body,
};
use serde_json::{Value, json};

async fn stored(app: &TestApp, key: &str) -> Option<IdempotencyDoc> {
    store::get(&app.state.db, COLLECTION, key).await.unwrap()
}

#[test]
fn only_writes_with_a_usable_key_are_considered() {
    assert!(
        is_write(&Method::POST)
            && is_write(&Method::PATCH)
            && is_write(&Method::PUT)
            && is_write(&Method::DELETE)
    );
    assert!(!is_write(&Method::GET) && !is_write(&Method::HEAD));
    let mut headers = HeaderMap::new();
    assert_eq!(key_of(&headers), None);
    headers.insert("idempotency-key", " abc ".parse().unwrap());
    assert_eq!(key_of(&headers), Some("abc"));
    headers.insert("idempotency-key", "  ".parse().unwrap());
    assert_eq!(key_of(&headers), None);
}

#[tokio::test]
async fn the_same_key_gives_the_same_response_and_nothing_runs_twice() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let key = new_id();
    let body = shelter_body(&new_id(), "Ginásio", ORIGIN);
    let (status, first) = app
        .send_keyed(
            "POST",
            "/v1/shelters",
            &ana.secret,
            &key,
            Some(body.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let audits = audit_actions(&app.state.db).await.len();

    let (status, again) = app
        .send_keyed("POST", "/v1/shelters", &ana.secret, &key, Some(body))
        .await;
    assert_eq!((status, &again), (StatusCode::CREATED, &first));
    assert_eq!(audit_actions(&app.state.db).await.len(), audits);
    assert_eq!(all_docs(&app.state.db, "shelters").await.len(), 1);
}

#[tokio::test]
async fn the_same_key_does_not_run_an_effect_twice_even_when_the_request_is_not_naturally_repeatable()
 {
    // Creating an invite makes a new code every time; the key is what keeps it to one.
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let shelter = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
    let path = format!("/v1/shelters/{shelter}/invites");
    let key = new_id();
    let body = json!({ "kind": "qr", "role": "volunteer" });
    let (_, first) = app
        .send_keyed("POST", &path, &ana.secret, &key, Some(body.clone()))
        .await;
    let (_, again) = app
        .send_keyed("POST", &path, &ana.secret, &key, Some(body.clone()))
        .await;
    assert_eq!(again["code"], first["code"]);
    assert_eq!(all_docs(&app.state.db, "invites").await.len(), 1);
    // Without a key, a repeat is a new request.
    app.send("POST", &path, &ana.secret, Some(body)).await;
    assert_eq!(all_docs(&app.state.db, "invites").await.len(), 2);
}

#[tokio::test]
async fn copies_of_a_request_sent_at_once_run_once() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let shelter = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
    let path = format!("/v1/shelters/{shelter}/invites");
    let key = new_id();
    let mut handles = Vec::new();
    for _ in 0..6 {
        let (app, path, secret, key) = (app.clone(), path.clone(), ana.secret.clone(), key.clone());
        handles.push(tokio::spawn(async move {
            app.send_keyed(
                "POST",
                &path,
                &secret,
                &key,
                Some(json!({ "kind": "link", "role": "volunteer" })),
            )
            .await
        }));
    }
    let mut codes = Vec::new();
    for handle in handles {
        let (status, body) = handle.await.unwrap();
        assert_eq!(status, StatusCode::CREATED, "{body}");
        codes.push(body["code"].as_str().unwrap().to_string());
    }
    codes.dedup();
    assert_eq!(codes.len(), 1, "every copy got the first answer");
    assert_eq!(all_docs(&app.state.db, "invites").await.len(), 1);
}

#[tokio::test]
async fn copies_sent_to_two_instances_at_once_run_once() {
    // Two instances share nothing but the database: no in-memory lock can serialize them.
    let one = TestApp::new().await;
    let two = TestApp::with_db(one.state.db.clone()).await;
    let ana = one.actor("Ana").await;
    let shelter = one.create_shelter(&ana, "Ginásio", ORIGIN).await;
    let path = format!("/v1/shelters/{shelter}/invites");
    let key = new_id();
    let mut handles = Vec::new();
    for n in 0..6 {
        let (app, path, secret, key) = (
            if n % 2 == 0 { one.clone() } else { two.clone() },
            path.clone(),
            ana.secret.clone(),
            key.clone(),
        );
        handles.push(tokio::spawn(async move {
            app.send_keyed(
                "POST",
                &path,
                &secret,
                &key,
                Some(json!({ "kind": "link", "role": "volunteer" })),
            )
            .await
        }));
    }
    let mut codes = Vec::new();
    for handle in handles {
        let (status, body) = handle.await.unwrap();
        assert_eq!(status, StatusCode::CREATED, "{body}");
        codes.push(body["code"].as_str().unwrap().to_string());
    }
    codes.dedup();
    assert_eq!(codes.len(), 1, "every copy got the first answer");
    assert_eq!(all_docs(&one.state.db, "invites").await.len(), 1);
    assert_eq!(stored(&one, &key).await.unwrap().attempts, 1);
}

#[tokio::test]
async fn the_same_key_with_another_payload_is_a_conflict_and_changes_nothing() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let key = new_id();
    let first = shelter_body(&new_id(), "Ginásio", ORIGIN);
    let (status, _) = app
        .send_keyed("POST", "/v1/shelters", &ana.secret, &key, Some(first))
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let other = shelter_body(&new_id(), "Outro nome", north_of_origin(5_000.0));
    let (status, body) = app
        .send_keyed("POST", "/v1/shelters", &ana.secret, &key, Some(other))
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("idempotency_key_reused"))
    );
    assert_eq!(all_docs(&app.state.db, "shelters").await.len(), 1);
}

#[tokio::test]
async fn a_claim_that_lapsed_after_the_commit_runs_again_and_records_the_second_attempt() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let key = new_id();
    let body = shelter_body(&new_id(), "Ginásio", ORIGIN);
    let (status, first) = app
        .send_keyed(
            "POST",
            "/v1/shelters",
            &ana.secret,
            &key,
            Some(body.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    // The process died after the commit and before the receipt: the claim is still pending
    // and its lease ran out.
    let mut doc = stored(&app, &key).await.unwrap();
    doc.state = ClaimState::Pending;
    doc.http_status = 0;
    doc.body.clear();
    doc.lease_until = Some(store::now() - SignedDuration::from_secs(1));
    store::replace(&app.state.db, COLLECTION, &key, &doc)
        .await
        .unwrap();

    let (status, again) = app
        .send_keyed("POST", "/v1/shelters", &ana.secret, &key, Some(body))
        .await;
    assert_eq!((status, &again), (StatusCode::CREATED, &first));
    assert_eq!(
        all_docs(&app.state.db, "shelters").await.len(),
        1,
        "the handler is idempotent by the id of the shelter"
    );
    let doc = stored(&app, &key).await.unwrap();
    assert_eq!((doc.state, doc.attempts), (ClaimState::Done, 2));
}

#[tokio::test]
async fn a_copy_of_a_request_still_running_waits_and_then_asks_to_retry() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let key = new_id();
    let body = shelter_body(&new_id(), "Ginásio", ORIGIN);
    let live = IdempotencyDoc {
        volunteer_id: Some(ana.id.clone()),
        route: "POST /v1/shelters".into(),
        fingerprint: digest_of(&[b"POST", b"/v1/shelters", b"", body.to_string().as_bytes()]),
        state: ClaimState::Pending,
        attempts: 1,
        lease_until: Some(store::now() + SignedDuration::from_secs(60)),
        http_status: 0,
        body: String::new(),
        expires_at: store::now() + TTL,
    };
    store::replace(&app.state.db, COLLECTION, &key, &live)
        .await
        .unwrap();
    let (status, headers, answer) = app
        .request(
            "POST",
            "/v1/shelters",
            &[
                ("authorization", format!("Bearer {}", ana.secret).as_str()),
                ("idempotency-key", key.as_str()),
            ],
            Some(body),
        )
        .await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("idempotency_in_progress")
        )
    );
    assert_eq!(headers.get("retry-after").unwrap(), "1");
    assert!(all_docs(&app.state.db, "shelters").await.is_empty());
}

#[tokio::test]
async fn a_replay_never_reaches_further_than_the_session_that_sends_it() {
    let app = TestApp::new().await;
    let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
    let shelter = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
    let path = format!("/v1/shelters/{shelter}/invites");
    let key = new_id();
    let body = json!({ "kind": "qr", "role": "volunteer" });
    let (status, first) = app
        .send_keyed("POST", &path, &ana.secret, &key, Some(body.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let code = first["code"].as_str().unwrap();

    // Another volunteer with the same key, route and payload does not get Ana's invite.
    let (status, stolen) = app
        .send_keyed("POST", &path, &bia.secret, &key, Some(body.clone()))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!stolen.to_string().contains(code));
    // Nor does a session that ended.
    app.send("DELETE", "/v1/sessions/current", &ana.secret, None)
        .await;
    let (status, gone) = app
        .send_keyed("POST", &path, &ana.secret, &key, Some(body))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!gone.to_string().contains(code));
}

#[tokio::test]
async fn the_paid_answer_of_a_photo_is_reused_only_for_the_same_content() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", "photo-key".parse().unwrap());
    let asks = std::sync::atomic::AtomicU32::new(0);
    let ask = |person: &'static str| {
        let asks = &asks;
        async move {
            asks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(person.to_string())
        }
    };
    let route = "POST /v1/rescues";
    let (d1, d2) = (digest_of(&[b"photo one"]), digest_of(&[b"photo two"]));
    let call = |digest: String, person| {
        let (state, headers, id, ask) = (&app.state, &headers, &ana.id, ask(person));
        async move { photo_person_id_of(state, headers, id, route, Some(&digest), ask).await }
    };
    assert_eq!(call(d1.clone(), "p1").await.unwrap(), "p1");
    // The retry of the same content does not ask again.
    assert_eq!(call(d1.clone(), "other").await.unwrap(), "p1");
    // The same key with another photo never gets the first person.
    assert_eq!(call(d2.clone(), "p2").await.unwrap(), "p2");
    assert_eq!(asks.load(std::sync::atomic::Ordering::SeqCst), 2);
    // Without a digest it asks and keeps nothing.
    let nothing = photo_person_id_of(&app.state, &headers, &ana.id, route, None, ask("p3")).await;
    assert_eq!(nothing.unwrap(), "p3");
    assert_eq!(asks.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[test]
fn the_digest_tells_apart_parts_that_run_together() {
    assert_ne!(digest_of(&[b"ab", b"c"]), digest_of(&[b"a", b"bc"]));
    assert_eq!(digest_of(&[b"x"]), digest_of(&[b"x"]));
}

#[tokio::test]
async fn the_response_is_kept_for_seven_days_with_who_and_where() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let key = new_id();
    let id = new_id();
    let (_, answer) = app
        .send_keyed(
            "POST",
            "/v1/shelters",
            &ana.secret,
            &key,
            Some(shelter_body(&id, "Ginásio", ORIGIN)),
        )
        .await;
    let doc = stored(&app, &key).await.unwrap();
    assert_eq!(
        (
            doc.volunteer_id.as_deref(),
            doc.route.as_str(),
            doc.http_status
        ),
        (Some(ana.id.as_str()), "POST /v1/shelters", 201)
    );
    assert_eq!(serde_json::from_str::<Value>(&doc.body).unwrap(), answer);
    let ttl = doc.expires_at.as_second() - store::now().as_second();
    assert!((ttl - 7 * 24 * 3600).abs() <= 2, "{ttl}");
}

#[tokio::test]
async fn a_refusal_is_not_kept_so_the_same_item_can_be_sent_again_with_an_explanation() {
    let app = TestApp::new().await;
    let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
    app.create_shelter(&ana, "Ginásio", ORIGIN).await;
    let key = new_id();
    let mut body = shelter_body(&new_id(), "Escola", north_of_origin(80.0));
    let (status, refused) = app
        .send_keyed(
            "POST",
            "/v1/shelters",
            &bia.secret,
            &key,
            Some(body.clone()),
        )
        .await;
    assert_eq!(
        (status, refused["error"].as_str()),
        (StatusCode::CONFLICT, Some("shelter_nearby"))
    );
    assert!(stored(&app, &key).await.is_none());

    body["other_site_reason"] = json!("Outro prédio");
    let (status, created) = app
        .send_keyed("POST", "/v1/shelters", &bia.secret, &key, Some(body))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert!(stored(&app, &key).await.is_some());
}

#[tokio::test]
async fn a_key_reused_for_something_else_never_returns_the_first_answer() {
    let app = TestApp::new().await;
    let (ana, bia) = (app.actor("Ana").await, app.actor("Bia").await);
    let key = new_id();
    app.send_keyed(
        "POST",
        "/v1/shelters",
        &ana.secret,
        &key,
        Some(shelter_body(&new_id(), "Ginásio", ORIGIN)),
    )
    .await;

    // Another volunteer with the same key.
    let (status, body) = app
        .send_keyed(
            "POST",
            "/v1/shelters",
            &bia.secret,
            &key,
            Some(shelter_body(&new_id(), "Outro", north_of_origin(5_000.0))),
        )
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("idempotency_key_mismatch")
        )
    );
    assert!(!body.to_string().contains("Ginásio"));
    // The same volunteer, another route.
    let (status, body) = app
        .send_keyed(
            "PATCH",
            "/v1/me",
            &ana.secret,
            &key,
            Some(json!({ "name": "Ana B" })),
        )
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("idempotency_key_mismatch")
        )
    );
    assert_eq!(all_docs(&app.state.db, "shelters").await.len(), 1);
    assert_eq!(
        app.get_authed("/v1/me", &ana.secret).await.1["volunteer"]["name"],
        "Ana"
    );
}

#[tokio::test]
async fn an_expired_record_is_ignored_while_the_ttl_has_not_deleted_it() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let key = new_id();
    let body = shelter_body(&new_id(), "Ginásio", ORIGIN);
    app.send_keyed(
        "POST",
        "/v1/shelters",
        &ana.secret,
        &key,
        Some(body.clone()),
    )
    .await;
    let mut doc = stored(&app, &key).await.unwrap();
    doc.route = "POST /v1/elsewhere".into();
    doc.expires_at = store::now() - SignedDuration::from_secs(1);
    store::replace(&app.state.db, COLLECTION, &key, &doc)
        .await
        .unwrap();
    // It would be a mismatch if it counted; instead the request runs and replaces it.
    let (status, _) = app
        .send_keyed("POST", "/v1/shelters", &ana.secret, &key, Some(body))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(stored(&app, &key).await.unwrap().route, "POST /v1/shelters");
}

#[tokio::test]
async fn reads_unsigned_requests_and_bad_keys_are_left_alone() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    // A GET with a key is just a GET.
    let (status, _) = app
        .send_keyed("GET", "/v1/me", &ana.secret, "k1", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    // A write with no session reaches the handler (which refuses it) and keeps nothing.
    let (status, _, _) = app
        .request(
            "PATCH",
            "/v1/me",
            &[("idempotency-key", "k2")],
            Some(json!({ "name": "X" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A key that cannot be a document id is refused.
    for bad in ["a/b".to_string(), "k".repeat(129)] {
        let (status, body) = app
            .send_keyed(
                "PATCH",
                "/v1/me",
                &ana.secret,
                &bad,
                Some(json!({ "name": "X" })),
            )
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_idempotency_key"))
        );
    }
    assert!(all_docs(&app.state.db, COLLECTION).await.is_empty());
}

#[tokio::test]
async fn an_empty_response_is_replayed_empty() {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let shelter = app.create_shelter(&ana, "Ginásio", ORIGIN).await;
    let code = app.invite(&ana, &shelter, "qr", "volunteer").await;
    let key = new_id();
    let path = format!("/v1/invites/{code}");
    assert_eq!(
        app.send_keyed("DELETE", &path, &ana.secret, &key, None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    // Without the key the second delete is a 410; with it, the first answer comes back.
    assert_eq!(
        app.send_keyed("DELETE", &path, &ana.secret, &key, None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.send("DELETE", &path, &ana.secret, None).await.0,
        StatusCode::GONE
    );
}
