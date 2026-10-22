use super::domain::{is_canonical, normalize};
use super::*;
use crate::households::tests::{World, household, person, world};
use crate::store::{self, now};
use crate::test_support::{Actor, new_id};
use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde_json::{Value, json};

fn error_of(body: &Value) -> Option<&str> {
    body["error"].as_str()
}

async fn issue(w: &World, who: &Actor, household_id: &str, code: &str) -> (StatusCode, Value) {
    w.app
        .send(
            "POST",
            &format!("/v1/households/{household_id}/cards"),
            &who.secret,
            Some(json!({ "code": code, "client_issued_at": "2026-10-21T10:00:00Z" })),
        )
        .await
}

async fn cancel(w: &World, who: &Actor, code: &str, body: Value) -> (StatusCode, Value) {
    w.app
        .send(
            "POST",
            &format!("/v1/cards/{code}/cancel"),
            &who.secret,
            Some(body),
        )
        .await
}

async fn read(w: &World, who: &Actor, code: &str) -> (StatusCode, Value) {
    w.app
        .send("GET", &format!("/v1/cards/{code}"), &who.secret, None)
        .await
}

async fn one_household(w: &World) -> String {
    let ana = person(&w.app, "Ana", Some(&w.a)).await;
    household(&w.app, &w.vol_a, &w.a, &[&ana]).await
}

/// A repeated cancel does not repeat its effect: the first time and substitute stay.
#[test]
fn canceling_twice_keeps_the_first_cancel() {
    let at = now();
    let later = FirestoreInstant::from_microsecond(at.as_microsecond() + 1_000_000).unwrap();
    let mut card = CardDoc {
        code: Some("ABCD2345".into()),
        household_id: "h".into(),
        shelter_id: "s".into(),
        status: CardStatus::Active,
        issued_by: "v".into(),
        client_issued_at: at,
        created_at: at,
        canceled_at: None,
        replaced_by: None,
    };
    assert!(card.cancel(Some("WXYZ2345".into()), at));
    assert!(!card.cancel(None, later));
    assert_eq!(card.canceled_at, Some(at));
    assert_eq!(card.replaced_by.as_deref(), Some("WXYZ2345"));
}

#[test]
fn crockford_codes_normalize_the_way_people_type_them() {
    assert_eq!(normalize("abcd2345", "code").unwrap(), "ABCD2345");
    assert_eq!(normalize(" OIl0Il0O ", "code").unwrap(), "01101100");
    for bad in [
        "",
        "ABCD234",
        "ABCD23456",
        "ABCD234U",
        "ABCD-345",
        "ABCD234é",
    ] {
        assert!(normalize(bad, "code").is_err(), "{bad}");
    }
    assert!(is_canonical("0123ABCZ"));
    assert!(!is_canonical("abcd2345"));
    assert!(!is_canonical("ABCD234I"));
}

#[tokio::test]
async fn issues_a_card_and_the_household_shows_it() {
    let w = world().await;
    let id = one_household(&w).await;
    let (status, card) = issue(&w, &w.vol_a, &id, "ABCD2345").await;
    assert_eq!(status, StatusCode::CREATED, "{card}");
    assert_eq!(card["code"], json!("ABCD2345"));
    assert_eq!(card["status"], json!("active"));
    assert_eq!(card["household_id"], json!(id));
    let (_, view) = w
        .app
        .send(
            "GET",
            &format!("/v1/households/{id}"),
            &w.vol_a.secret,
            None,
        )
        .await;
    assert_eq!(view["card_code"], json!("ABCD2345"));
    let stored: CardDoc = store::get(&w.app.state.db, COLLECTION, "ABCD2345")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.shelter_id, w.a);
    assert_eq!(stored.issued_by, w.vol_a.id);
}

// Criterion 35: a card from another shelter reads fine, and shows no shelter or history.
#[tokio::test]
async fn any_member_of_any_shelter_reads_a_card_without_shelter_or_history() {
    let w = world().await;
    let id = one_household(&w).await;
    issue(&w, &w.vol_a, &id, "ABCD2345").await;
    for who in [&w.vol_a, &w.admin_a, &w.vol_b, &w.admin_b] {
        let (status, card) = read(&w, who, "abcd2345").await;
        assert_eq!(status, StatusCode::OK, "{card}");
        let text = card.to_string();
        for hidden in [w.a.as_str(), "shelter", "stays", "history"] {
            assert!(!text.contains(hidden), "{hidden} in {text}");
        }
        assert_eq!(card["members"][0]["name"], json!("Ana"));
        assert_eq!(card["members"][0]["level"], json!("no_biometrics"));
    }
    for who in [&w.outsider, &w.validator, &w.civil_defense] {
        assert_eq!(read(&w, who, "ABCD2345").await.0, StatusCode::FORBIDDEN);
    }
    let (status, body) = read(&w, &w.vol_a, "ZZZZ2345").await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::NOT_FOUND, Some("card_not_found"))
    );
    assert_eq!(
        read(&w, &w.vol_a, "short").await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn matrix_issuing_and_canceling_follow_the_household_shelter() {
    let w = world().await;
    for (n, (who, shelter)) in [
        (&w.vol_a, &w.a),
        (&w.admin_a, &w.a),
        (&w.vol_b, &w.b),
        (&w.admin_b, &w.b),
    ]
    .into_iter()
    .enumerate()
    {
        let p = person(&w.app, "P", Some(shelter)).await;
        let id = household(&w.app, who, shelter, &[&p]).await;
        let code = format!("CARD000{n}");
        assert_eq!(issue(&w, who, &id, &code).await.0, StatusCode::CREATED);
        assert_eq!(cancel(&w, who, &code, json!({})).await.0, StatusCode::OK);
    }
    let id = one_household(&w).await;
    for who in [
        &w.outsider,
        &w.validator,
        &w.civil_defense,
        &w.vol_b,
        &w.admin_b,
    ] {
        assert_eq!(
            issue(&w, who, &id, "FREE2345").await.0,
            StatusCode::FORBIDDEN
        );
    }
    issue(&w, &w.vol_a, &id, "ABCD2345").await;
    for who in [
        &w.outsider,
        &w.validator,
        &w.civil_defense,
        &w.vol_b,
        &w.admin_b,
    ] {
        let (status, _) = cancel(&w, who, "ABCD2345", json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    assert_eq!(
        read(&w, &w.vol_a, "ABCD2345").await.1["status"],
        json!("active")
    );
}

// Criterion 34: second card for the household.
#[tokio::test]
async fn a_second_active_card_is_a_conflict() {
    let w = world().await;
    let id = one_household(&w).await;
    assert_eq!(
        issue(&w, &w.vol_a, &id, "ABCD2345").await.0,
        StatusCode::CREATED
    );
    let (status, body) = issue(&w, &w.vol_a, &id, "WXYZ2345").await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::CONFLICT, Some("household_has_card"))
    );
    let all = crate::test_support::all_docs(&w.app.state.db, COLLECTION).await;
    assert_eq!(all.len(), 1);
}

#[tokio::test]
async fn the_same_card_again_is_answered_again_and_a_taken_code_is_refused() {
    let w = world().await;
    let id = one_household(&w).await;
    issue(&w, &w.vol_a, &id, "ABCD2345").await;
    assert_eq!(
        issue(&w, &w.vol_a, &id, "ABCD2345").await.0,
        StatusCode::CREATED
    );
    let other = one_household(&w).await;
    let (status, body) = issue(&w, &w.vol_a, &other, "ABCD2345").await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::CONFLICT, Some("code_taken"))
    );
}

#[tokio::test]
async fn invalid_codes_and_times_are_refused() {
    let w = world().await;
    let id = one_household(&w).await;
    for code in ["abcd2345", "ABCD234", "ABCD234U", "ABCDEFGHJ"] {
        let (status, body) = issue(&w, &w.vol_a, &id, code).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{code} {body}");
        assert_eq!(body["detail"]["field"], json!("code"));
    }
    let (status, body) = w
        .app
        .send(
            "POST",
            &format!("/v1/households/{id}/cards"),
            &w.vol_a.secret,
            Some(json!({ "code": "ABCD2345", "client_issued_at": "ontem" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, _) = issue(&w, &w.vol_a, &new_id(), "ABCD2345").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cancel_frees_the_household_and_the_code_is_never_reused() {
    let w = world().await;
    let id = one_household(&w).await;
    issue(&w, &w.vol_a, &id, "ABCD2345").await;
    // Typed by hand: lowercase and `O` for `0` still find the card.
    let (status, canceled) = cancel(
        &w,
        &w.vol_a,
        "abcd2345",
        json!({ "replaced_by": "wxyz2o45" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{canceled}");
    assert_eq!(canceled["status"], json!("canceled"));
    let stored: CardDoc = store::get(&w.app.state.db, COLLECTION, "ABCD2345")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, CardStatus::Canceled);
    assert!(stored.canceled_at.is_some());
    assert_eq!(stored.replaced_by.as_deref(), Some("WXYZ2045"));
    let (_, view) = w
        .app
        .send(
            "GET",
            &format!("/v1/households/{id}"),
            &w.vol_a.secret,
            None,
        )
        .await;
    assert_eq!(view["card_code"], Value::Null);
    // A canceled card still reads, as canceled.
    assert_eq!(
        read(&w, &w.vol_b, "ABCD2345").await.1["status"],
        json!("canceled")
    );
    // Canceling again changes nothing.
    let (status, again) = cancel(
        &w,
        &w.vol_a,
        "ABCD2345",
        json!({ "replaced_by": "ZZZZ2345" }),
    )
    .await;
    assert_eq!(
        (status, again["status"].as_str()),
        (StatusCode::OK, Some("canceled"))
    );
    let stored: CardDoc = store::get(&w.app.state.db, COLLECTION, "ABCD2345")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.replaced_by.as_deref(), Some("WXYZ2045"));
    // The substitute is issued next; the old code is gone for good.
    assert_eq!(
        issue(&w, &w.vol_a, &id, "WXYZ2045").await.0,
        StatusCode::CREATED
    );
    let (status, body) = issue(&w, &w.vol_a, &id, "ABCD2345").await;
    assert_eq!(
        (status, error_of(&body)),
        (StatusCode::CONFLICT, Some("code_taken"))
    );
    assert_eq!(
        read(&w, &w.vol_a, "WXYZ2045").await.1["status"],
        json!("active")
    );
}

#[tokio::test]
async fn cancel_without_a_body_and_with_a_bad_substitute() {
    let w = world().await;
    let id = one_household(&w).await;
    issue(&w, &w.vol_a, &id, "ABCD2345").await;
    let (status, body) = cancel(&w, &w.vol_a, "ABCD2345", json!({ "replaced_by": "nope" })).await;
    assert_eq!(
        (status, body["detail"]["field"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("replaced_by"))
    );
    let (status, _, _) = w
        .app
        .request(
            "POST",
            "/v1/cards/ABCD2345/cancel",
            &[("authorization", &format!("Bearer {}", w.vol_a.secret))],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = cancel(&w, &w.vol_a, "ZZZZ2345", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// Criterion 34, concurrent: N second cards for one household.
#[tokio::test]
async fn of_many_cards_racing_for_one_household_exactly_one_is_issued() {
    let w = world().await;
    let id = one_household(&w).await;
    let codes = [
        "AAAA0001", "AAAA0002", "AAAA0003", "AAAA0004", "AAAA0005", "AAAA0006",
    ];
    let mut handles = Vec::new();
    for code in codes {
        let (app, secret, id) = (w.app.clone(), w.vol_a.secret.clone(), id.clone());
        handles.push(tokio::spawn(async move {
            app.send(
                "POST",
                &format!("/v1/households/{id}/cards"),
                &secret,
                Some(json!({ "code": code, "client_issued_at": "2026-10-21T10:00:00Z" })),
            )
            .await
        }));
    }
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    let issued = results
        .iter()
        .filter(|r| r.0 == StatusCode::CREATED)
        .count();
    let refused = results
        .iter()
        .filter(|r| error_of(&r.1) == Some("household_has_card"))
        .count();
    assert_eq!((issued, refused), (1, 5), "{results:?}");
    let all = crate::test_support::all_docs(&w.app.state.db, COLLECTION).await;
    assert_eq!(all.len(), 1);
}

#[tokio::test]
async fn of_many_households_racing_for_one_code_exactly_one_gets_it() {
    let w = world().await;
    let mut ids = Vec::new();
    for _ in 0..6 {
        let p = person(&w.app, "P", Some(&w.a)).await;
        ids.push(household(&w.app, &w.vol_a, &w.a, &[&p]).await);
    }
    let mut handles = Vec::new();
    for id in ids {
        let (app, secret) = (w.app.clone(), w.vol_a.secret.clone());
        handles.push(tokio::spawn(async move {
            app.send(
                "POST",
                &format!("/v1/households/{id}/cards"),
                &secret,
                Some(json!({ "code": "SAME2345", "client_issued_at": "2026-10-21T10:00:00Z" })),
            )
            .await
        }));
    }
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    let issued = results
        .iter()
        .filter(|r| r.0 == StatusCode::CREATED)
        .count();
    let taken = results
        .iter()
        .filter(|r| error_of(&r.1) == Some("code_taken"))
        .count();
    assert_eq!((issued, taken), (1, 5), "{results:?}");
}
