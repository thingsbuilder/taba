use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde_json::{Value, json};

use super::domain::*;
use super::http::MAX_NOTE_CHARS;
use super::persist::*;
use super::*;
use crate::auth::CaptureDoc;
use crate::auth::Purpose as P;
use crate::degraded;
use crate::events::{self, EventDoc, EventStatus};
use crate::exits::fixtures;
use crate::households::tests::{World, household, person, world};
use crate::shelters::{self, Confirmation, ShelterDoc};
use crate::store;
use crate::test_support::{Actor, TestApp, all_docs, audit_actions, new_id};

const CARD: &str = "ABCD2345";
const DAYS: i64 = DAY_MICROS;

fn spec(scope: ItemScope, period: Option<u32>, once: bool, high: bool) -> ItemSpec {
    ItemSpec {
        scope,
        period_days: period,
        once_per_event: once,
        high_value: high,
    }
}

/// An id the 5% draw leaves out, so ordinary tests are deterministic.
fn unsampled() -> String {
    loop {
        let id = new_id();
        if !is_sampled(&id, 0.05) {
            return id;
        }
    }
}

fn at_instant(offset_micros: i64) -> FirestoreInstant {
    FirestoreInstant::from_microsecond(store::now().as_microsecond() + offset_micros).unwrap()
}

fn at(base: FirestoreInstant, offset_micros: i64) -> String {
    FirestoreInstant::from_microsecond(base.as_microsecond() + offset_micros)
        .unwrap()
        .to_string()
}

fn req(id: &str, person: &str, item: &str) -> Value {
    json!({
        "id": id, "card_code": CARD, "person_id": person, "item": item,
        "quantity": 1, "delivered_at": store::now().to_string(),
    })
}

/// A delivery the app queued offline: it says so and carries a `delivered_at` from minutes ago.
fn offline(body: Value) -> Value {
    with(
        with(body, "offline", json!(true)),
        "delivered_at",
        json!(at(store::now(), -10 * 60_000_000)),
    )
}

fn with(mut body: Value, key: &str, value: Value) -> Value {
    body[key] = value;
    body
}

async fn deliver(app: &TestApp, who: &Actor, shelter: &str, body: Value) -> (StatusCode, Value) {
    let auth = format!("Bearer {}", who.secret);
    let (status, _, json) = app
        .request(
            "POST",
            &format!("/v1/shelters/{shelter}/deliveries"),
            &[
                ("authorization", auth.as_str()),
                ("x-device-id", who.device.as_str()),
            ],
            Some(body),
        )
        .await;
    (status, json)
}

async fn set_items(app: &TestApp, shelter: &str, items: &[(&str, ItemSpec)]) {
    let mut doc: ShelterDoc = store::get(&app.state.db, shelters::COLLECTION, shelter)
        .await
        .unwrap()
        .unwrap();
    doc.items = items
        .iter()
        .map(|(name, spec)| (name.to_string(), spec.clone()))
        .collect();
    store::replace(&app.state.db, shelters::COLLECTION, shelter, &doc)
        .await
        .unwrap();
}

async fn stored(app: &TestApp, id: &str) -> DeliveryDoc {
    store::get(&app.state.db, COLLECTION, id)
        .await
        .unwrap()
        .unwrap()
}

async fn issue_card(w: &World, household: &str, code: &str) {
    let (status, body) = w
        .app
        .send(
            "POST",
            &format!("/v1/households/{household}/cards"),
            &w.vol_a.secret,
            Some(json!({ "code": code, "client_issued_at": "2026-10-21T10:00:00Z" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// A person of `level` in shelter A, stored under the id Unico's token `token` gives.
async fn known_person(w: &World, token: &str, level: crate::people::Level) -> String {
    let id = w.app.state.person_id_for_token(token);
    fixtures::put(&w.app, &id, &fixtures::person(level, Some(&w.a))).await;
    id
}

struct S {
    w: World,
    p1: String,
    p2: String,
    household: String,
}

/// Shelter A (unconfirmed) with a household of two people without biometrics and its card.
async fn setup() -> S {
    let w = world().await;
    let p1 = person(&w.app, "Ana", Some(&w.a)).await;
    let p2 = person(&w.app, "Bia", Some(&w.a)).await;
    let household = household(&w.app, &w.vol_a, &w.a, &[&p1, &p2]).await;
    issue_card(&w, &household, CARD).await;
    set_items(
        &w.app,
        &w.a,
        &[
            ("cesta", spec(ItemScope::PerHousehold, None, false, false)),
            ("kit", spec(ItemScope::PerPerson, Some(7), false, false)),
            ("colchao", spec(ItemScope::PerPerson, None, true, true)),
            ("tv", spec(ItemScope::PerHousehold, Some(30), false, true)),
        ],
    )
    .await;
    S {
        w,
        p1,
        p2,
        household,
    }
}

async fn revalidation_capture(
    app: &TestApp,
    who: &Actor,
    shelter: &str,
    person: &str,
    token: &str,
) -> String {
    let auth = format!("Bearer {}", who.secret);
    let (status, _, body) = app
        .request(
            "POST",
            "/v1/captures",
            &[
                ("authorization", auth.as_str()),
                ("x-device-id", who.device.as_str()),
            ],
            Some(json!({
                "purpose": "revalidation", "shelter_id": shelter, "expected_person_id": person,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["capture_id"].as_str().unwrap().to_string();
    let doc: CaptureDoc = store::get(&app.state.db, CAPTURES, &id)
        .await
        .unwrap()
        .unwrap();
    app.unico.finish_with_token(&doc.process_id, token);
    id
}

// ---- criteria 25, 27, 28: ordinary deliveries ----

#[tokio::test]
async fn an_ordinary_delivery_is_recorded_on_the_card_without_asking_unico() {
    let s = setup().await;
    let (posts, gets) = (s.w.app.unico.post_count(), s.w.app.unico.get_count());
    let id = unsampled();
    let (status, body) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&id, &s.p1, "kit")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body,
        json!({ "delivery_id": id, "status": "ok", "reasons": [] })
    );
    let doc = stored(&s.w.app, &id).await;
    assert_eq!(doc.verification, Verification::Card);
    assert_eq!(doc.status, DeliveryStatus::Ok);
    assert_eq!(doc.approved_by, None);
    assert_eq!(doc.person_id.as_deref(), Some(s.p1.as_str()));
    assert_eq!(doc.household_id.as_deref(), Some(s.household.as_str()));
    assert_eq!(doc.delivered_by, s.w.vol_a.id);
    assert_eq!(doc.shelter_name, "Abrigo A");
    assert_eq!(
        (s.w.app.unico.post_count(), s.w.app.unico.get_count()),
        (posts, gets)
    );
    let audit = all_docs(&s.w.app.state.db, "audit_log").await;
    let entry = audit
        .iter()
        .find(|e| e["action"] == "delivery.create")
        .unwrap();
    assert_eq!(entry["legal_basis"], "fraud_prevention");
    assert_eq!(entry["target_id"], id);
}

#[tokio::test]
async fn a_person_who_declined_the_selfie_gets_an_ordinary_delivery() {
    // criterion 25: `person` writes people who declined (no_biometrics, biometrics_declined)
    let s = setup().await;
    let declined = stored_person(&s.w.app, &s.p1).await;
    assert!(declined.biometrics_declined);
    let (status, body) = deliver(
        &s.w.app,
        &s.w.vol_a,
        &s.w.a,
        req(&unsampled(), &s.p1, "kit"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["status"], "ok");
}

async fn stored_person(app: &TestApp, id: &str) -> crate::people::PersonDoc {
    crate::households::tests::stored_person(app, id).await
}

#[tokio::test]
async fn an_assisted_person_receives_like_anyone_else_without_428() {
    let s = setup().await;
    let ana = known_person(&s.w, "tok-assisted", crate::people::Level::Assisted).await;
    let h = household(&s.w.app, &s.w.vol_a, &s.w.a, &[&ana]).await;
    issue_card(&s.w, &h, "EFGH6789").await;
    let body = with(
        req(&unsampled(), &ana, "kit"),
        "card_code",
        json!("EFGH6789"),
    );
    let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(
        stored(&s.w.app, answer["delivery_id"].as_str().unwrap())
            .await
            .verification,
        Verification::Card
    );
}

// ---- criterion 30: the draw and retries ----

#[tokio::test]
async fn a_drawn_delivery_asks_for_liveness_every_time_it_is_sent() {
    let mut s = setup().await;
    let mut config = (*s.w.app.state.config).clone();
    config.delivery_sample_rate = 1.0;
    s.w.app.state.config = std::sync::Arc::new(config);
    let id = new_id();
    for _ in 0..2 {
        let (status, body) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&id, &s.p1, "kit")).await;
        assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}");
        assert_eq!(body["error"], "liveness_required");
        assert_eq!(body["detail"]["reason"], "sample");
        assert_eq!(body["detail"]["admin_approval_only"], true);
    }
    assert!(all_docs(&s.w.app.state.db, COLLECTION).await.is_empty());
}

#[tokio::test]
async fn the_same_delivery_sent_again_gets_the_same_answer_and_one_document() {
    let s = setup().await;
    let id = unsampled();
    let body = req(&id, &s.p1, "cesta");
    let first = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body.clone()).await;
    let second = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body.clone()).await;
    assert_eq!(first, second);
    assert_eq!(all_docs(&s.w.app.state.db, COLLECTION).await.len(), 1);
    // another volunteer cannot take the id
    let (status, answer) = deliver(&s.w.app, &s.w.admin_a, &s.w.a, body).await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::CONFLICT, Some("delivery_id_taken"))
    );
}

#[tokio::test]
async fn the_idempotency_key_replays_the_answer_of_a_duplicate_too() {
    let s = setup().await;
    let first_id = unsampled();
    deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&first_id, &s.p1, "cesta")).await;
    let id = unsampled();
    let path = format!("/v1/shelters/{}/deliveries", s.w.a);
    let body = req(&id, &s.p2, "cesta");
    let a =
        s.w.app
            .send_keyed("POST", &path, &s.w.vol_a.secret, "k1", Some(body.clone()))
            .await;
    let b =
        s.w.app
            .send_keyed("POST", &path, &s.w.vol_a.secret, "k1", Some(body))
            .await;
    assert_eq!(a.0, StatusCode::CREATED);
    assert_eq!(a, b);
    assert_eq!(a.1["status"], "in_review");
    assert_eq!(all_docs(&s.w.app.state.db, COLLECTION).await.len(), 2);
}

// ---- criterion 29: high value ----

#[tokio::test]
async fn high_value_without_capture_asks_for_liveness() {
    let s = setup().await;
    let ana = known_person(&s.w, "tok-v", crate::people::Level::Verified).await;
    let h = household(&s.w.app, &s.w.vol_a, &s.w.a, &[&ana]).await;
    issue_card(&s.w, &h, "EFGH6789").await;
    let body = with(
        req(&new_id(), &ana, "colchao"),
        "card_code",
        json!("EFGH6789"),
    );
    let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{answer}");
    assert_eq!(answer["detail"]["reason"], "high_value");
    assert_eq!(answer["detail"]["admin_approval_only"], false);
}

#[tokio::test]
async fn a_capture_of_the_right_person_records_a_liveness_delivery_and_is_consumed() {
    let s = setup().await;
    let ana = known_person(&s.w, "tok-v", crate::people::Level::Verified).await;
    let h = household(&s.w.app, &s.w.vol_a, &s.w.a, &[&ana]).await;
    issue_card(&s.w, &h, "EFGH6789").await;
    let capture = revalidation_capture(&s.w.app, &s.w.vol_a, &s.w.a, &ana, "tok-v").await;
    let id = new_id();
    let body = with(
        with(req(&id, &ana, "colchao"), "card_code", json!("EFGH6789")),
        "capture_id",
        json!(capture),
    );
    let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["status"], "ok");
    assert_eq!(
        stored(&s.w.app, &id).await.verification,
        Verification::Liveness
    );
    let doc: CaptureDoc = store::get(&s.w.app.state.db, CAPTURES, &capture)
        .await
        .unwrap()
        .unwrap();
    assert!(doc.consumed);
    // sent again by the same volunteer: same answer, Unico not asked again
    let gets = s.w.app.unico.get_count();
    let again = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
    assert_eq!(again, (StatusCode::CREATED, answer));
    assert_eq!(s.w.app.unico.get_count(), gets);
}

#[tokio::test]
async fn a_capture_of_someone_else_is_refused_and_nothing_is_recorded() {
    let s = setup().await;
    let ana = known_person(&s.w, "tok-v", crate::people::Level::Verified).await;
    let h = household(&s.w.app, &s.w.vol_a, &s.w.a, &[&ana]).await;
    issue_card(&s.w, &h, "EFGH6789").await;
    let capture =
        revalidation_capture(&s.w.app, &s.w.vol_a, &s.w.a, &ana, "tok-someone-else").await;
    let body = with(
        with(
            req(&new_id(), &ana, "colchao"),
            "card_code",
            json!("EFGH6789"),
        ),
        "capture_id",
        json!(capture),
    );
    let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body.clone()).await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::CONFLICT, Some("revalidation_failed"))
    );
    assert!(all_docs(&s.w.app.state.db, COLLECTION).await.is_empty());
    let doc: CaptureDoc = store::get(&s.w.app.state.db, CAPTURES, &capture)
        .await
        .unwrap()
        .unwrap();
    assert!(doc.consumed);
    assert!(
        audit_actions(&s.w.app.state.db)
            .await
            .contains(&"delivery.revalidation_failed".to_string())
    );
    // the spent capture cannot be used again
    let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::CONFLICT, Some("capture_consumed"))
    );
}

#[tokio::test]
async fn a_capture_asked_for_another_person_or_volunteer_is_refused() {
    let s = setup().await;
    let ana = known_person(&s.w, "tok-v", crate::people::Level::Verified).await;
    let h = household(&s.w.app, &s.w.vol_a, &s.w.a, &[&ana]).await;
    issue_card(&s.w, &h, "EFGH6789").await;
    // asked for someone else of the household
    let capture = revalidation_capture(&s.w.app, &s.w.vol_a, &s.w.a, &s.p1, "tok-v").await;
    let body = with(
        with(
            req(&new_id(), &ana, "colchao"),
            "card_code",
            json!("EFGH6789"),
        ),
        "capture_id",
        json!(capture),
    );
    let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
    assert!(all_docs(&s.w.app.state.db, COLLECTION).await.is_empty());
}

#[tokio::test]
async fn admin_approval_by_an_admin_is_ok_audited_and_not_reviewed() {
    let s = setup().await;
    let id = new_id();
    let body = with(req(&id, &s.p1, "colchao"), "admin_approval", json!(true));
    let (status, answer) = deliver(&s.w.app, &s.w.admin_a, &s.w.a, body).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["status"], "ok");
    let doc = stored(&s.w.app, &id).await;
    assert_eq!(doc.verification, Verification::AdminApproval);
    assert_eq!(doc.approved_by.as_deref(), Some(s.w.admin_a.id.as_str()));
    assert_eq!(doc.delivered_by, s.w.admin_a.id);
    let audit = all_docs(&s.w.app.state.db, "audit_log").await;
    let entry = audit
        .iter()
        .find(|e| e["action"] == "delivery.admin_approval")
        .unwrap();
    assert_eq!(entry["volunteer_id"], s.w.admin_a.id);
    assert_eq!(entry["legal_basis"], "fraud_prevention");
}

#[tokio::test]
async fn admin_approval_by_a_volunteer_is_refused() {
    let s = setup().await;
    let body = with(
        req(&new_id(), &s.p1, "colchao"),
        "admin_approval",
        json!(true),
    );
    let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("admin_approval_required"))
    );
    assert!(all_docs(&s.w.app.state.db, COLLECTION).await.is_empty());
}

// ---- criteria 4 and 33: offline and degraded ----

#[tokio::test]
async fn offline_high_value_without_an_admin_goes_to_review_and_with_one_is_ok() {
    let s = setup().await;
    let id = new_id();
    let body = offline(req(&id, &s.p1, "colchao"));
    let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["status"], "in_review");
    assert_eq!(
        answer["reasons"],
        json!([{ "kind": "liveness_unavailable" }])
    );
    assert_eq!(stored(&s.w.app, &id).await.verification, Verification::Card);

    let approved = with(
        offline(req(&new_id(), &s.p2, "colchao")),
        "admin_approval",
        json!(true),
    );
    let (status, answer) = deliver(&s.w.app, &s.w.admin_a, &s.w.a, approved).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["status"], "ok");
}

#[tokio::test]
async fn offline_has_no_draw_and_never_asks_unico() {
    let mut s = setup().await;
    let mut config = (*s.w.app.state.config).clone();
    config.delivery_sample_rate = 1.0;
    s.w.app.state.config = std::sync::Arc::new(config);
    let (posts, gets) = (s.w.app.unico.post_count(), s.w.app.unico.get_count());
    let body = offline(req(&new_id(), &s.p1, "kit"));
    let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["status"], "ok");
    assert_eq!(
        (s.w.app.unico.post_count(), s.w.app.unico.get_count()),
        (posts, gets)
    );
}

#[tokio::test]
async fn degraded_mode_works_like_offline() {
    let mut s = setup().await;
    let mut config = (*s.w.app.state.config).clone();
    config.delivery_sample_rate = 1.0;
    s.w.app.state.config = std::sync::Arc::new(config);
    degraded::enter_limit(&s.w.app.state.db).await.unwrap();
    // ordinary item: no draw, recorded on the card
    let (status, answer) =
        deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&new_id(), &s.p1, "kit")).await;
    assert_eq!(
        (status, answer["status"].as_str()),
        (StatusCode::CREATED, Some("ok"))
    );
    // high value without an admin: review
    let (status, answer) = deliver(
        &s.w.app,
        &s.w.vol_a,
        &s.w.a,
        req(&new_id(), &s.p1, "colchao"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["reasons"][0]["kind"], "liveness_unavailable");
    // high value with an admin's approval: ok
    let body = with(
        req(&new_id(), &s.p2, "colchao"),
        "admin_approval",
        json!(true),
    );
    let (status, answer) = deliver(&s.w.app, &s.w.admin_a, &s.w.a, body).await;
    assert_eq!(
        (status, answer["status"].as_str()),
        (StatusCode::CREATED, Some("ok"))
    );
}

#[tokio::test]
async fn offline_high_value_that_is_also_a_duplicate_carries_both_reasons() {
    let s = setup().await;
    let first = req(&new_id(), &s.p1, "colchao");
    let first = with(first, "admin_approval", json!(true));
    deliver(&s.w.app, &s.w.admin_a, &s.w.a, first).await;
    let body = offline(req(&new_id(), &s.p1, "colchao"));
    let (_, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body).await;
    let kinds: Vec<&str> = answer["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["duplicate_in_period", "liveness_unavailable"]);
}

// ---- criteria 31 and 32: duplicates ----

#[tokio::test]
async fn per_household_flags_the_second_member_and_per_person_does_not() {
    let s = setup().await;
    let first = unsampled();
    let (_, a) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&first, &s.p1, "cesta")).await;
    assert_eq!(a["status"], "ok");
    let (status, b) = deliver(
        &s.w.app,
        &s.w.vol_a,
        &s.w.a,
        req(&unsampled(), &s.p2, "cesta"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    assert_eq!(b["status"], "in_review");
    assert_eq!(b["reasons"][0]["kind"], "duplicate_in_period");
    assert_eq!(b["reasons"][0]["previous_delivery_id"], first);
    assert!(b["reasons"][0]["previous_delivered_at"].is_string());

    deliver(
        &s.w.app,
        &s.w.vol_a,
        &s.w.a,
        req(&unsampled(), &s.p1, "kit"),
    )
    .await;
    let (_, c) = deliver(
        &s.w.app,
        &s.w.vol_a,
        &s.w.a,
        req(&unsampled(), &s.p2, "kit"),
    )
    .await;
    assert_eq!(c["status"], "ok");
    // the same person again is flagged, and still recorded
    let (status, d) = deliver(
        &s.w.app,
        &s.w.vol_a,
        &s.w.a,
        req(&unsampled(), &s.p1, "kit"),
    )
    .await;
    assert_eq!(
        (status, d["status"].as_str()),
        (StatusCode::CREATED, Some("in_review"))
    );
}

#[tokio::test]
async fn the_period_limits_and_the_default_of_seven_days() {
    let s = setup().await;
    let base = at_instant(-29 * DAYS);
    let send = |offset: i64| {
        let body = with(
            req(&unsampled(), &s.p1, "kit"),
            "delivered_at",
            json!(at(base, offset)),
        );
        deliver(&s.w.app, &s.w.vol_a, &s.w.a, body)
    };
    assert_eq!(send(0).await.1["status"], "ok");
    assert_eq!(send(7 * DAYS).await.1["status"], "ok");
    assert_eq!(send(7 * DAYS + DAYS / 2).await.1["status"], "in_review");
    assert_eq!(send(20 * DAYS).await.1["status"], "ok");
    // an item without period_days uses 7
    let body = |offset: i64| {
        with(
            req(&unsampled(), &s.p1, "cesta"),
            "delivered_at",
            json!(at(base, offset)),
        )
    };
    assert_eq!(
        deliver(&s.w.app, &s.w.vol_a, &s.w.a, body(0)).await.1["status"],
        "ok"
    );
    assert_eq!(
        deliver(&s.w.app, &s.w.vol_a, &s.w.a, body(7 * DAYS - 1))
            .await
            .1["status"],
        "in_review"
    );
}

#[tokio::test]
async fn once_per_event_is_flagged_even_after_the_period() {
    let s = setup().await;
    let base = store::now();
    let old = with(
        with(
            req(&new_id(), &s.p1, "colchao"),
            "delivered_at",
            json!(at(base, -25 * DAYS)),
        ),
        "admin_approval",
        json!(true),
    );
    let (_, a) = deliver(&s.w.app, &s.w.admin_a, &s.w.a, old).await;
    assert_eq!(a["status"], "ok");
    let again = with(
        with(
            req(&new_id(), &s.p1, "colchao"),
            "admin_approval",
            json!(true),
        ),
        "delivered_at",
        json!(at(base, 0)),
    );
    let (status, b) = deliver(&s.w.app, &s.w.admin_a, &s.w.a, again).await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    assert_eq!(b["status"], "in_review");
    assert_eq!(b["reasons"][0]["kind"], "duplicate_in_period");
    // another person is not affected (per_person)
    let other = with(
        req(&new_id(), &s.p2, "colchao"),
        "admin_approval",
        json!(true),
    );
    assert_eq!(
        deliver(&s.w.app, &s.w.admin_a, &s.w.a, other).await.1["status"],
        "ok"
    );
}

#[tokio::test]
async fn duplicates_are_looked_for_in_every_active_event_and_only_there() {
    let s = setup().await;
    set_items(
        &s.w.app,
        &s.w.b,
        &[("cesta", spec(ItemScope::PerHousehold, None, false, false))],
    )
    .await;
    // the card works in any shelter: first at B (confirmed, another event)
    let first = unsampled();
    let (status, a) = deliver(&s.w.app, &s.w.vol_b, &s.w.b, req(&first, &s.p1, "cesta")).await;
    assert_eq!(status, StatusCode::CREATED, "{a}");
    let (_, b) = deliver(
        &s.w.app,
        &s.w.vol_a,
        &s.w.a,
        req(&unsampled(), &s.p2, "cesta"),
    )
    .await;
    assert_eq!(b["status"], "in_review");
    assert_eq!(b["reasons"][0]["previous_delivery_id"], first);
    // the requester, shelter A, is not confirmed: it is told nothing of the other shelter's delivery
    assert!(
        b["reasons"][0].get("previous_shelter_name").is_none(),
        "{b}"
    );
    assert!(
        b["reasons"][0].get("previous_delivered_at").is_none(),
        "{b}"
    );

    // once the event of B is closed its deliveries no longer count
    let shelter_b: ShelterDoc = store::get(&s.w.app.state.db, shelters::COLLECTION, &s.w.b)
        .await
        .unwrap()
        .unwrap();
    let event_id = shelter_b.event_id.unwrap();
    let mut event: EventDoc = store::get(&s.w.app.state.db, events::COLLECTION, &event_id)
        .await
        .unwrap()
        .unwrap();
    event.status = EventStatus::Closed;
    store::replace(&s.w.app.state.db, events::COLLECTION, &event_id, &event)
        .await
        .unwrap();
    let (_, c) = deliver(
        &s.w.app,
        &s.w.vol_a,
        &s.w.a,
        req(&unsampled(), &s.p1, "kit"),
    )
    .await;
    assert_eq!(c["status"], "ok");
    let (_, d) = deliver(
        &s.w.app,
        &s.w.vol_a,
        &s.w.a,
        req(&unsampled(), &s.p2, "cesta"),
    )
    .await;
    // the duplicate of A's own first cesta (the in-review one) still counts, B's does not
    assert_eq!(d["reasons"][0]["previous_delivery_id"], b["delivery_id"]);
}

#[tokio::test]
async fn the_shelter_of_the_other_delivery_is_not_shown_when_it_is_unconfirmed() {
    let s = setup().await;
    set_items(
        &s.w.app,
        &s.w.b,
        &[("cesta", spec(ItemScope::PerHousehold, None, false, false))],
    )
    .await;
    let first = unsampled();
    deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&first, &s.p1, "cesta")).await;
    let (status, b) = deliver(
        &s.w.app,
        &s.w.vol_b,
        &s.w.b,
        req(&unsampled(), &s.p2, "cesta"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    assert_eq!(b["reasons"][0]["previous_delivery_id"], first);
    assert!(
        b["reasons"][0].get("previous_shelter_name").is_none(),
        "{b}"
    );
    // and the replay of that answer does not leak it either
    let again = deliver(
        &s.w.app,
        &s.w.vol_b,
        &s.w.b,
        req(b["delivery_id"].as_str().unwrap(), &s.p2, "cesta"),
    )
    .await;
    assert!(again.1["reasons"][0].get("previous_shelter_name").is_none());
}

// ---- two deliveries at once ----

#[tokio::test]
async fn two_simultaneous_deliveries_to_one_household_flag_exactly_one() {
    let s = setup().await;
    let names: Vec<String> = (0..4).map(|n| format!("pacote{n}")).collect();
    let items: Vec<(&str, ItemSpec)> = names
        .iter()
        .map(|n| {
            (
                n.as_str(),
                spec(ItemScope::PerHousehold, None, false, false),
            )
        })
        .collect();
    set_items(&s.w.app, &s.w.a, &items).await;
    for name in &names {
        let (a, b) = tokio::join!(
            deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&unsampled(), &s.p1, name)),
            deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&unsampled(), &s.p2, name)),
        );
        assert_eq!(
            (a.0, b.0),
            (StatusCode::CREATED, StatusCode::CREATED),
            "{a:?} {b:?}"
        );
        let mut statuses = [
            a.1["status"].as_str().unwrap().to_string(),
            b.1["status"].as_str().unwrap().to_string(),
        ];
        statuses.sort();
        assert_eq!(statuses, ["in_review", "ok"], "{name}");
    }
}

// ---- refusals ----

#[tokio::test]
async fn who_may_deliver() {
    let s = setup().await;
    let path = format!("/v1/shelters/{}/deliveries", s.w.a);
    let (status, _, answer) =
        s.w.app
            .request("POST", &path, &[], Some(req(&unsampled(), &s.p1, "kit")))
            .await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("unauthorized"))
    );
    for who in [
        &s.w.outsider,
        &s.w.vol_b,
        &s.w.validator,
        &s.w.civil_defense,
    ] {
        let (status, answer) =
            deliver(&s.w.app, who, &s.w.a, req(&unsampled(), &s.p1, "kit")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
    }
    // volunteers and admins of an unconfirmed shelter deliver (matrix 2.3)
    for who in [&s.w.vol_a, &s.w.admin_a] {
        let (status, answer) =
            deliver(&s.w.app, who, &s.w.a, req(&unsampled(), &s.p1, "kit")).await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
    }
    assert!(all_docs(&s.w.app.state.db, COLLECTION).await.len() == 2);
}

#[tokio::test]
async fn the_card_the_person_and_the_item_must_exist() {
    let s = setup().await;
    let other = person(&s.w.app, "Carla", Some(&s.w.a)).await;
    let cases = [
        (
            with(
                req(&unsampled(), &s.p1, "kit"),
                "card_code",
                json!("ZZZZZZZZ"),
            ),
            StatusCode::NOT_FOUND,
            "card_not_found",
        ),
        (
            req(&unsampled(), &other, "kit"),
            StatusCode::NOT_FOUND,
            "person_not_in_household",
        ),
        (
            req(&unsampled(), &s.p1, "unknown"),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            with(req(&unsampled(), &s.p1, "kit"), "quantity", json!(0)),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            with(
                req(&unsampled(), &s.p1, "kit"),
                "delivered_at",
                json!("yesterday"),
            ),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            with(req(&unsampled(), &s.p1, "kit"), "id", json!("not-a-uuid")),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
    ];
    for (body, expected, code) in cases {
        let (status, answer) = deliver(&s.w.app, &s.w.vol_a, &s.w.a, body.clone()).await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (expected, Some(code)),
            "{body}"
        );
    }
    let (_, answer) = deliver(
        &s.w.app,
        &s.w.vol_a,
        &s.w.a,
        req(&unsampled(), &s.p1, "unknown"),
    )
    .await;
    assert_eq!(answer["detail"]["field"], "item");
    // a canceled card no longer works
    let (status, _) =
        s.w.app
            .send(
                "POST",
                &format!("/v1/cards/{CARD}/cancel"),
                &s.w.vol_a.secret,
                Some(json!({})),
            )
            .await;
    assert_eq!(status, StatusCode::OK);
    let (status, answer) = deliver(
        &s.w.app,
        &s.w.vol_a,
        &s.w.a,
        req(&unsampled(), &s.p1, "kit"),
    )
    .await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("card_not_found"))
    );
    assert!(all_docs(&s.w.app.state.db, COLLECTION).await.is_empty());
}

#[tokio::test]
async fn a_closed_event_takes_no_deliveries() {
    let s = setup().await;
    let shelter: ShelterDoc = store::get(&s.w.app.state.db, shelters::COLLECTION, &s.w.a)
        .await
        .unwrap()
        .unwrap();
    let event_id = shelter.event_id.unwrap();
    let mut event: EventDoc = store::get(&s.w.app.state.db, events::COLLECTION, &event_id)
        .await
        .unwrap()
        .unwrap();
    event.status = EventStatus::Closed;
    store::replace(&s.w.app.state.db, events::COLLECTION, &event_id, &event)
        .await
        .unwrap();
    let (status, answer) = deliver(
        &s.w.app,
        &s.w.vol_a,
        &s.w.a,
        req(&unsampled(), &s.p1, "kit"),
    )
    .await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::CONFLICT, Some("event_closed"))
    );
}

// ---- late biometrics (spec 4.12) ----

#[tokio::test]
async fn late_biometrics_repoints_deliveries_and_the_duplicate_follows_the_new_id() {
    let s = setup().await;
    let first = unsampled();
    deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&first, &s.p1, "kit")).await;
    let auth = format!("Bearer {}", s.w.vol_a.secret);
    let (status, _, body) =
        s.w.app
            .request(
                "POST",
                "/v1/captures",
                &[
                    ("authorization", auth.as_str()),
                    ("x-device-id", s.w.vol_a.device.as_str()),
                ],
                Some(json!({ "purpose": "biometrics", "expected_person_id": s.p1 })),
            )
            .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let capture = body["capture_id"].as_str().unwrap().to_string();
    let doc: CaptureDoc = store::get(&s.w.app.state.db, CAPTURES, &capture)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(doc.purpose, P::Biometrics);
    s.w.app
        .unico
        .finish_with_token(&doc.process_id, "tok-ana-late");
    let (status, _, answer) =
        s.w.app
            .request(
                "PUT",
                &format!("/v1/people/{}/biometrics", s.p1),
                &[
                    ("authorization", auth.as_str()),
                    ("x-device-id", s.w.vol_a.device.as_str()),
                ],
                Some(json!({ "capture_id": capture })),
            )
            .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let new = s.w.app.state.person_id_for_token("tok-ana-late");
    assert_eq!(
        stored(&s.w.app, &first).await.person_id.as_deref(),
        Some(new.as_str())
    );
    // the household now lists the new id, and a second kit is a duplicate of the first
    let (status, again) =
        deliver(&s.w.app, &s.w.vol_a, &s.w.a, req(&unsampled(), &new, "kit")).await;
    assert_eq!(status, StatusCode::CREATED, "{again}");
    assert_eq!(again["status"], "in_review");
    assert_eq!(again["reasons"][0]["previous_delivery_id"], first);
    // an offline delivery that still carries the old id is the same person (`former_ids`)
    let (status, late) = deliver(
        &s.w.app,
        &s.w.vol_a,
        &s.w.a,
        req(&unsampled(), &s.p1, "kit"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{late}");
    assert_eq!(late["status"], "in_review");
    assert_eq!(
        stored(&s.w.app, late["delivery_id"].as_str().unwrap())
            .await
            .person_id
            .as_deref(),
        Some(new.as_str())
    );
    // an id that was never theirs still leads nowhere
    let (status, _) = deliver(
        &s.w.app,
        &s.w.vol_a,
        &s.w.a,
        req(&unsampled(), &new_id(), "kit"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

mod admin_tests {
    use super::*;
    use crate::deliveries::{self, ItemSpec};
    use crate::events::{self as events_mod, EventDoc, EventStatus};
    use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, audit_actions, new_id};
    use crate::volunteers::GlobalRole;

    fn spec(scope: &str, extra: Value) -> Value {
        let mut body = json!({ "scope": scope, "once_per_event": false, "high_value": false });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        body
    }

    fn items_body(items: Value) -> Option<Value> {
        Some(json!({ "items": items }))
    }

    struct World {
        app: TestApp,
        admin: Actor,
        volunteer: Actor,
        shelter: String,
    }

    async fn world() -> World {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.create_shelter(&admin, "Ginásio", ORIGIN).await;
        let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let volunteer = app.join(&code, "Vol").await;
        World {
            app,
            admin,
            volunteer,
            shelter,
        }
    }

    async fn put_items_as(w: &World, who: &Actor, body: Option<Value>) -> (StatusCode, Value) {
        w.app
            .send(
                "PUT",
                &format!("/v1/shelters/{}/items", w.shelter),
                &who.secret,
                body,
            )
            .await
    }

    async fn close_event_of(app: &TestApp, shelter_id: &str) {
        let shelter: ShelterDoc = store::get(&app.state.db, shelters::COLLECTION, shelter_id)
            .await
            .unwrap()
            .unwrap();
        let event_id = shelter.event_id.unwrap();
        let mut event: EventDoc = store::get(&app.state.db, events_mod::COLLECTION, &event_id)
            .await
            .unwrap()
            .unwrap();
        event.status = EventStatus::Closed;
        event.closed_at = Some(store::now());
        store::replace(&app.state.db, events_mod::COLLECTION, &event_id, &event)
            .await
            .unwrap();
    }

    // -- items --

    #[tokio::test]
    async fn put_items_saves_returns_replaces_and_audits() {
        let w = world().await;
        let first = json!({
            "cesta": spec("per_household", json!({ "period_days": 7 })),
            "colchão": spec("per_person", json!({ "once_per_event": true, "high_value": true, "period_days": 3 })),
        });
        let (status, body) = put_items_as(&w, &w.admin, items_body(first)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        // `period_days` is dropped where `once_per_event` applies.
        assert_eq!(
            body["items"],
            json!({
                "cesta": { "scope": "per_household", "period_days": 7, "once_per_event": false, "high_value": false },
                "colchão": { "scope": "per_person", "once_per_event": true, "high_value": true },
            })
        );
        let (_, view) = w
            .app
            .send(
                "GET",
                &format!("/v1/shelters/{}", w.shelter),
                &w.admin.secret,
                None,
            )
            .await;
        assert_eq!(view["shelter"]["items"], body["items"]);

        let second = json!({ "kit": spec("per_person", json!({})) });
        let (status, body) = put_items_as(&w, &w.admin, items_body(second)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["items"],
            json!({ "kit": { "scope": "per_person", "once_per_event": false, "high_value": false } })
        );
        let stored: ShelterDoc = store::get(&w.app.state.db, shelters::COLLECTION, &w.shelter)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.items.keys().collect::<Vec<_>>(), vec!["kit"]);
        assert_eq!(
            stored.items["kit"],
            ItemSpec {
                scope: ItemScope::PerPerson,
                period_days: None,
                once_per_event: false,
                high_value: false
            }
        );

        let (status, body) = put_items_as(&w, &w.admin, items_body(json!({}))).await;
        assert_eq!((status, &body["items"]), (StatusCode::OK, &json!({})));
        let actions = audit_actions(&w.app.state.db).await;
        assert_eq!(
            actions.iter().filter(|a| *a == "shelter.items").count(),
            3,
            "{actions:?}"
        );
    }

    #[tokio::test]
    async fn put_items_refuses_invalid_lists_and_keeps_the_old_one() {
        let w = world().await;
        let keep = json!({ "cesta": spec("per_household", json!({})) });
        put_items_as(&w, &w.admin, items_body(keep.clone())).await;

        let many: serde_json::Map<String, Value> = (0..=MAX_ITEMS)
            .map(|n| (format!("item {n}"), spec("per_person", json!({}))))
            .collect();
        let cases = [
            ("item", json!({ "": spec("per_person", json!({})) })),
            ("item", json!({ "   ": spec("per_person", json!({})) })),
            (
                "item",
                json!({ "x".repeat(MAX_ITEM_NAME_CHARS + 1): spec("per_person", json!({})) }),
            ),
            ("item", json!({ "a\nb": spec("per_person", json!({})) })),
            (
                "item",
                json!({ "Cesta": spec("per_person", json!({})), " cesta ": spec("per_household", json!({})) }),
            ),
            (
                "period_days",
                json!({ "a": spec("per_person", json!({ "period_days": 0 })) }),
            ),
            (
                "period_days",
                json!({ "a": spec("per_person", json!({ "period_days": MAX_PERIOD_DAYS + 1 })) }),
            ),
            ("items", Value::Object(many)),
        ];
        for (field, items) in cases {
            let (status, body) = put_items_as(&w, &w.admin, items_body(items.clone())).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{items}");
            assert_eq!(body["error"], "invalid_request", "{items}");
            assert_eq!(body["detail"]["field"], field, "{items}");
        }
        // The list as it was, and a bad scope is refused at the door.
        let stored: ShelterDoc = store::get(&w.app.state.db, shelters::COLLECTION, &w.shelter)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.items.keys().collect::<Vec<_>>(), vec!["cesta"]);
        let (status, _) = put_items_as(
            &w,
            &w.admin,
            items_body(json!({ "a": spec("everyone", json!({})) })),
        )
        .await;
        assert!(status.is_client_error());
        // The limits themselves are accepted.
        let edge = json!({
            "x".repeat(MAX_ITEM_NAME_CHARS): spec("per_person", json!({ "period_days": MAX_PERIOD_DAYS })),
        });
        let (status, _) = put_items_as(&w, &w.admin, items_body(edge)).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn put_items_follows_the_permission_matrix() {
        let w = world().await;
        let body = items_body(json!({ "cesta": spec("per_household", json!({})) }));
        let civil_defense = w.app.actor("DC").await;
        w.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let validator = w.app.actor("Val").await;
        w.app.grant(&validator, GlobalRole::Validator).await;
        let stranger = w.app.actor("Ze").await;
        for who in [&w.volunteer, &civil_defense, &validator, &stranger] {
            let (status, answer) = put_items_as(&w, who, body.clone()).await;
            assert_eq!(
                (status, answer["error"].as_str()),
                (StatusCode::FORBIDDEN, Some("forbidden"))
            );
        }
        let (status, _, answer) = w
            .app
            .request(
                "PUT",
                &format!("/v1/shelters/{}/items", w.shelter),
                &[],
                body.clone(),
            )
            .await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::UNAUTHORIZED, Some("unauthorized"))
        );

        // Unconfirmed shelter: the admin configures (matrix 2.3).
        let (status, _) = put_items_as(&w, &w.admin, body.clone()).await;
        assert_eq!(status, StatusCode::OK);
        // Confirmed shelter: the admin too.
        let confirmed = w
            .app
            .confirmed_shelter(
                &w.admin,
                "Escola",
                crate::test_support::north_of_origin(5000.0),
            )
            .await;
        let (status, answer) = w
            .app
            .send(
                "PUT",
                &format!("/v1/shelters/{confirmed}/items"),
                &w.admin.secret,
                body,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
    }

    #[tokio::test]
    async fn put_items_on_a_closed_event_is_409() {
        let w = world().await;
        close_event_of(&w.app, &w.shelter).await;
        let (status, body) = put_items_as(
            &w,
            &w.admin,
            items_body(json!({ "cesta": spec("per_household", json!({})) })),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::CONFLICT, Some("event_closed"))
        );
    }

    // -- list and review --

    struct Seed<'a> {
        id: &'a str,
        shelter: &'a str,
        person: Option<&'a str>,
        by: &'a Actor,
        status: DeliveryStatus,
        at: &'a str,
        reasons: Vec<Reason>,
    }

    async fn seed(w: &World, s: Seed<'_>) {
        let at: FirestoreInstant = s.at.parse().unwrap();
        let doc = DeliveryDoc {
            shelter_id: s.shelter.into(),
            shelter_name: "x".into(),
            event_id: None,
            person_id: s.person.map(str::to_string),
            household_id: Some("h1".into()),
            household_pseudonym: None,
            card_code: Some("ABCD2345".into()),
            item: "cesta".into(),
            scope: ItemScope::PerHousehold,
            quantity: 1,
            delivered_by: s.by.id.clone(),
            delivered_at: at,
            received_at: at,
            verification: Verification::Card,
            approved_by: None,
            status: s.status,
            reasons: s.reasons,
            offline_claimed: false,
            delivered_at_clamped: false,
            reviewed_by: None,
            review_note: None,
            pseudonymized: false,
        };
        store::replace(&w.app.state.db, deliveries::COLLECTION, s.id, &doc)
            .await
            .unwrap();
    }

    async fn seed_person(w: &World, id: &str, name: &str) {
        let mut person = crate::exits::fixtures::person(crate::people::Level::Verified, None);
        person.name = Some(name.into());
        crate::exits::fixtures::put(&w.app, id, &person).await;
    }

    async fn list_as(w: &World, who: &Actor, shelter: &str, status: &str) -> (StatusCode, Value) {
        w.app
            .send(
                "GET",
                &format!("/v1/shelters/{shelter}/deliveries?status={status}"),
                &who.secret,
                None,
            )
            .await
    }

    #[tokio::test]
    async fn list_filters_orders_names_and_never_shows_the_person_id() {
        let w = world().await;
        let person = new_id();
        seed_person(&w, &person, "Maria Souza").await;
        for (id, status, at) in [
            ("d-old", DeliveryStatus::InReview, "2026-10-21T10:00:00Z"),
            ("d-new", DeliveryStatus::InReview, "2026-10-21T12:00:00Z"),
            ("d-mid", DeliveryStatus::InReview, "2026-10-21T11:00:00Z"),
            ("d-ok", DeliveryStatus::Ok, "2026-10-21T13:00:00Z"),
            ("d-irr", DeliveryStatus::Irregular, "2026-10-21T14:00:00Z"),
            ("d-rok", DeliveryStatus::ReviewedOk, "2026-10-21T15:00:00Z"),
        ] {
            seed(
                &w,
                Seed {
                    id,
                    shelter: &w.shelter,
                    person: Some(&person),
                    by: &w.volunteer,
                    status,
                    at,
                    reasons: vec![],
                },
            )
            .await;
        }
        // Another shelter's delivery never shows.
        seed(
            &w,
            Seed {
                id: "d-other",
                shelter: "other-shelter",
                person: Some(&person),
                by: &w.volunteer,
                status: DeliveryStatus::InReview,
                at: "2026-10-21T16:00:00Z",
                reasons: vec![],
            },
        )
        .await;

        let (status, body) = list_as(&w, &w.admin, &w.shelter, "in_review").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let ids: Vec<&str> = body["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["d-new", "d-mid", "d-old"]);
        let first = &body["deliveries"][0];
        assert_eq!(first["person_name"], "Maria Souza");
        assert_eq!(first["delivered_by_name"], "Vol");
        assert_eq!(first["card_code"], "ABCD2345");
        assert_eq!(first["item"], "cesta");
        assert!(!body.to_string().contains(&person), "person_id leaked");
        assert!(first.get("person_id").is_none());

        for (filter, id) in [
            ("ok", "d-ok"),
            ("irregular", "d-irr"),
            ("reviewed_ok", "d-rok"),
        ] {
            let (_, body) = list_as(&w, &w.admin, &w.shelter, filter).await;
            assert_eq!(body["deliveries"].as_array().unwrap().len(), 1, "{filter}");
            assert_eq!(body["deliveries"][0]["id"], id);
        }
    }

    #[tokio::test]
    async fn list_is_one_page_of_at_most_the_limit() {
        let w = world().await;
        for n in 0..(LIST_LIMIT + 3) {
            seed(
                &w,
                Seed {
                    id: &format!("d{n}"),
                    shelter: &w.shelter,
                    person: None,
                    by: &w.volunteer,
                    status: DeliveryStatus::InReview,
                    at: "2026-10-21T10:00:00Z",
                    reasons: vec![],
                },
            )
            .await;
        }
        let (_, body) = list_as(&w, &w.admin, &w.shelter, "in_review").await;
        assert_eq!(
            body["deliveries"].as_array().unwrap().len(),
            LIST_LIMIT as usize
        );
        assert_eq!(body["has_more"], true, "{body}");
    }

    #[tokio::test]
    async fn list_says_when_the_page_is_all_there_is() {
        let w = world().await;
        for n in 0..LIST_LIMIT {
            seed(
                &w,
                Seed {
                    id: &format!("d{n}"),
                    shelter: &w.shelter,
                    person: None,
                    by: &w.volunteer,
                    status: DeliveryStatus::InReview,
                    at: "2026-10-21T10:00:00Z",
                    reasons: vec![],
                },
            )
            .await;
        }
        let (_, body) = list_as(&w, &w.admin, &w.shelter, "in_review").await;
        assert_eq!(
            body["deliveries"].as_array().unwrap().len(),
            LIST_LIMIT as usize
        );
        assert_eq!(body["has_more"], false, "{body}");
        let (_, none) = list_as(&w, &w.admin, &w.shelter, "irregular").await;
        assert_eq!(none["has_more"], false, "{none}");
    }

    #[tokio::test]
    async fn list_refuses_a_missing_or_unknown_status_and_non_admins() {
        let w = world().await;
        for query in ["", "?status=", "?status=nope"] {
            let (status, body) = w
                .app
                .send(
                    "GET",
                    &format!("/v1/shelters/{}/deliveries{query}", w.shelter),
                    &w.admin.secret,
                    None,
                )
                .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
            assert_eq!(body["detail"]["field"], "status", "{query}");
        }
        let civil_defense = w.app.actor("DC").await;
        w.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let stranger = w.app.actor("Ze").await;
        for who in [&w.volunteer, &civil_defense, &stranger] {
            let (status, _) = list_as(&w, who, &w.shelter, "in_review").await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }
        // Admin of another shelter does not see this one.
        let other_admin = w.app.actor("Other").await;
        w.app
            .create_shelter(
                &other_admin,
                "Outro",
                crate::test_support::north_of_origin(9000.0),
            )
            .await;
        let (status, _) = list_as(&w, &other_admin, &w.shelter, "in_review").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = w
            .app
            .request(
                "GET",
                &format!("/v1/shelters/{}/deliveries?status=ok", w.shelter),
                &[],
                None,
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_other_shelter_is_named_only_when_it_is_confirmed() {
        let w = world().await;
        let validator = w.app.actor("Val").await;
        w.app.grant(&validator, GlobalRole::Validator).await;
        let far = w.app.actor("Far").await;
        let unconfirmed = w
            .app
            .create_shelter(
                &far,
                "Abrigo escondido",
                crate::test_support::north_of_origin(4000.0),
            )
            .await;
        let confirmed = w
            .app
            .confirmed_shelter(
                &far,
                "Abrigo aberto",
                crate::test_support::north_of_origin(8000.0),
            )
            .await;
        for (id, shelter) in [
            ("p-unc", &unconfirmed),
            ("p-con", &confirmed),
            ("p-own", &w.shelter),
        ] {
            seed(
                &w,
                Seed {
                    id,
                    shelter,
                    person: None,
                    by: &w.volunteer,
                    status: DeliveryStatus::Ok,
                    at: "2026-10-20T10:00:00Z",
                    reasons: vec![],
                },
            )
            .await;
        }
        for (id, previous) in [
            ("d1", "p-unc"),
            ("d2", "p-con"),
            ("d3", "p-own"),
            ("d4", "gone"),
        ] {
            seed(
                &w,
                Seed {
                    id,
                    shelter: &w.shelter,
                    person: None,
                    by: &w.volunteer,
                    status: DeliveryStatus::InReview,
                    at: "2026-10-21T10:00:00Z",
                    reasons: vec![Reason {
                        kind: ReasonKind::DuplicateInPeriod,
                        previous_delivery_id: Some(previous.into()),
                    }],
                },
            )
            .await;
        }
        let (_, body) = list_as(&w, &w.admin, &w.shelter, "in_review").await;
        let reason_of = |id: &str| {
            body["deliveries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|d| d["id"] == id)
                .unwrap()["reasons"][0]
                .clone()
        };
        let unc = reason_of("d1");
        assert_eq!(unc["previous_delivery_id"], "p-unc");
        // This shelter is not confirmed: of another shelter's delivery it is told nothing.
        for other in [unc, reason_of("d2")] {
            assert!(other.get("previous_delivered_at").is_none(), "{other}");
            assert!(other.get("previous_shelter_name").is_none(), "{other}");
        }
        assert!(!body.to_string().contains("Abrigo escondido"));
        assert!(!body.to_string().contains("Abrigo aberto"));
        // Its own delivery it is told, with the date.
        assert_eq!(reason_of("d3")["previous_shelter_name"], "Ginásio");
        assert!(reason_of("d3").get("previous_delivered_at").is_some());
        let gone = reason_of("d4");
        assert_eq!(gone["previous_delivery_id"], "gone");
        assert!(gone.get("previous_delivered_at").is_none());
    }

    async fn review_as(w: &World, who: &Actor, id: &str, body: Value) -> (StatusCode, Value) {
        w.app
            .send(
                "POST",
                &format!("/v1/deliveries/{id}/review"),
                &who.secret,
                Some(body),
            )
            .await
    }

    async fn stored(w: &World, id: &str) -> DeliveryDoc {
        store::get(&w.app.state.db, deliveries::COLLECTION, id)
            .await
            .unwrap()
            .unwrap()
    }

    async fn in_review(w: &World, id: &str) {
        seed(
            w,
            Seed {
                id,
                shelter: &w.shelter,
                person: None,
                by: &w.volunteer,
                status: DeliveryStatus::InReview,
                at: "2026-10-21T10:00:00Z",
                reasons: vec![],
            },
        )
        .await;
    }

    #[tokio::test]
    async fn review_decides_records_the_admin_and_audits_with_fraud_prevention() {
        let w = world().await;
        in_review(&w, "d1").await;
        in_review(&w, "d2").await;
        let (status, body) = review_as(
            &w,
            &w.admin,
            "d1",
            json!({ "decision": "reviewed_ok", "note": "  conferido com a família  " }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body,
            json!({ "delivery_id": "d1", "status": "reviewed_ok" })
        );
        let d1 = stored(&w, "d1").await;
        assert_eq!(d1.status, DeliveryStatus::ReviewedOk);
        assert_eq!(d1.reviewed_by.as_deref(), Some(w.admin.id.as_str()));
        assert_eq!(d1.review_note.as_deref(), Some("conferido com a família"));

        let (status, _) = review_as(
            &w,
            &w.admin,
            "d2",
            json!({ "decision": "irregular", "note": "" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let d2 = stored(&w, "d2").await;
        assert_eq!(
            (d2.status, d2.review_note),
            (DeliveryStatus::Irregular, None)
        );

        let entries: Vec<Value> = all_docs(&w.app.state.db, "audit_log")
            .await
            .into_iter()
            .filter(|e| e["action"] == "delivery.review")
            .collect();
        assert_eq!(entries.len(), 2);
        for entry in &entries {
            assert_eq!(entry["legal_basis"], "fraud_prevention");
            assert_eq!(entry["volunteer_id"], w.admin.id.as_str());
            assert_eq!(entry["shelter_id"], w.shelter.as_str());
            assert_eq!(entry["target_kind"], "delivery");
        }
        // Reviewed deliveries leave the in-review list and show in the other one.
        let (_, body) = list_as(&w, &w.admin, &w.shelter, "in_review").await;
        assert_eq!(body["deliveries"], json!([]));
        let (_, body) = list_as(&w, &w.admin, &w.shelter, "reviewed_ok").await;
        assert_eq!(
            body["deliveries"][0]["review_note"],
            "conferido com a família"
        );
    }

    #[tokio::test]
    async fn review_only_takes_deliveries_in_review() {
        let w = world().await;
        for (id, status) in [
            ("ok", DeliveryStatus::Ok),
            ("rok", DeliveryStatus::ReviewedOk),
            ("irr", DeliveryStatus::Irregular),
        ] {
            seed(
                &w,
                Seed {
                    id,
                    shelter: &w.shelter,
                    person: None,
                    by: &w.volunteer,
                    status,
                    at: "2026-10-21T10:00:00Z",
                    reasons: vec![],
                },
            )
            .await;
            let (code, body) = review_as(
                &w,
                &w.admin,
                id,
                json!({ "decision": "irregular", "note": "x" }),
            )
            .await;
            assert_eq!(
                (code, body["error"].as_str()),
                (StatusCode::CONFLICT, Some("delivery_not_in_review")),
                "{id}"
            );
            assert_eq!(stored(&w, id).await.status, status);
        }
        let (code, body) = review_as(
            &w,
            &w.admin,
            "missing",
            json!({ "decision": "irregular", "note": "" }),
        )
        .await;
        assert_eq!(
            (code, body["error"].as_str()),
            (StatusCode::NOT_FOUND, Some("delivery_not_found"))
        );
    }

    #[tokio::test]
    async fn review_refuses_bad_bodies_and_non_admins() {
        let w = world().await;
        in_review(&w, "d1").await;
        let (code, _) =
            review_as(&w, &w.admin, "d1", json!({ "decision": "ok", "note": "" })).await;
        assert!(code.is_client_error());
        let (code, body) = review_as(
            &w,
            &w.admin,
            "d1",
            json!({ "decision": "irregular", "note": "x".repeat(MAX_NOTE_CHARS + 1) }),
        )
        .await;
        assert_eq!(
            (code, body["detail"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("note"))
        );

        let civil_defense = w.app.actor("DC").await;
        w.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let stranger = w.app.actor("Ze").await;
        let other_admin = w.app.actor("Other").await;
        w.app
            .create_shelter(
                &other_admin,
                "Outro",
                crate::test_support::north_of_origin(9000.0),
            )
            .await;
        for who in [&w.volunteer, &civil_defense, &stranger, &other_admin] {
            let (code, body) = review_as(
                &w,
                who,
                "d1",
                json!({ "decision": "reviewed_ok", "note": "" }),
            )
            .await;
            assert_eq!(
                (code, body["error"].as_str()),
                (StatusCode::FORBIDDEN, Some("forbidden"))
            );
        }
        assert_eq!(stored(&w, "d1").await.status, DeliveryStatus::InReview);
        let (code, _, _) = w
            .app
            .request(
                "POST",
                "/v1/deliveries/d1/review",
                &[],
                Some(json!({ "decision": "irregular", "note": "" })),
            )
            .await;
        assert_eq!(code, StatusCode::UNAUTHORIZED);

        close_event_of(&w.app, &w.shelter).await;
        let (code, body) = review_as(
            &w,
            &w.admin,
            "d1",
            json!({ "decision": "reviewed_ok", "note": "" }),
        )
        .await;
        assert_eq!(
            (code, body["error"].as_str()),
            (StatusCode::CONFLICT, Some("event_closed"))
        );
    }

    #[tokio::test]
    async fn two_simultaneous_reviews_one_wins() {
        let w = world().await;
        let second_admin = {
            let code = w.app.invite(&w.admin, &w.shelter, "qr", "admin").await;
            w.app.join(&code, "Admin2").await
        };
        for round in 0..3 {
            let id = format!("race-{round}");
            in_review(&w, &id).await;
            let (a, b) = tokio::join!(
                review_as(
                    &w,
                    &w.admin,
                    &id,
                    json!({ "decision": "reviewed_ok", "note": "a" })
                ),
                review_as(
                    &w,
                    &second_admin,
                    &id,
                    json!({ "decision": "irregular", "note": "b" })
                ),
            );
            let mut codes = [a.0, b.0];
            codes.sort();
            assert_eq!(codes, [StatusCode::OK, StatusCode::CONFLICT], "{a:?} {b:?}");
            let loser = if a.0 == StatusCode::OK { &b } else { &a };
            assert_eq!(loser.1["error"], "delivery_not_in_review");
            // The stored decision is the winner's, whole.
            let doc = stored(&w, &id).await;
            let (winner, note) = if a.0 == StatusCode::OK {
                (&w.admin, "a")
            } else {
                (&second_admin, "b")
            };
            assert_eq!(doc.reviewed_by.as_deref(), Some(winner.id.as_str()));
            assert_eq!(doc.review_note.as_deref(), Some(note));
        }
        let reviews = all_docs(&w.app.state.db, "audit_log")
            .await
            .into_iter()
            .filter(|e| e["action"] == "delivery.review")
            .count();
        assert_eq!(reviews, 3, "one audit entry per decided delivery");
    }
}

mod domain_tests {
    use super::*;
    use crate::shelters::{Address, ShelterStatus};
    use crate::store;
    use crate::test_support::new_id;

    const DAYS: i64 = DAY_MICROS;

    fn spec(scope: ItemScope, period: Option<u32>, once: bool, high: bool) -> ItemSpec {
        ItemSpec {
            scope,
            period_days: period,
            once_per_event: once,
            high_value: high,
        }
    }

    fn shelter(confirmation: Confirmation) -> ShelterDoc {
        let now = store::now();
        ShelterDoc {
            id: None,
            name: "Abrigo".into(),
            lat: 0.0,
            lon: 0.0,
            accuracy_m: 5.0,
            geohash: String::new(),
            address: Address::default(),
            event_id: Some("e1".into()),
            capacity: None,
            created_by: "v".into(),
            client_created_at: now,
            created_at: now,
            status: ShelterStatus::Open,
            confirmation,
            confirmation_type: None,
            can_endorse: false,
            endorsements: vec![],
            revocations: vec![],
            flagged: false,
            other_site_reason: None,
            conflicts_with: None,
            items: Items::new(),
        }
    }

    fn write(decision: Decision) -> Write {
        let now = store::now();
        Write {
            id: "d2".into(),
            shelter_id: "s1".into(),
            shelter: shelter(Confirmation::Unconfirmed),
            volunteer_id: "vol".into(),
            card_code: "ABCD2345".into(),
            person_id: "p1".into(),
            household_id: "h1".into(),
            item: "cesta".into(),
            spec: spec(ItemScope::PerHousehold, None, false, false),
            quantity: 1,
            delivered_at: now,
            decision,
            offline_claimed: false,
            delivered_at_clamped: false,
            received_at: now,
            capture_id: None,
        }
    }

    /// A fixed instant plus `micros`, so differences between two of them are exact.
    fn at(micros: i64) -> FirestoreInstant {
        FirestoreInstant::from_microsecond(1_800_000_000_000_000 + micros).unwrap()
    }

    // ---- the draw and the decision ----

    #[test]
    fn the_draw_is_deterministic_and_close_to_the_rate() {
        let id = new_id();
        assert_eq!(is_sampled(&id, 0.05), is_sampled(&id, 0.05));
        let ids: Vec<String> = (0..20_000).map(|_| new_id()).collect();
        let drawn = ids.iter().filter(|id| is_sampled(id, 0.05)).count();
        assert!((700..=1300).contains(&drawn), "{drawn} of 20000");
        assert!(ids.iter().all(|id| !is_sampled(id, 0.0)));
        assert!(ids.iter().all(|id| is_sampled(id, 1.0)));
        // growing the rate never undraws an id
        assert!(
            ids.iter()
                .all(|id| !is_sampled(id, 0.05) || is_sampled(id, 0.5))
        );
    }

    #[test]
    fn the_decision_table() {
        use Decision::*;
        // (high_value, sampled, unavailable, admin_approval, has_capture)
        let cases = [
            ((false, false, false, false, false), Card),
            ((false, true, false, false, false), Ask),
            ((true, false, false, false, false), Ask),
            ((true, false, false, false, true), Revalidate),
            ((false, false, false, false, true), Revalidate),
            ((true, true, false, true, true), AdminApproval),
            ((false, false, false, true, false), AdminApproval),
            ((true, false, true, false, false), Unavailable),
            ((true, false, true, true, false), AdminApproval),
            ((false, true, true, false, false), Card),
            ((false, false, true, false, true), Card),
        ];
        for ((high, sampled, unavailable, admin, capture), expected) in cases {
            assert_eq!(
                decide(high, sampled, unavailable, admin, capture),
                expected,
                "{high} {sampled} {unavailable} {admin} {capture}"
            );
        }
    }

    #[test]
    fn the_period_is_strictly_shorter_than_period_days_and_ignored_for_once_per_event() {
        let base = store::now();
        let week = spec(ItemScope::PerPerson, None, false, false);
        let at_ = |micros: i64| {
            FirestoreInstant::from_microsecond(base.as_microsecond() + micros).unwrap()
        };
        assert!(within_period(&week, base, at_(7 * DAYS - 1)));
        assert!(!within_period(&week, base, at_(7 * DAYS)));
        assert!(
            within_period(&week, at_(7 * DAYS - 1), base),
            "either order"
        );
        let three = spec(ItemScope::PerPerson, Some(3), false, false);
        assert!(!within_period(&three, base, at_(3 * DAYS)));
        let once = spec(ItemScope::PerPerson, Some(3), true, false);
        assert!(within_period(&once, base, at_(400 * DAYS)));
    }

    #[test]
    fn the_offline_claim_needs_a_real_wait_and_a_future_clock_is_cut_at_receipt() {
        let received = at(0);
        assert!(offline_claim_accepted(
            true,
            at(-OFFLINE_MIN_DELAY_MICROS),
            received
        ));
        assert!(!offline_claim_accepted(
            true,
            at(-OFFLINE_MIN_DELAY_MICROS + 1),
            received
        ));
        assert!(!offline_claim_accepted(false, at(-60_000_000), received));

        assert_eq!(
            plausible_delivered_at(at(1), received),
            Some((received, true))
        );
        assert_eq!(
            plausible_delivered_at(at(-MAX_AGE_MICROS), received),
            Some((at(-MAX_AGE_MICROS), false))
        );
        assert_eq!(
            plausible_delivered_at(at(-MAX_AGE_MICROS - 1), received),
            None
        );
    }

    #[test]
    fn what_is_told_of_the_other_delivery_follows_who_asks() {
        let unconfirmed = shelter(Confirmation::Unconfirmed);
        let confirmed = shelter(Confirmation::Confirmed);
        // -> (date, name)
        assert_eq!(
            previous_visibility(&unconfirmed, "a", "a", None),
            (true, true)
        );
        assert_eq!(
            previous_visibility(&unconfirmed, "a", "b", Some(&confirmed)),
            (false, false)
        );
        assert_eq!(
            previous_visibility(&confirmed, "a", "b", Some(&unconfirmed)),
            (true, false)
        );
        assert_eq!(
            previous_visibility(&confirmed, "a", "b", Some(&confirmed)),
            (true, true)
        );
        assert_eq!(
            previous_visibility(&confirmed, "a", "b", None),
            (true, false)
        );
    }

    // ---- invariants (matrix of Q03: what each operation may do to them) ----

    /// A repeated take is flagged, never refused: whatever the decision, a duplicate is a reason
    /// and the delivery is recorded in review.
    #[test]
    fn a_repeated_take_is_flagged_never_refused() {
        for decision in [
            Decision::Card,
            Decision::Revalidate,
            Decision::AdminApproval,
            Decision::Unavailable,
        ] {
            let w = write(decision);
            let doc = w.delivery(w.reasons(Some("d1".into())));
            assert_eq!(doc.status, DeliveryStatus::InReview, "{decision:?}");
            assert!(
                doc.reasons
                    .iter()
                    .any(|r| r.kind == ReasonKind::DuplicateInPeriod
                        && r.previous_delivery_id.as_deref() == Some("d1")),
                "{decision:?}"
            );
        }
        let w = write(Decision::Card);
        assert_eq!(w.delivery(w.reasons(None)).status, DeliveryStatus::Ok);
    }

    #[test]
    fn a_duplicate_and_a_liveness_that_could_not_be_done_carry_both_reasons() {
        let w = write(Decision::Unavailable);
        let kinds: Vec<_> = w
            .reasons(Some("d1".into()))
            .iter()
            .map(|r| r.kind)
            .collect();
        assert_eq!(
            kinds,
            [
                ReasonKind::DuplicateInPeriod,
                ReasonKind::LivenessUnavailable
            ]
        );
    }

    /// A repeated item does not repeat its effect: a delivery is never its own duplicate, so the
    /// same request sent again is not flagged against itself.
    #[test]
    fn a_delivery_is_never_its_own_duplicate() {
        let mut w = write(Decision::Card);
        w.delivered_at = at(0);
        assert!(!counts_against(&w, &w.id, at(0)));
        assert!(counts_against(&w, "d1", at(0)));
        assert!(counts_against(&w, "d1", at(-7 * DAYS + 1)));
        assert!(!counts_against(&w, "d1", at(-7 * DAYS)));
    }

    /// A replay does not widen access: the stored delivery answers the same shelter and volunteer
    /// again; anyone else using the id gets a conflict, never the stored data.
    #[test]
    fn a_replay_answers_only_the_volunteer_and_shelter_that_made_it() {
        let doc = write(Decision::Card).delivery(vec![]);
        assert!(same_delivery(doc.clone(), "s1", "vol").is_ok());
        for (shelter_id, volunteer_id) in [("s1", "other"), ("s2", "vol"), ("s2", "other")] {
            let error = same_delivery(doc.clone(), shelter_id, volunteer_id).unwrap_err();
            assert_eq!(error.code, "delivery_id_taken");
        }
    }

    /// A repeated review does not repeat its effect: the second neither changes the decision nor
    /// replaces the reviewer and the note.
    #[test]
    fn a_review_decides_once() {
        let w = write(Decision::Card);
        let mut doc = w.delivery(w.reasons(Some("d1".into())));
        doc.review(ReviewDecision::Irregular, "admin1", Some("first".into()))
            .unwrap();
        assert_eq!(doc.status, DeliveryStatus::Irregular);
        for decision in [ReviewDecision::ReviewedOk, ReviewDecision::Irregular] {
            let error = doc
                .review(decision, "admin2", Some("second".into()))
                .unwrap_err();
            assert_eq!(error.code, "delivery_not_in_review");
        }
        assert_eq!(doc.status, DeliveryStatus::Irregular);
        assert_eq!(doc.reviewed_by.as_deref(), Some("admin1"));
        assert_eq!(doc.review_note.as_deref(), Some("first"));
    }

    #[test]
    fn only_a_delivery_in_review_can_be_reviewed() {
        let mut ok = write(Decision::Card).delivery(vec![]);
        assert!(
            ok.review(ReviewDecision::ReviewedOk, "admin", None)
                .is_err()
        );
        assert_eq!(ok.status, DeliveryStatus::Ok);
        assert!(ok.reviewed_by.is_none());
    }

    #[test]
    fn the_audit_action_says_how_the_delivery_was_vouched_for() {
        let mut w = write(Decision::Card);
        assert_eq!(w.audit_parts(), ["create", "", ""]);
        w.offline_claimed = true;
        w.delivered_at_clamped = true;
        assert_eq!(w.audit_parts(), ["create", "_offline", "_clamped"]);
        let admin = write(Decision::AdminApproval);
        assert_eq!(admin.audit_parts()[0], "admin_approval");
        assert_eq!(admin.delivery(vec![]).approved_by.as_deref(), Some("vol"));
    }
}
