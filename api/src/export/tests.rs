use axum::body::Body;
use axum::http::StatusCode;
use axum::http::{HeaderMap, Request};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as STANDARD_B64;
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::persist::{MAX_ROWS, build, event_stats};
use super::view::{
    age_band, cell, child_status_tag, delivery_status_tag, level_tag, verification_tag,
};
use crate::deliveries::ItemScope;
use crate::deliveries::{self, DeliveryDoc, DeliveryStatus, Verification};
use crate::events::EventStats;
use crate::exits::fixtures::{person, put};
use crate::households::Pet;
use crate::households::{self, HouseholdDoc};
use crate::people::Needs;
use crate::people::{ChildStatus, Level};
use crate::rescues::RescueDoc;
use crate::shelters::{self, ShelterDoc};
use crate::store;
use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, new_id, north_of_origin};
use crate::volunteers::GlobalRole;

const CARD: &str = "ZK7Q2M9X";

/// The raw answer: the CSV is not JSON.
async fn get_csv(
    app: &TestApp,
    path: &str,
    session: Option<&str>,
) -> (StatusCode, HeaderMap, String) {
    let mut request = Request::builder().method("GET").uri(path);
    if let Some(session) = session {
        request = request.header("authorization", format!("Bearer {session}"));
    }
    let response = crate::router(app.state.clone())
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, headers) = (response.status(), response.headers().clone());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

fn url(shelter: &str) -> String {
    format!("/v1/shelters/{shelter}/export.csv")
}

async fn delivery(app: &TestApp, shelter: &str, event: Option<&str>, item: &str, person: &str) {
    let now = store::now();
    let doc = DeliveryDoc {
        shelter_id: shelter.into(),
        shelter_name: "Shelter".into(),
        event_id: event.map(str::to_string),
        person_id: Some(person.into()),
        household_id: Some("hh-secret".into()),
        household_pseudonym: None,
        card_code: Some(CARD.into()),
        item: item.into(),
        scope: ItemScope::PerPerson,
        quantity: 2,
        delivered_by: "vol".into(),
        delivered_at: now,
        received_at: now,
        verification: Verification::Card,
        approved_by: None,
        status: DeliveryStatus::Ok,
        reasons: vec![],
        offline_claimed: false,
        delivered_at_clamped: false,
        reviewed_by: None,
        review_note: None,
        pseudonymized: false,
    };
    store::replace(&app.state.db, deliveries::COLLECTION, &new_id(), &doc)
        .await
        .unwrap();
}

async fn household(app: &TestApp, id: &str, shelter: &str, head: &str, members: &[&str]) {
    let now = store::now();
    let doc = HouseholdDoc {
        id: None,
        member_ids: members.iter().map(|m| m.to_string()).collect(),
        head_id: Some(head.into()),
        pets: vec![Pet {
            species: "dog".into(),
            count: 2,
        }],
        card_code: Some(CARD.into()),
        created_by: "vol".into(),
        created_shelter_id: shelter.into(),
        client_created_at: now,
        created_at: now,
        updated_at: now,
    };
    store::replace(&app.state.db, households::COLLECTION, id, &doc)
        .await
        .unwrap();
}

struct World {
    app: TestApp,
    admin: Actor,
    shelter: String,
    other: String,
    ids: Vec<String>,
}

/// A confirmed shelter with three people (two in a household with a card, a pet and a
/// thumbnail), two deliveries; another shelter with a person and a delivery of its own.
async fn world() -> World {
    let app = TestApp::new().await;
    let admin = app.actor("Admin").await;
    let shelter = app.confirmed_shelter(&admin, "Gym", ORIGIN).await;
    let boss = app.actor("Boss").await;
    let other = app
        .confirmed_shelter(&boss, "School", north_of_origin(5000.0))
        .await;
    let (a, b, c, z) = (new_id(), new_id(), new_id(), new_id());

    let mut ana = person(Level::Verified, Some(&shelter));
    ana.name = Some("=HYPERLINK(\"http://x\")".into());
    ana.wristband = Some("W1".into());
    ana.household_id = Some("hh-1".into());
    ana.thumbnail_encrypted = Some(b"ENCRYPTED-THUMB-BYTES".to_vec());
    ana.needs = Needs {
        continuous_medication: true,
        reduced_mobility: true,
        ..Needs::default()
    };
    let mut joao = person(Level::NoBiometrics, Some(&shelter));
    joao.name = Some("Joao, \"Jo\"\nSilva".into());
    joao.social_name = Some("@Jo".into());
    joao.household_id = Some("hh-1".into());
    joao.birth_year = Some(2020);
    let mut eva = person(Level::Assisted, Some(&shelter));
    eva.name = None;
    eva.wristband = Some("W3".into());
    let mut zed = person(Level::Verified, Some(&other));
    zed.name = Some("Zed Elsewhere".into());
    for (id, p) in [(&a, &ana), (&b, &joao), (&c, &eva), (&z, &zed)] {
        put(&app, id, p).await;
    }
    household(&app, "hh-1", &shelter, &a, &[&a, &b]).await;
    delivery(&app, &shelter, None, "blanket", &a).await;
    delivery(&app, &shelter, None, "=soap", &c).await;
    delivery(&app, &other, None, "elsewhere-item", &z).await;
    World {
        app,
        admin,
        shelter,
        other,
        ids: vec![a, b, c, z],
    }
}

#[test]
fn cells_are_guarded_against_formulas_and_escaped() {
    assert_eq!(cell("plain"), "plain");
    for start in ["=1+1", "+1", "-1", "@SUM(A1)", "\tx", "\rx"] {
        assert!(cell(start).trim_matches('"').starts_with('\''), "{start:?}");
    }
    assert_eq!(cell("a,b"), "\"a,b\"");
    assert_eq!(cell("say \"hi\""), "\"say \"\"hi\"\"\"");
    assert_eq!(cell("two\nlines"), "\"two\nlines\"");
    assert_eq!(cell("=a,b"), "\"'=a,b\"");
    assert_eq!(cell(""), "");
}

#[test]
fn age_bands_follow_the_birth_year() {
    for (born, band) in [
        (2026, "0-11"),
        (2015, "0-11"),
        (2014, "12-17"),
        (2009, "12-17"),
        (2008, "18-59"),
        (1967, "18-59"),
        (1966, "60+"),
    ] {
        assert_eq!(age_band(Some(born), 2026), band, "{born}");
    }
    assert_eq!(age_band(None, 2026), "");
}

#[tokio::test]
async fn only_an_admin_of_a_confirmed_shelter_exports() {
    let w = world().await;
    let app = &w.app;
    let (status, headers, body) = get_csv(app, &url(&w.shelter), Some(&w.admin.secret)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.starts_with("# people\r\n"));
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["content-type"], "text/csv; charset=utf-8");
    let disposition = headers["content-disposition"].to_str().unwrap();
    assert!(disposition.starts_with("attachment; filename=\"taba-shelter-"));
    assert!(disposition.ends_with(".csv\""));

    let (status, _, _) = get_csv(app, &url(&w.shelter), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // An ordinary volunteer of the shelter.
    let code = app.invite(&w.admin, &w.shelter, "qr", "volunteer").await;
    let volunteer = app.join(&code, "Vera").await;
    let (status, _, body) = get_csv(app, &url(&w.shelter), Some(&volunteer.secret)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Global roles without a link to the shelter, and an admin of another shelter.
    for (name, role) in [
        ("Val", GlobalRole::Validator),
        ("Dee", GlobalRole::CivilDefense),
        ("Med", GlobalRole::Mediator),
        ("Pro", GlobalRole::Protection),
    ] {
        let who = app.actor(name).await;
        app.grant(&who, role).await;
        let (status, _, _) = get_csv(app, &url(&w.shelter), Some(&who.secret)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{name}");
    }
    let boss = app.actor("Boss").await;
    let (status, _, _) = get_csv(app, &url(&w.shelter), Some(&boss.secret)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = get_csv(app, &url(&w.other), Some(&w.admin.secret)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = get_csv(app, &url("nope"), Some(&w.admin.secret)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_unconfirmed_shelter_does_not_export() {
    let app = TestApp::new().await;
    let admin = app.actor("Admin").await;
    let shelter = app.create_shelter(&admin, "Gym", ORIGIN).await;
    let (status, _, _) = get_csv(&app, &url(&shelter), Some(&admin.secret)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        !crate::test_support::audit_actions(&app.state.db)
            .await
            .contains(&"export.shelter".to_string())
    );
}

#[tokio::test]
async fn the_csv_has_the_shelter_data_and_no_identifier() {
    let w = world().await;
    let (_, _, body) = get_csv(&w.app, &url(&w.shelter), Some(&w.admin.secret)).await;

    for section in ["# people", "# households", "# deliveries"] {
        assert_eq!(body.matches(section).count(), 1, "{section}");
    }
    for expected in [
        "W1",
        "W3",
        "verified",
        "no_biometrics",
        "assisted",
        "blanket",
        "dog x2",
    ] {
        assert!(body.contains(expected), "{expected}\n{body}");
    }
    assert!(body.contains("continuous_medication|reduced_mobility"));
    assert!(body.contains("0-11"));
    // Formula guard and escaping.
    assert!(body.contains("\"'=HYPERLINK(\"\"http://x\"\")\""), "{body}");
    assert!(body.contains("'@Jo"));
    assert!(body.contains("\"Joao, \"\"Jo\"\"\nSilva\""));
    assert!(body.contains("'=soap"));
    // The household has a card, but not its code.
    assert!(body.contains(",yes\r\n"));
    assert!(body.contains("N1"));
    // Nothing of the other shelter.
    for foreign in ["Zed", "elsewhere-item"] {
        assert!(!body.contains(foreign), "{foreign}");
    }
    // No person id, card code, household id, token or thumbnail, by pattern.
    for id in &w.ids {
        assert!(!body.contains(id.as_str()), "person id {id}");
    }
    assert!(!body.contains(CARD));
    assert!(!body.contains("hh-1") && !body.contains("hh-secret"));
    assert!(!body.contains("tok-"));
    assert!(!body.contains("ENCRYPTED-THUMB"));
    assert!(!body.contains(&STANDARD_B64.encode(b"ENCRYPTED-THUMB-BYTES")));
    let uuid_like = body
        .split(|c: char| !(c.is_ascii_hexdigit() || c == '-'))
        .any(|w| w.len() == 36 && w.matches('-').count() == 4);
    assert!(!uuid_like, "a UUID in the export");
    let long_base64 = body
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='))
        .any(|w| w.len() >= 40);
    assert!(!long_base64, "a long base64 blob in the export");
    let people_header = body.lines().nth(1).unwrap();
    assert!(!people_header.contains("id") && !people_header.contains("thumbnail"));
}

#[tokio::test]
async fn the_export_is_audited_without_its_content() {
    let w = world().await;
    let (status, _, _) = get_csv(&w.app, &url(&w.shelter), Some(&w.admin.secret)).await;
    assert_eq!(status, StatusCode::OK);
    let entries: Vec<_> = all_docs(&w.app.state.db, "audit_log")
        .await
        .into_iter()
        .filter(|e| e["action"] == "export.shelter")
        .collect();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry["volunteer_id"], w.admin.id);
    assert_eq!(entry["shelter_id"], w.shelter);
    assert_eq!(entry["target_kind"], "shelter");
    assert_eq!(entry["target_id"], w.shelter);
    assert_eq!(entry["legal_basis"], "public_policy");
    let text = entry.to_string();
    for secret in ["W1", "Joao", "blanket"] {
        assert!(!text.contains(secret));
    }
}

#[tokio::test]
async fn event_stats_count_the_event_without_personal_data() {
    let app = TestApp::new().await;
    let admin = app.actor("Admin").await;
    let s1 = app.confirmed_shelter(&admin, "Gym", ORIGIN).await;
    let boss = app.actor("Boss").await;
    let s2 = app
        .confirmed_shelter(&boss, "School", north_of_origin(5000.0))
        .await;
    let first: ShelterDoc = store::get(&app.state.db, shelters::COLLECTION, &s1)
        .await
        .unwrap()
        .unwrap();
    let event = first.event_id.clone().expect("a shelter has an event");
    let second: ShelterDoc = store::get(&app.state.db, shelters::COLLECTION, &s2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.event_id.as_deref(), Some(event.as_str()));

    for (i, shelter_id) in [&s1, &s1, &s2].into_iter().enumerate() {
        let mut p = person(Level::Assisted, Some(shelter_id));
        p.event_ids = vec![event.clone()];
        put(&app, &format!("p{i}"), &p).await;
    }
    let mut outsider = person(Level::Assisted, Some(&s1));
    outsider.event_ids = vec!["another-event".into()];
    put(&app, "outsider", &outsider).await;
    household(&app, "h1", &s1, "p0", &["p0", "p1"]).await;
    household(&app, "h2", &s2, "p2", &["p2"]).await;
    household(&app, "h-other", "some-other-shelter", "x", &["x"]).await;
    for item in ["soap", "blanket", "soap"] {
        delivery(&app, &s1, Some(&event), item, "p0").await;
    }
    delivery(&app, &s1, Some("another-event"), "soap", "p0").await;
    let now = store::now();
    let rescue = RescueDoc {
        person_id: "p0".into(),
        volunteer_id: "vol".into(),
        device_id: None,
        lat: ORIGIN.0,
        lon: ORIGIN.1,
        accuracy_m: 10.0,
        landing_point: None,
        captured_at: now,
        received_at: now,
        event_id: Some(event.clone()),
        destination_shelter_id: None,
        note: None,
    };
    store::replace(
        &app.state.db,
        crate::rescues::COLLECTION,
        &new_id(),
        &rescue,
    )
    .await
    .unwrap();

    let stats = event_stats(&app.state.db, &event).await.unwrap();
    assert_eq!(
        stats,
        EventStats {
            shelters: 2,
            unique_people: 3,
            households: 2,
            rescues: 1,
            deliveries: 3
        }
    );
    assert_eq!(
        event_stats(&app.state.db, "empty-event").await.unwrap(),
        EventStats::default()
    );
}

#[test]
fn tags_match_the_serde_names() {
    fn serde_name<T: serde::Serialize>(value: &T) -> String {
        serde_json::to_value(value)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    }
    for v in [Level::Verified, Level::Assisted, Level::NoBiometrics] {
        assert_eq!(level_tag(v), serde_name(&v));
    }
    for v in [
        ChildStatus::WithGuardian,
        ChildStatus::Separated,
        ChildStatus::Unaccompanied,
    ] {
        assert_eq!(child_status_tag(v), serde_name(&v));
    }
    for v in [
        DeliveryStatus::Ok,
        DeliveryStatus::InReview,
        DeliveryStatus::ReviewedOk,
        DeliveryStatus::Irregular,
    ] {
        assert_eq!(delivery_status_tag(v), serde_name(&v));
    }
    for v in [
        Verification::Card,
        Verification::Liveness,
        Verification::AdminApproval,
    ] {
        assert_eq!(verification_tag(v), serde_name(&v));
    }
}

/// The data rows of a section (no header, no warning).
fn rows_of<'a>(body: &'a str, section: &str) -> Vec<&'a str> {
    body.split("\r\n\r\n")
        .find(|s| s.starts_with(&format!("# {section}\r\n")))
        .unwrap()
        .split("\r\n")
        .skip(2)
        .filter(|l| !l.is_empty() && !l.starts_with("# warning"))
        .collect()
}

#[tokio::test]
async fn the_export_has_every_eligible_row_and_says_when_it_cut() {
    let w = world().await;
    let db = &w.app.state.db;
    let now = store::now();
    let all = build(db, &w.shelter, now, MAX_ROWS).await.unwrap();
    let (people, deliveries) = (
        rows_of(&all, "people").len(),
        rows_of(&all, "deliveries").len(),
    );
    assert!(people >= 3 && deliveries >= 2);
    assert!(!all.contains("# warning"));
    // Exactly at the limit: nothing is cut, no warning.
    let at_limit = build(db, &w.shelter, now, people.max(deliveries) as u32)
        .await
        .unwrap();
    assert_eq!(rows_of(&at_limit, "people").len(), people);
    assert!(!at_limit.contains("# warning"));
    // One below the limit of people: one row less and the warning.
    let cut = build(db, &w.shelter, now, people as u32 - 1).await.unwrap();
    assert_eq!(rows_of(&cut, "people").len(), people - 1);
    assert!(
        cut.contains(&format!("# warning,truncated,people,{}", people - 1)),
        "{cut}"
    );
}
