use axum::body::Body;
use axum::http::Request;
use axum::http::StatusCode;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use http_body_util::BodyExt;
use jiff::tz::TimeZone;
use serde_json::{Value, json};
use std::time::Duration;
use tower::ServiceExt;

use super::domain::{cap, count_value};
use super::model::{EventDashboard, NOTICE};
use super::persist::{
    CAPTURES, COUNT_LIMITER, DETAIL, LIST, Limits, SCANS, TtlCache, build_event, build_list,
    count_with, current_year, pets_of, scan_moves,
};
use super::view::csv_field;
use crate::alerts::AlertDoc;
use crate::alerts::AlertKind;
use crate::audit::LegalBasis;
use crate::auth::{CaptureDoc, CaptureStatus, Purpose};
use crate::error::ApiError;
use crate::events::{self, EventStatus};
use crate::households::{self, Pet};
use crate::matches::{self, MatchDoc};
use crate::matches::{MatchStatus, MediatorKind};
use crate::people::{self, ChildStatus, Disability, ExitReason, Level, PersonDoc, Stay};
use crate::people::{ExitDestination as Dest, Needs, Origin, Vulnerabilities};
use crate::rescues::{self, RescueDoc};
use crate::shelters::{self, ShelterDoc, ShelterStatus};
use crate::store;
use crate::test_support::{Actor, ORIGIN, TestApp, new_id, north_of_origin};
use crate::volunteers::GlobalRole;

const HOUR_US: i64 = 3_600 * 1_000_000;

fn ago(hours: i64) -> firestore::FirestoreInstant {
    firestore::FirestoreInstant::from_microsecond(store::now().as_microsecond() - hours * HOUR_US)
        .unwrap()
}

fn born(years_old: i64) -> Option<u16> {
    Some(u16::try_from(current_year() - years_old).unwrap())
}

/// An adult in `shelter` (of `event`) with nothing else set.
fn person(level: Level, shelter: Option<&str>, event: &str) -> PersonDoc {
    let now = store::now();
    PersonDoc {
        id: None,
        level,
        biometrics_declined: false,
        former_ids: vec![],
        name: Some("Maria Secreta".into()),
        social_name: None,
        wristband: None,
        birth_year: born(30),
        is_child: false,
        child_status: None,
        guardians: vec![],
        guardian_ids: vec![],
        needs: Needs::default(),
        vulnerabilities: Vulnerabilities::default(),
        household_id: None,
        current_shelter_id: shelter.map(str::to_string),
        stays: vec![],
        event_ids: vec![event.to_string()],
        rescue_ids: vec![],
        thumbnail_encrypted: None,
        origin: Origin::Checkin,
        legal_basis: LegalBasis::Consent,
        created_at: now,
        updated_at: now,
        client_edited_at: None,
    }
}

async fn put_person(app: &TestApp, doc: &PersonDoc) -> String {
    let id = new_id();
    store::replace(&app.state.db, people::COLLECTION, &id, doc)
        .await
        .unwrap();
    id
}

fn closed_stay(
    shelter: &str,
    event: &str,
    left_hours_ago: i64,
    reason: ExitReason,
    destination: Option<Dest>,
) -> Stay {
    Stay {
        shelter_id: shelter.into(),
        event_id: Some(event.into()),
        entered_at: ago(left_hours_ago + 1),
        left_at: Some(ago(left_hours_ago)),
        exit_reason: Some(reason),
        exit_destination: destination,
        destination_municipality: None,
        registered_by: "test".into(),
    }
}

async fn shelter_doc(app: &TestApp, id: &str) -> ShelterDoc {
    store::get(&app.state.db, shelters::COLLECTION, id)
        .await
        .unwrap()
        .unwrap()
}

async fn update_shelter(app: &TestApp, id: &str, edit: impl FnOnce(&mut ShelterDoc)) {
    let mut doc = shelter_doc(app, id).await;
    edit(&mut doc);
    store::replace(&app.state.db, shelters::COLLECTION, id, &doc)
        .await
        .unwrap();
}

struct World {
    app: TestApp,
    civil_defense: Actor,
    validator: Actor,
    ana: Actor,
    event: String,
    a: String,
    b: String,
    other_event: String,
    people_ids: Vec<String>,
}

/// Event E: shelter A (unconfirmed, flagged, capacity 10, 5 people) and shelter B (confirmed, no
/// capacity, 6 people on the edges of the age bands); event O: one shelter with one person.
async fn world() -> World {
    let app = TestApp::new().await;
    let ana = app.actor("Ana").await;
    let civil_defense = app.actor("Defesa").await;
    app.grant(&civil_defense, GlobalRole::CivilDefense).await;
    let validator = app.actor("Validador").await;
    app.grant(&validator, GlobalRole::Validator).await;

    let a = app.create_shelter(&ana, "Abrigo A", ORIGIN).await;
    let b = app
        .confirmed_shelter(&ana, "Abrigo B", north_of_origin(5_000.0))
        .await;
    let far = app.actor("Longe").await;
    let o = app
        .create_shelter(&far, "Abrigo O", north_of_origin(1_000_000.0))
        .await;
    let event = shelter_doc(&app, &a).await.event_id.unwrap();
    let other_event = shelter_doc(&app, &o).await.event_id.unwrap();
    assert_ne!(event, other_event);
    assert_eq!(shelter_doc(&app, &b).await.event_id.unwrap(), event);
    update_shelter(&app, &a, |s| {
        s.capacity = Some(10);
        s.flagged = true;
    })
    .await;
    update_shelter(&app, &b, |s| s.other_site_reason = Some("a reason".into())).await;

    let mut ids = vec![];
    // Shelter A.
    let mut p = person(Level::Verified, Some(&a), &event);
    p.birth_year = born(5);
    p.is_child = true;
    p.child_status = Some(ChildStatus::Separated);
    ids.push(put_person(&app, &p).await);
    let mut p = person(Level::Assisted, Some(&a), &event);
    p.birth_year = born(15);
    p.is_child = true;
    p.child_status = Some(ChildStatus::Unaccompanied);
    ids.push(put_person(&app, &p).await);
    let mut p = person(Level::NoBiometrics, Some(&a), &event);
    p.vulnerabilities.pregnant = true;
    p.vulnerabilities.disability = vec![Disability::Mobility, Disability::Visual];
    p.biometrics_declined = true;
    ids.push(put_person(&app, &p).await);
    let mut p = person(Level::Verified, Some(&a), &event);
    p.birth_year = born(70);
    p.vulnerabilities.lactating = true;
    p.vulnerabilities.indigenous_people = Some("guarani".into());
    ids.push(put_person(&app, &p).await);
    let mut p = person(Level::Assisted, Some(&a), &event);
    p.birth_year = None;
    p.vulnerabilities.migrant_language = Some("es".into());
    p.vulnerabilities.disability = vec![Disability::Mobility];
    ids.push(put_person(&app, &p).await);
    // Shelter B: the edges of the age bands.
    for years_old in [18, 17, 12, 11, 60, 59] {
        let mut p = person(Level::Verified, Some(&b), &event);
        p.birth_year = born(years_old);
        ids.push(put_person(&app, &p).await);
    }
    // The other event.
    ids.push(put_person(&app, &person(Level::Verified, Some(&o), &other_event)).await);
    // Left the shelters: the exits and movements of E.
    let mut gone = person(Level::Verified, None, &event);
    gone.stays = vec![
        closed_stay(&a, &event, 30, ExitReason::Exit, Some(Dest::OwnHome)),
        // Another event's stay on the same person: not an exit of E.
        closed_stay(&o, &other_event, 1, ExitReason::Exit, Some(Dest::RentalAid)),
    ];
    ids.push(put_person(&app, &gone).await);
    let mut gone = person(Level::Verified, None, &event);
    gone.stays = vec![closed_stay(
        &a,
        &event,
        100,
        ExitReason::ExceptionalExit,
        Some(Dest::RelativesOrFriends),
    )];
    ids.push(put_person(&app, &gone).await);
    for hours in [2, 72, 240] {
        let mut gone = person(Level::Verified, None, &event);
        gone.stays = vec![closed_stay(
            &a,
            &event,
            hours,
            ExitReason::CheckinElsewhere,
            None,
        )];
        ids.push(put_person(&app, &gone).await);
    }

    // Households and animals.
    for (shelter, pets) in [
        (&a, vec![("dog", 2), ("cat", 1)]),
        (&a, vec![("dog", 1)]),
        (&b, vec![]),
    ] {
        let now = store::now();
        let household = households::HouseholdDoc {
            id: None,
            member_ids: vec![],
            head_id: None,
            pets: pets
                .into_iter()
                .map(|(species, count)| Pet {
                    species: species.into(),
                    count,
                })
                .collect(),
            card_code: None,
            created_by: "test".into(),
            created_shelter_id: shelter.clone(),
            client_created_at: now,
            created_at: now,
            updated_at: now,
        };
        store::replace(&app.state.db, households::COLLECTION, &new_id(), &household)
            .await
            .unwrap();
    }

    // Rescues: two of E, one of O.
    for ev in [&event, &event, &other_event] {
        let now = store::now();
        let rescue = RescueDoc {
            person_id: "p".into(),
            volunteer_id: "v".into(),
            device_id: None,
            lat: 0.0,
            lon: 0.0,
            accuracy_m: 1.0,
            landing_point: None,
            captured_at: now,
            received_at: now,
            event_id: Some(ev.clone()),
            destination_shelter_id: None,
            note: None,
        };
        store::replace(&app.state.db, rescues::COLLECTION, &new_id(), &rescue)
            .await
            .unwrap();
    }

    // Alerts: two open (one informational) and one resolved of E, one open of O.
    for (ev, informational, resolved) in [
        (&event, false, false),
        (&event, true, false),
        (&event, false, true),
        (&other_event, false, false),
    ] {
        let mut alert = AlertDoc::new(AlertKind::ShelterFlagged, "shelter", &a, json!({}));
        alert.event_id = Some(ev.clone());
        alert.informational = informational;
        alert.resolved = resolved;
        store::replace(&app.state.db, crate::alerts::COLLECTION, &new_id(), &alert)
            .await
            .unwrap();
    }

    // Matches: 2 central open, 1 protection open, 1 central accepted, 1 not_the_person.
    for (kind, status) in [
        (MediatorKind::Central, MatchStatus::Open),
        (MediatorKind::Central, MatchStatus::Open),
        (MediatorKind::Protection, MatchStatus::Open),
        (MediatorKind::Central, MatchStatus::Accepted),
        (MediatorKind::Shelter, MatchStatus::NotThePerson),
    ] {
        let doc = MatchDoc {
            id: None,
            search_id: "s".into(),
            person_id: "p".into(),
            is_child: false,
            mediator_kind: kind,
            mediator_shelter_id: None,
            status,
            decided_by: None,
            decided_by_volunteer: None,
            decided_at: None,
            note: None,
            created_at: store::now(),
        };
        store::replace(&app.state.db, matches::COLLECTION, &new_id(), &doc)
            .await
            .unwrap();
    }

    // Captures: 3 that count in A, a login that does not, one in the other event's shelter.
    for (shelter, purpose) in [
        (&a, Purpose::Checkin),
        (&a, Purpose::Checkin),
        (&a, Purpose::Biometrics),
        (&a, Purpose::Login),
        (&o, Purpose::Checkin),
    ] {
        let doc = CaptureDoc {
            process_id: "proc".into(),
            purpose,
            device_id: "d".into(),
            status: CaptureStatus::Ok,
            person_id: None,
            volunteer_id: None,
            shelter_id: Some(shelter.clone()),
            expected_person_id: None,
            consumed: false,
            created_at: store::now(),
            expires_at: store::now(),
        };
        store::replace(&app.state.db, CAPTURES, &new_id(), &doc)
            .await
            .unwrap();
    }
    World {
        app,
        civil_defense,
        validator,
        ana,
        event,
        a,
        b,
        other_event,
        people_ids: ids,
    }
}

async fn get_json(w: &World, who: &Actor, path: &str) -> (StatusCode, Value) {
    w.app.get_authed(path, &who.secret).await
}

fn row<'a>(body: &'a Value, shelter_id: &str) -> &'a Value {
    body["shelters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == shelter_id)
        .unwrap()
}

#[tokio::test]
async fn the_event_totals_count_every_cut_of_the_people_in_shelters() {
    let w = world().await;
    let path = format!("/v1/dashboard/events/{}", w.event);
    let (status, body) = get_json(&w, &w.civil_defense, &path).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["notice"], NOTICE);
    assert_eq!(body["id"], w.event.as_str());
    let t = &body["totals"];
    assert_eq!(t["unique_people"], 11);
    assert_eq!(
        t["by_level"],
        json!({ "verified": 8, "assisted": 2, "no_biometrics": 1 })
    );
    assert_eq!(
        t["age_bands"],
        json!({ "0_11": 2, "12_17": 3, "18_59": 3, "60_plus": 2, "unknown": 1 })
    );
    assert_eq!(t["pregnant"], 1);
    assert_eq!(t["lactating"], 1);
    assert_eq!(
        t["disability"],
        json!({ "mobility": 2, "visual": 1, "hearing": 0, "intellectual": 0, "psychosocial": 0, "other": 0 })
    );
    assert_eq!(t["children_separated"], 1);
    assert_eq!(t["children_unaccompanied"], 1);
    assert_eq!(t["indigenous"], 1);
    assert_eq!(t["migrant"], 1);
    assert_eq!(t["households"], 3);
    assert_eq!(t["pets"], 4);
}

#[tokio::test]
async fn each_shelter_row_has_its_own_counts_capacity_and_vacancies() {
    let w = world().await;
    let path = format!("/v1/dashboard/events/{}", w.event);
    let (_, body) = get_json(&w, &w.validator, &path).await;
    assert_eq!(body["shelters"].as_array().unwrap().len(), 2);
    let a = row(&body, &w.a);
    assert_eq!(a["name"], "Abrigo A");
    assert_eq!(a["confirmation"], "unconfirmed");
    assert_eq!(a["flagged"], true);
    assert_eq!(a["lat"], ORIGIN.0);
    assert_eq!(a["lon"], ORIGIN.1);
    assert_eq!(a["capacity"], 10);
    assert_eq!(a["occupancy"], 5);
    assert_eq!(a["vacancies"], 5);
    let c = &a["counts"];
    assert_eq!(c["unique_people"], 5);
    assert_eq!(
        c["by_level"],
        json!({ "verified": 2, "assisted": 2, "no_biometrics": 1 })
    );
    // The person without `birth_year` is only in `unknown`.
    assert_eq!(
        c["age_bands"],
        json!({ "0_11": 1, "12_17": 1, "18_59": 1, "60_plus": 1, "unknown": 1 })
    );
    assert_eq!(c["pregnant"], 1);
    assert_eq!(c["lactating"], 1);
    assert_eq!(c["disability"]["mobility"], 2);
    assert_eq!(c["disability"]["visual"], 1);
    assert_eq!(c["children_separated"], 1);
    assert_eq!(c["children_unaccompanied"], 1);
    assert_eq!(c["indigenous"], 1);
    assert_eq!(c["migrant"], 1);
    assert_eq!(c["households"], 2);
    assert_eq!(c["pets"], 4);

    let b = row(&body, &w.b);
    assert_eq!(b["confirmation"], "confirmed");
    assert_eq!(b["flagged"], false);
    assert_eq!(b["capacity"], Value::Null);
    assert_eq!(b["occupancy"], 6);
    assert_eq!(b["vacancies"], Value::Null);
    let c = &b["counts"];
    assert_eq!(c["unique_people"], 6);
    assert_eq!(
        c["by_level"],
        json!({ "verified": 6, "assisted": 0, "no_biometrics": 0 })
    );
    // Ages 18 and 59 are 18_59, 17 and 12 are 12_17, 11 is 0_11, 60 is 60_plus.
    assert_eq!(
        c["age_bands"],
        json!({ "0_11": 1, "12_17": 2, "18_59": 2, "60_plus": 1, "unknown": 0 })
    );
    assert_eq!(c["households"], 1);
    assert_eq!(c["pets"], 0);
    assert_eq!(c["pregnant"], 0);
    assert_eq!(c["indigenous"], 0);
}

#[tokio::test]
async fn movements_exits_rescues_alerts_matches_and_pilot_indicators() {
    let w = world().await;
    let path = format!("/v1/dashboard/events/{}", w.event);
    let (_, body) = get_json(&w, &w.civil_defense, &path).await;
    // A check-in elsewhere 2 h ago is in both windows, 72 h ago only in the 7 days, 240 h in none.
    assert_eq!(body["movement"], json!({ "last_24h": 1, "last_7d": 2 }));
    // The stay of the other event on one of E's people is not an exit of E.
    assert_eq!(
        body["exits_by_destination"],
        json!({ "own_home": 1, "relatives_or_friends": 1 })
    );
    assert_eq!(body["rescues"], 2);
    assert_eq!(body["open_alerts"], 2);
    assert_eq!(
        body["open_matches"],
        json!({ "central": 2, "protection": 1 })
    );
    let pilot = &body["pilot"];
    assert_eq!(pilot["not_the_person"], 1);
    assert_eq!(pilot["other_site_reasons"], 1);
    // One refusal in 11 people; 3 Unico processes in 11 people.
    assert!((pilot["selfie_refusal_rate"].as_f64().unwrap() - 1.0 / 11.0).abs() < 1e-9);
    assert!((pilot["unico_processes_per_person"].as_f64().unwrap() - 3.0 / 11.0).abs() < 1e-9);
}

#[tokio::test]
async fn an_event_without_shelters_is_all_zero_and_the_rates_are_absent() {
    let w = world().await;
    let empty = new_id();
    let doc = events::EventDoc {
        name: "Vazio".into(),
        auto_named: false,
        status: EventStatus::Active,
        created_at: store::now(),
        last_activity_at: store::now(),
        closed_at: None,
        closure_reason: None,
        purge_at: None,
        delivery_purge_at: None,
        purged_at: None,
        stats: None,
        reference_contacts: Default::default(),
    };
    store::replace(&w.app.state.db, events::COLLECTION, &empty, &doc)
        .await
        .unwrap();
    let (status, body) = get_json(
        &w,
        &w.civil_defense,
        &format!("/v1/dashboard/events/{empty}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["totals"]["unique_people"], 0);
    assert_eq!(body["shelters"], json!([]));
    assert_eq!(body["movement"], json!({ "last_24h": 0, "last_7d": 0 }));
    assert_eq!(body["exits_by_destination"], json!({}));
    assert_eq!(body["pilot"]["selfie_refusal_rate"], Value::Null);
    assert_eq!(body["pilot"]["unico_processes_per_person"], Value::Null);
}

#[tokio::test]
async fn the_list_has_the_summary_and_the_shelters_to_confirm() {
    let w = world().await;
    let (status, body) = get_json(&w, &w.civil_defense, "/v1/dashboard/events").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let events = body["events"].as_array().unwrap();
    let e = events.iter().find(|e| e["id"] == w.event.as_str()).unwrap();
    assert_eq!(e["status"], "active");
    assert_eq!(e["shelters"], 2);
    assert_eq!(e["occupancy"], 11);
    assert_eq!(e["unique_people"], 11);
    let waiting = e["unconfirmed_shelters"].as_array().unwrap();
    assert_eq!(waiting.len(), 1, "{e}");
    let s = &waiting[0];
    assert_eq!(s["id"], w.a.as_str());
    assert_eq!(s["name"], "Abrigo A");
    assert_eq!(s["flagged"], true);
    assert_eq!(s["lat"], ORIGIN.0);
    assert_eq!(s["lon"], ORIGIN.1);
    assert_eq!(s["created_by_name"], "Ana");
    let o = events
        .iter()
        .find(|e| e["id"] == w.other_event.as_str())
        .unwrap();
    assert_eq!(o["occupancy"], 1);
    assert_eq!(o["unconfirmed_shelters"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_closed_shelter_is_not_waiting_for_confirmation() {
    let w = world().await;
    update_shelter(&w.app, &w.a, |s| s.status = ShelterStatus::Closed).await;
    let (_, body) = get_json(&w, &w.validator, "/v1/dashboard/events").await;
    let e = body["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == w.event.as_str())
        .unwrap();
    assert_eq!(e["unconfirmed_shelters"], json!([]));
    let path = format!("/v1/dashboard/events/{}", w.event);
    let (_, detail) = get_json(&w, &w.validator, &path).await;
    assert_eq!(detail["unconfirmed_shelters"], json!([]));
}

#[tokio::test]
async fn the_detail_lists_the_unconfirmed_shelters_too() {
    let w = world().await;
    let path = format!("/v1/dashboard/events/{}", w.event);
    let (_, body) = get_json(&w, &w.validator, &path).await;
    let waiting = body["unconfirmed_shelters"].as_array().unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0]["id"], w.a.as_str());
    assert_eq!(waiting[0]["created_by_name"], "Ana");
}

#[tokio::test]
async fn only_validator_and_civil_defense_read_the_panel() {
    let w = world().await;
    let paths = [
        "/v1/dashboard/events".to_string(),
        format!("/v1/dashboard/events/{}", w.event),
        format!("/v1/dashboard/events/{}/daily.csv", w.event),
    ];
    let mediator = w.app.actor("Mediadora").await;
    w.app.grant(&mediator, GlobalRole::Mediator).await;
    let protection = w.app.actor("Protecao").await;
    w.app.grant(&protection, GlobalRole::Protection).await;
    let stranger = w.app.actor("Estranho").await;
    for path in &paths {
        let (status, _, body) = w.app.request("GET", path, &[], None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(body["error"], "unauthorized");
        // `ana` is the admin of a shelter of the event.
        for who in [&w.ana, &mediator, &protection, &stranger] {
            let (status, body) = get_json(&w, who, path).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
            assert_eq!(body["error"], "forbidden");
        }
        for who in [&w.validator, &w.civil_defense] {
            let auth = format!("Bearer {}", who.secret);
            let (status, _, _) = w
                .app
                .request("GET", path, &[("authorization", auth.as_str())], None)
                .await;
            assert_eq!(status, StatusCode::OK, "{path}");
        }
    }
}

#[tokio::test]
async fn an_unknown_event_is_404_after_the_permission_check() {
    let w = world().await;
    for suffix in ["", "/daily.csv"] {
        let path = format!("/v1/dashboard/events/nope{suffix}");
        let (status, body) = get_json(&w, &w.civil_defense, &path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"], "event_not_found");
        let (status, _) = get_json(&w, &w.ana, &path).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}

async fn csv(w: &World, who: &Actor) -> (StatusCode, axum::http::HeaderMap, String) {
    let request = Request::builder()
        .method("GET")
        .uri(format!("/v1/dashboard/events/{}/daily.csv", w.event))
        .header("authorization", format!("Bearer {}", who.secret))
        .body(Body::empty())
        .unwrap();
    let response = crate::router(w.app.state.clone())
        .oneshot(request)
        .await
        .unwrap();
    let (status, headers) = (response.status(), response.headers().clone());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn the_daily_csv_has_one_line_per_shelter_and_no_person() {
    let w = world().await;
    let (status, headers, body) = csv(&w, &w.civil_defense).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[CONTENT_TYPE], "text/csv; charset=utf-8");
    assert!(
        headers[CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment; filename=\"taba-")
    );
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 3, "header and two shelters: {body}");
    assert_eq!(
        lines[0],
        "date,event_id,shelter_id,shelter_name,confirmation,flagged,lat,lon,capacity,occupancy,vacancies,\
unique_people,verified,assisted,no_biometrics,age_0_11,age_12_17,age_18_59,age_60_plus,age_unknown,\
pregnant,lactating,disability_mobility,disability_visual,disability_hearing,disability_intellectual,\
disability_psychosocial,disability_other,children_separated,children_unaccompanied,indigenous,migrant,\
households_created_here,pets_in_households_created_here"
    );
    let width = lines[0].split(',').count();
    let a: Vec<&str> = lines
        .iter()
        .find(|l| l.contains(&w.a))
        .unwrap()
        .split(',')
        .collect();
    assert_eq!(a.len(), width);
    let date = store::now().to_zoned(TimeZone::UTC).date().to_string();
    assert_eq!(a[0], date);
    assert_eq!(a[1], w.event);
    assert_eq!(a[3], "Abrigo A");
    assert_eq!(a[4], "unconfirmed");
    assert_eq!(a[5], "true");
    assert_eq!(&a[8..12], ["10", "5", "5", "5"]);
    assert_eq!(&a[12..15], ["2", "2", "1"]);
    assert_eq!(&a[15..20], ["1", "1", "1", "1", "1"]);
    assert_eq!(&a[20..22], ["1", "1"]);
    assert_eq!(&a[22..28], ["2", "1", "0", "0", "0", "0"]);
    assert_eq!(&a[28..34], ["1", "1", "1", "1", "2", "4"]);
    let b: Vec<&str> = lines
        .iter()
        .find(|l| l.contains(&w.b))
        .unwrap()
        .split(',')
        .collect();
    // No capacity: both `capacity` and `vacancies` are empty.
    assert_eq!((b[8], b[9], b[10]), ("", "6", ""));
    assert!(!body.contains("Maria"));
    assert!(!body.contains("person"));
    for id in &w.people_ids {
        assert!(!body.contains(id.as_str()));
    }
}

#[tokio::test]
async fn the_json_answers_never_carry_a_name_or_a_person_id() {
    let w = world().await;
    for path in [
        "/v1/dashboard/events".to_string(),
        format!("/v1/dashboard/events/{}", w.event),
    ] {
        let (_, body) = get_json(&w, &w.civil_defense, &path).await;
        let text = body.to_string();
        assert!(!text.contains("Maria"), "{path}");
        assert!(!text.contains("person_id"), "{path}");
        for id in &w.people_ids {
            assert!(!text.contains(id.as_str()), "{path}");
        }
    }
}

#[tokio::test]
async fn a_shelter_name_that_looks_like_a_formula_is_neutralized_in_the_csv() {
    let w = world().await;
    update_shelter(&w.app, &w.b, |s| {
        s.name = "=HYPERLINK(\"http://x\",\"a,b\")".into();
    })
    .await;
    let (_, _, body) = csv(&w, &w.civil_defense).await;
    assert!(
        body.contains("\"'=HYPERLINK(\"\"http://x\"\",\"\"a,b\"\")\""),
        "{body}"
    );
}

#[test]
fn csv_fields_are_guarded_against_formulas_and_quoted_when_needed() {
    assert_eq!(csv_field("Abrigo"), "Abrigo");
    for lead in ["=1+1", "+1", "-1", "@SUM(A1)", "\tx"] {
        assert_eq!(csv_field(lead), format!("'{lead}"), "{lead:?}");
    }
    assert_eq!(csv_field("\rx"), "\"'\rx\"");
    assert_eq!(csv_field("a,b"), "\"a,b\"");
    assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    assert_eq!(csv_field("a\nb"), "\"a\nb\"");
    assert_eq!(csv_field("São José"), "São José");
}

#[tokio::test]
async fn the_panel_is_served_from_the_cache_until_it_expires() {
    let w = world().await;
    let path = format!("/v1/dashboard/events/{}", w.event);
    let (_, first) = get_json(&w, &w.civil_defense, &path).await;
    assert_eq!(first["totals"]["unique_people"], 11);
    put_person(&w.app, &person(Level::Assisted, Some(&w.b), &w.event)).await;
    let (_, second) = get_json(&w, &w.civil_defense, &path).await;
    assert_eq!(second, first, "served from the cache");
    // The cache does not skip the permission check.
    let (status, _) = get_json(&w, &w.ana, &path).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    DETAIL.remove(&format!(
        "{}/{}",
        w.app.state.db.get_database_path(),
        w.event
    ));
    let (_, third) = get_json(&w, &w.civil_defense, &path).await;
    assert_eq!(third["totals"]["unique_people"], 12);
}

#[tokio::test]
async fn the_list_reads_counts_only_and_the_detail_scans_the_people() {
    let w = world().await;
    let scans = || SCANS.with(|n| n.get());
    LIST.remove(w.app.state.db.get_database_path());
    let before = scans();
    let (status, _) = get_json(&w, &w.civil_defense, "/v1/dashboard/events").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(scans(), before, "the list scanned people");
    let path = format!("/v1/dashboard/events/{}", w.event);
    get_json(&w, &w.civil_defense, &path).await;
    assert!(scans() > before, "the detail scans the people");
}

#[tokio::test]
async fn the_cache_builds_once_inside_the_ttl_and_again_after_it() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let cache: TtlCache<u32> = TtlCache::new(8);
    let built = AtomicU32::new(0);
    let build = || async { Ok(built.fetch_add(1, Ordering::SeqCst) + 1) };
    let ttl = Duration::from_secs(60);
    assert_eq!(cache.get_or("k", ttl, build).await.unwrap(), 1);
    assert_eq!(cache.get_or("k", ttl, build).await.unwrap(), 1);
    // Another key is its own entry.
    assert_eq!(cache.get_or("other", ttl, build).await.unwrap(), 2);
    // A ttl of zero has always expired.
    assert_eq!(cache.get_or("k", Duration::ZERO, build).await.unwrap(), 3);
    // A failed build is not cached.
    let failed = cache
        .get_or("bad", ttl, || async { Err(ApiError::internal()) })
        .await;
    assert!(failed.is_err());
    assert_eq!(cache.get_or("bad", ttl, build).await.unwrap(), 4);
}

#[test]
fn a_read_is_cut_only_when_it_returns_more_than_the_limit() {
    for (rows, cut) in [(2, false), (3, false), (4, true)] {
        let (kept, was_cut) = cap((0..rows).collect::<Vec<_>>(), 3);
        assert_eq!((kept.len(), was_cut), (rows.min(3), cut), "{rows} rows");
    }
}

#[test]
fn a_count_that_does_not_fit_is_an_error_not_zero() {
    assert_eq!(count_value(0).unwrap(), 0);
    assert_eq!(count_value(i64::from(u32::MAX)).unwrap(), u32::MAX);
    assert!(count_value(-1).is_err());
    assert!(count_value(i64::from(u32::MAX) + 1).is_err());
}

#[tokio::test]
async fn the_cache_runs_one_build_per_key_at_a_time() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let cache: TtlCache<u32> = TtlCache::new(8);
    let built = AtomicU32::new(0);
    let build = || async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(built.fetch_add(1, Ordering::SeqCst) + 1)
    };
    let ttl = Duration::from_secs(60);
    let (a, b, c) = tokio::join!(
        cache.get_or("k", ttl, build),
        cache.get_or("k", ttl, build),
        cache.get_or("k", ttl, build)
    );
    assert_eq!((a.unwrap(), b.unwrap(), c.unwrap()), (1, 1, 1));
    assert_eq!(built.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_cache_keeps_at_most_its_limit_of_keys() {
    let cache: TtlCache<u32> = TtlCache::new(3);
    let ttl = Duration::from_secs(60);
    for (i, key) in ["a", "b", "c", "d", "e"].into_iter().enumerate() {
        cache
            .get_or(key, ttl, || async { Ok(i as u32) })
            .await
            .unwrap();
        assert!(cache.len() <= 3);
    }
    // The oldest were dropped, the newest is still served.
    assert_eq!(
        cache.get_or("e", ttl, || async { Ok(99) }).await.unwrap(),
        4
    );
    assert_eq!(
        cache.get_or("a", ttl, || async { Ok(99) }).await.unwrap(),
        99
    );
}

#[tokio::test]
async fn the_moves_scan_is_marked_partial_only_past_its_limit() {
    let w = world().await;
    let in_event = w.people_ids.len() as u32 - 1; // all but the person of the other event
    for (limit, complete) in [
        (in_event - 1, false),
        (in_event, true),
        (in_event + 1, true),
    ] {
        let moves = scan_moves(&w.app.state.db, &w.event, limit).await.unwrap();
        assert_eq!(moves.complete, complete, "limit {limit}");
    }
}

#[tokio::test]
async fn the_animals_scan_is_marked_partial_only_past_its_limit() {
    let w = world().await;
    let now = store::now();
    for _ in 0..3 {
        let doc = households::HouseholdDoc {
            id: None,
            member_ids: vec![],
            head_id: None,
            pets: vec![Pet {
                species: "dog".into(),
                count: 2,
            }],
            card_code: None,
            created_by: "vol".into(),
            created_shelter_id: w.a.clone(),
            client_created_at: now,
            created_at: now,
            updated_at: now,
        };
        store::replace(&w.app.state.db, households::COLLECTION, &new_id(), &doc)
            .await
            .unwrap();
    }
    let db = &w.app.state.db;
    let a = w.a.as_str();
    let n = count_with(db, &COUNT_LIMITER, households::COLLECTION, |q| {
        q.for_all([q.field("created_shelter_id").eq(a)])
    })
    .await
    .unwrap();
    assert!(n >= 3);
    for (limit, cut) in [(n - 1, true), (n, false), (n + 1, false)] {
        assert_eq!(pets_of(db, a, limit).await.unwrap().1, cut, "limit {limit}");
    }
    // The animals of the households read, not of all of them, when cut.
    assert_eq!(pets_of(db, a, n).await.unwrap().0, 2 * n);
}

async fn build(w: &World, shelters: u32, moves: u32) -> Result<EventDashboard, ApiError> {
    build_event(
        &w.app.state.db,
        &COUNT_LIMITER,
        &w.event,
        Limits { shelters, moves },
    )
    .await
}

#[tokio::test]
async fn an_indicator_cut_at_its_limit_is_marked_partial_in_the_answer() {
    // Each world has its own database, so its own cache keys: no `clear` that would reach
    // the tests running beside this one.
    let w = world().await;
    let full = build(&w, 2, 10_000).await.unwrap();
    assert!(full.complete && full.partial_indicators.is_empty());
    assert!(!full.generated_at.is_empty());
    assert_eq!(full.totals.unique_people, 11);

    let cut = build(&w, 1, 10_000).await.unwrap();
    assert!(!cut.complete);
    assert_eq!(cut.partial_indicators, ["shelters"]);

    let w = world().await;
    let cut = build(&w, 2, 3).await.unwrap();
    assert!(!cut.complete);
    assert_eq!(cut.partial_indicators, ["exits"]);
}

#[tokio::test]
async fn the_list_is_marked_partial_only_past_the_event_or_shelter_limit() {
    let w = world().await;
    let db = &w.app.state.db;
    // Two events; the first has two shelters.
    for (events, shelters, complete) in [(1, 2, false), (2, 1, false), (2, 2, true), (3, 3, true)] {
        let list = build_list(db, events, shelters).await.unwrap();
        assert_eq!(
            list["complete"], complete,
            "{events} events, {shelters} shelters"
        );
        assert!(list["generated_at"].is_string());
    }
}

#[tokio::test]
async fn the_served_panel_says_when_it_was_built_and_that_it_is_complete() {
    let w = world().await;
    let (_, body) = get_json(
        &w,
        &w.civil_defense,
        &format!("/v1/dashboard/events/{}", w.event),
    )
    .await;
    assert_eq!(body["complete"], true);
    assert_eq!(body["partial_indicators"], json!([]));
    assert!(body["generated_at"].as_str().is_some_and(|s| !s.is_empty()));
}
