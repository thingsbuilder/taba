use super::domain::{
    PersonData, enter_shelter, merge_into, new_person, repoint_guardians, repoint_household,
};
use super::persist::{CAPTURES, PersonRef};
use super::view::visible_history;
use super::*;
use super::{domain, persist, view};
use crate::audit::LegalBasis;
use crate::auth::CaptureDoc;
use crate::auth::Volunteer;
use crate::households::{self, HouseholdDoc};
use crate::store;
use crate::test_support::{Actor, TestApp, audit_actions, new_id, north_of_origin};
use crate::{alerts, thumbnail};
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use firestore::FirestoreInstant;
use serde_json::{Value, json};

const JPEG: [u8; 6] = [0xFF, 0xD8, 0xFF, 0xE0, 1, 2];

fn at(text: &str) -> FirestoreInstant {
    text.parse().unwrap()
}

fn stay(shelter: &str, entered: &str) -> Stay {
    Stay {
        shelter_id: shelter.into(),
        event_id: None,
        entered_at: at(entered),
        left_at: None,
        exit_reason: None,
        exit_destination: None,
        destination_municipality: None,
        registered_by: "v".into(),
    }
}

fn person_in(shelter: &str) -> PersonDoc {
    let mut person = new_person("p", Level::Assisted, LegalBasis::Consent, store::now());
    enter_shelter(
        &mut person,
        shelter,
        None,
        at("2026-10-21T10:00:00Z"),
        "v",
        store::now(),
    );
    person
}

// ---- units ----

#[test]
fn the_level_never_goes_down() {
    for (from, to, expected) in [
        (Level::NoBiometrics, Level::Assisted, Level::Assisted),
        (Level::Assisted, Level::Verified, Level::Verified),
        (Level::Verified, Level::Assisted, Level::Verified),
        (Level::Verified, Level::NoBiometrics, Level::Verified),
        (Level::Assisted, Level::NoBiometrics, Level::Assisted),
    ] {
        assert_eq!(from.raised_to(to), expected, "{from:?} -> {to:?}");
    }
}

#[test]
fn a_new_stay_closes_the_one_open_elsewhere() {
    let mut person = person_in("a");
    let now = store::now();
    assert!(enter_shelter(&mut person, "b", Some("ev"), now, "v", now));
    assert_eq!(person.current_shelter_id.as_deref(), Some("b"));
    assert_eq!(person.stays.len(), 2);
    assert_eq!(
        person.stays[0].exit_reason,
        Some(ExitReason::CheckinElsewhere)
    );
    assert!(person.stays[0].left_at.is_some());
    assert!(person.stays[1].left_at.is_none());
    assert_eq!(person.event_ids, vec!["ev".to_string()]);
}

#[test]
fn checking_in_again_where_the_person_is_changes_nothing() {
    let mut person = person_in("a");
    assert!(!enter_shelter(
        &mut person,
        "a",
        None,
        store::now(),
        "v",
        store::now()
    ));
    assert_eq!(person.stays.len(), 1);
}

#[test]
fn an_older_offline_check_in_is_recorded_closed_and_keeps_the_current_shelter() {
    let mut person = person_in("a");
    let now = store::now();
    enter_shelter(&mut person, "b", None, at("2026-10-21T09:00:00Z"), "v", now);
    assert_eq!(person.current_shelter_id.as_deref(), Some("a"));
    assert_eq!(person.stays.len(), 2);
    let older = &person.stays[1];
    assert_eq!(older.left_at, Some(at("2026-10-21T10:00:00Z")));
    assert_eq!(older.exit_reason, Some(ExitReason::CheckinElsewhere));
    assert_eq!(
        person.stays.iter().filter(|s| s.left_at.is_none()).count(),
        1
    );
}

#[test]
fn history_follows_role_and_confirmation() {
    let stays = vec![
        stay("here", "2026-10-21T10:00:00Z"),
        stay("there", "2026-10-20T10:00:00Z"),
    ];
    let shelters = |v: &Value| -> Vec<String> {
        v["stays"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["shelter_id"].as_str().unwrap().to_string())
            .collect()
    };
    let admin = visible_history(&stays, "here", true, true);
    assert_eq!(shelters(&admin), ["here", "there"]);
    assert_eq!(admin["has_other_stays"], true);
    let volunteer = visible_history(&stays, "here", false, true);
    assert_eq!(shelters(&volunteer), ["here"]);
    assert_eq!(volunteer["has_other_stays"], true);
    for admin in [true, false] {
        let unconfirmed = visible_history(&stays, "here", admin, false);
        assert_eq!(shelters(&unconfirmed), ["here"]);
        assert_eq!(unconfirmed["has_other_stays"], false);
    }
    let alone = visible_history(&stays[..1], "here", false, true);
    assert_eq!(alone["has_other_stays"], false);
}

#[test]
fn data_accepts_only_the_four_needs_and_known_vulnerabilities() {
    assert!(PersonData::parse(json!({ "needs": { "reduced_mobility": true } })).is_ok());
    for bad in [
        json!({ "needs": { "diagnosis": "diabetes" } }),
        json!({ "needs": { "reduced_mobility": "needs a wheelchair" } }),
        json!({ "vulnerabilities": { "notes": "x" } }),
        json!({ "vulnerabilities": { "disability": ["flying"] } }),
        json!({ "religion": "x" }),
    ] {
        let error = PersonData::parse(bad.clone()).unwrap_err();
        assert_eq!(
            (error.status, error.code),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_person"),
            "{bad}"
        );
    }
}

#[test]
fn data_is_trimmed_and_bounded() {
    let data = PersonData::parse(json!({ "name": "  Ana  ", "wristband": "   " })).unwrap();
    assert_eq!(data.name.as_deref(), Some("Ana"));
    assert_eq!(data.wristband, None);
    assert!(
        !PersonData::parse(json!({ "wristband": " " }))
            .unwrap()
            .identifies()
    );
    for bad in [
        json!({ "name": "x".repeat(101) }),
        json!({ "birth_year": 1850 }),
        json!({ "birth_year": 2999 }),
        json!({ "is_child": false, "child_status": "unaccompanied" }),
    ] {
        assert!(PersonData::parse(bad.clone()).is_err(), "{bad}");
    }
    assert_eq!(
        PersonData::parse(json!({ "child_status": "separated" }))
            .unwrap()
            .is_child,
        Some(true)
    );
}

#[test]
fn filling_a_name_or_a_wristband_drops_the_thumbnail_in_the_same_write() {
    for data in [json!({ "name": "Ana" }), json!({ "wristband": "B-1" })] {
        let mut person = person_in("a");
        person.thumbnail_encrypted = Some(vec![1, 2, 3]);
        PersonData::parse(json!({ "needs": { "reduced_mobility": true } }))
            .unwrap()
            .apply(&mut person, false)
            .unwrap();
        assert!(person.thumbnail_encrypted.is_some());
        PersonData::parse(data)
            .unwrap()
            .apply(&mut person, false)
            .unwrap();
        assert!(person.thumbnail_encrypted.is_none());
    }
}

#[test]
fn merge_joins_stays_keeps_one_open_and_remembers_the_old_id() {
    let mut old = new_person(
        "old",
        Level::NoBiometrics,
        LegalBasis::Consent,
        store::now(),
    );
    old.name = Some("Ana".into());
    old.stays = vec![stay("a", "2026-10-21T10:00:00Z")];
    old.current_shelter_id = Some("a".into());
    let mut new = new_person("new", Level::Assisted, LegalBasis::Consent, store::now());
    new.stays = vec![stay("b", "2026-10-21T08:00:00Z")];
    new.current_shelter_id = Some("b".into());
    let (merged, _) = merge_into(new, old, "old", Level::Verified);
    assert_eq!(merged.level, Level::Verified);
    assert_eq!(merged.former_ids, vec!["old".to_string()]);
    assert_eq!(merged.name.as_deref(), Some("Ana"));
    assert_eq!(merged.stays.len(), 2);
    assert_eq!(
        merged.stays.iter().filter(|s| s.left_at.is_none()).count(),
        1
    );
    assert_eq!(merged.current_shelter_id.as_deref(), Some("a"));
    assert_eq!(
        merged.stays[0].exit_reason,
        Some(ExitReason::CheckinElsewhere)
    );
}

#[test]
fn household_and_guardian_references_move_to_the_new_id() {
    let now = store::now();
    let mut household = HouseholdDoc {
        id: None,
        member_ids: vec!["x".into(), "old".into(), "new".into()],
        head_id: Some("old".into()),
        pets: vec![],
        card_code: None,
        created_by: "v".into(),
        created_shelter_id: "s".into(),
        client_created_at: now,
        created_at: now,
        updated_at: now,
    };
    repoint_household(&mut household, "old", "new");
    assert_eq!(household.member_ids, ["x", "new"]);
    assert_eq!(household.head_id.as_deref(), Some("new"));
    let guardian = |id: &str| Guardian {
        person_id: id.into(),
        link: GuardianLink::Declared,
        checked_by: None,
        checked_at: None,
    };
    let mut guardians = vec![guardian("old"), guardian("z")];
    assert!(repoint_guardians(&mut guardians, "old", "new"));
    assert_eq!(guardians[0].person_id, "new");
    assert!(!repoint_guardians(&mut guardians, "old", "new"));
}

// ---- API ----

struct World {
    app: TestApp,
    admin: Actor,
    volunteer: Actor,
    shelter: String,
}

/// A confirmed shelter with its admin and one volunteer.
async fn world() -> World {
    let app = TestApp::new().await;
    let admin = app.actor("Admin").await;
    let shelter = app
        .confirmed_shelter(&admin, "Shelter A", north_of_origin(0.0))
        .await;
    let code = app.invite(&admin, &shelter, "link", "volunteer").await;
    let volunteer = app.join(&code, "Volunteer").await;
    World {
        app,
        admin,
        volunteer,
        shelter,
    }
}

async fn call(
    app: &TestApp,
    who: &Actor,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let auth = format!("Bearer {}", who.secret);
    let (status, _, json) = app
        .request(
            method,
            path,
            &[
                ("authorization", auth.as_str()),
                ("x-device-id", who.device.as_str()),
            ],
            body,
        )
        .await;
    (status, json)
}

fn checkin(extra: Value) -> Value {
    let mut body = json!({
        "id": new_id(),
        "legal_basis": "consent",
        "client_created_at": store::now().to_string(),
        "data": {},
    });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

fn photo() -> Value {
    json!(STANDARD.encode(b"a face"))
}

fn finished(token: &str) -> (u16, Value) {
    (
        200,
        json!({ "id": "img", "state": "PROCESS_STATE_FINISHED", "data": { "humanToken": { "value": token } } }),
    )
}

/// The next `n` photos sent to Unico come back as `token`.
fn photos_are(app: &TestApp, token: &str, n: usize) {
    app.unico
        .with(|s| (0..n).for_each(|_| s.post_replies.push_back(finished(token))));
}

/// A finished `checkin`/`biometrics` capture of the actor's device for `token`; `target` is
/// the shelter (check-in) or the person (biometrics) it is asked for.
async fn capture(app: &TestApp, who: &Actor, purpose: &str, token: &str, target: &str) -> String {
    let body = match purpose {
        "checkin" => json!({ "purpose": purpose, "shelter_id": target }),
        _ => json!({ "purpose": purpose, "expected_person_id": target }),
    };
    let auth = format!("Bearer {}", who.secret);
    let (status, _, body) = app
        .request(
            "POST",
            "/v1/captures",
            &[
                ("x-device-id", who.device.as_str()),
                ("authorization", auth.as_str()),
            ],
            Some(body),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["capture_id"].as_str().unwrap().to_string();
    let stored: CaptureDoc = store::get(&app.state.db, CAPTURES, &id)
        .await
        .unwrap()
        .unwrap();
    app.unico.finish_with_token(&stored.process_id, token);
    id
}

async fn stored(app: &TestApp, id: &str) -> PersonDoc {
    store::get(&app.state.db, COLLECTION, id)
        .await
        .unwrap()
        .unwrap()
}

fn token_id(app: &TestApp, token: &str) -> String {
    app.state.person_id_for_token(token)
}

#[tokio::test]
async fn checkin_with_a_selfie_creates_a_verified_person() {
    let w = world().await;
    let capture_id = capture(&w.app, &w.volunteer, "checkin", "tok-ana", &w.shelter).await;
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(
            json!({ "capture_id": capture_id, "data": { "name": "Ana" } }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = token_id(&w.app, "tok-ana");
    assert_eq!(
        (
            body["person_id"].as_str(),
            body["level"].as_str(),
            body["is_new"].as_bool()
        ),
        (Some(id.as_str()), Some("verified"), Some(true))
    );
    assert_eq!(body["history"]["has_other_stays"], false);
    let person = stored(&w.app, &id).await;
    assert_eq!(person.name.as_deref(), Some("Ana"));
    assert_eq!(
        person.current_shelter_id.as_deref(),
        Some(w.shelter.as_str())
    );
    assert_eq!(person.stays.len(), 1);
    assert!(
        audit_actions(&w.app.state.db)
            .await
            .contains(&"person.checkin".to_string())
    );
    // the capture is spent
    let again = call(
        &w.app,
        &w.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(json!({ "capture_id": capture_id }))),
    )
    .await;
    assert_eq!(
        (again.0, again.1["error"].as_str()),
        (StatusCode::CONFLICT, Some("capture_consumed"))
    );
}

#[tokio::test]
async fn checkin_with_a_photo_is_assisted_and_waits_for_a_selfie() {
    let w = world().await;
    photos_are(&w.app, "tok-bia", 1);
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(json!({ "photo_base64": photo() }))),
    )
    .await;
    assert_eq!(
        (status, body["level"].as_str()),
        (StatusCode::CREATED, Some("assisted")),
        "{body}"
    );
    let (_, list) = call(
        &w.app,
        &w.admin,
        "GET",
        &format!("/v1/shelters/{}/people/needs-selfie", w.shelter),
        None,
    )
    .await;
    assert_eq!(list["people"].as_array().unwrap().len(), 1);
    assert_eq!(list["people"][0]["id"], body["person_id"]);
}

#[tokio::test]
async fn checkin_without_biometrics_uses_the_client_id_and_the_refusal_leaves_the_selfie_list() {
    let w = world().await;
    let calls_before = w.app.unico.post_count();
    let (declined, kept) = (new_id(), new_id());
    for (id, refused) in [(&declined, true), (&kept, false)] {
        let (status, body) = call(&w.app, &w.volunteer, "POST", &format!("/v1/shelters/{}/checkins", w.shelter),
            Some(checkin(json!({ "id": id, "no_biometrics": true, "biometrics_declined": refused, "data": { "name": "N" } })))).await;
        assert_eq!(
            (status, body["level"].as_str(), body["person_id"].as_str()),
            (
                StatusCode::CREATED,
                Some("no_biometrics"),
                Some(id.as_str())
            ),
            "{body}"
        );
    }
    assert!(stored(&w.app, &declined).await.biometrics_declined);
    assert_eq!(w.app.unico.post_count(), calls_before);
    let (_, selfies) = call(
        &w.app,
        &w.admin,
        "GET",
        &format!("/v1/shelters/{}/people/needs-selfie", w.shelter),
        None,
    )
    .await;
    let ids: Vec<&str> = selfies["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [kept.as_str()]);
    let (_, all) = call(
        &w.app,
        &w.admin,
        "GET",
        &format!("/v1/shelters/{}/people", w.shelter),
        None,
    )
    .await;
    assert_eq!(all["people"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn exactly_one_way_to_identify_is_required_and_only_members_check_in() {
    let w = world().await;
    let path = format!("/v1/shelters/{}/checkins", w.shelter);
    for extra in [
        json!({}),
        json!({ "no_biometrics": true, "photo_base64": photo() }),
        json!({ "capture_id": "c", "photo_base64": photo() }),
    ] {
        let (status, body) = call(&w.app, &w.volunteer, "POST", &path, Some(checkin(extra))).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_person")),
            "{body}"
        );
    }
    let stranger = w.app.actor("Stranger").await;
    let (status, _) = call(
        &w.app,
        &stranger,
        "POST",
        &path,
        Some(checkin(json!({ "no_biometrics": true }))),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &w.app,
        &stranger,
        "GET",
        &format!("/v1/shelters/{}/people", w.shelter),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &w.app,
        &w.volunteer,
        "POST",
        "/v1/shelters/nope/checkins",
        Some(checkin(json!({ "no_biometrics": true }))),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_image_that_is_not_base64_is_refused_before_unico() {
    let w = world().await;
    let calls_before = w.app.unico.post_count();
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(json!({ "photo_base64": "***" }))),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_image"))
    );
    assert_eq!(w.app.unico.post_count(), calls_before);
}

// criterion 24
#[tokio::test]
async fn a_photo_checkin_then_a_selfie_is_the_same_document_raised_to_verified() {
    let w = world().await;
    let path = format!("/v1/shelters/{}/checkins", w.shelter);
    photos_are(&w.app, "tok-1", 1);
    let (_, first) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &path,
        Some(checkin(
            json!({ "photo_base64": photo(), "data": { "name": "Ana" } }),
        )),
    )
    .await;
    let capture_id = capture(&w.app, &w.volunteer, "checkin", "tok-1", &w.shelter).await;
    let (status, second) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &path,
        Some(checkin(json!({ "capture_id": capture_id }))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    assert_eq!(
        (
            second["person_id"].clone(),
            second["level"].as_str(),
            second["is_new"].as_bool()
        ),
        (first["person_id"].clone(), Some("verified"), Some(false))
    );
    assert_eq!(
        crate::test_support::all_docs(&w.app.state.db, COLLECTION)
            .await
            .len(),
        1
    );
    let person = stored(&w.app, first["person_id"].as_str().unwrap()).await;
    assert_eq!(person.name.as_deref(), Some("Ana"));
    assert_eq!(person.stays.len(), 1);
    // and a later photo does not take it back down
    photos_are(&w.app, "tok-1", 1);
    let (_, third) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &path,
        Some(checkin(json!({ "photo_base64": photo() }))),
    )
    .await;
    assert_eq!(third["level"], "verified");
}

async fn second_shelter(w: &World) -> (String, Actor) {
    let other = w.app.actor("Other").await;
    let shelter = w
        .app
        .confirmed_shelter(&other, "Shelter B", north_of_origin(500.0))
        .await;
    (shelter, other)
}

// criterion 23
#[tokio::test]
async fn the_same_token_in_two_shelters_is_one_document_two_stays_one_open() {
    let w = world().await;
    let (b, other) = second_shelter(&w).await;
    photos_are(&w.app, "tok-1", 2);
    let (_, first) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(json!({ "photo_base64": photo() }))),
    )
    .await;
    let (status, second) = call(
        &w.app,
        &other,
        "POST",
        &format!("/v1/shelters/{b}/checkins"),
        Some(checkin(json!({ "photo_base64": photo() }))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    assert_eq!(first["person_id"], second["person_id"]);
    assert_eq!(second["is_new"], false);
    let person = stored(&w.app, first["person_id"].as_str().unwrap()).await;
    assert_eq!(person.stays.len(), 2);
    assert_eq!(
        person.stays.iter().filter(|s| s.left_at.is_none()).count(),
        1
    );
    assert_eq!(
        person.stays[0].exit_reason,
        Some(ExitReason::CheckinElsewhere)
    );
    assert_eq!(person.current_shelter_id.as_deref(), Some(b.as_str()));
    assert_eq!(
        crate::test_support::all_docs(&w.app.state.db, COLLECTION)
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn concurrent_checkins_of_one_token_leave_one_document_and_one_open_stay() {
    let w = world().await;
    let (b, other) = second_shelter(&w).await;
    photos_are(&w.app, "tok-1", 1);
    call(
        &w.app,
        &w.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(json!({ "photo_base64": photo() }))),
    )
    .await;
    let n = 6;
    photos_are(&w.app, "tok-1", n);
    let mut tasks = Vec::new();
    for _ in 0..n {
        let (app, who, shelter) = (w.app.clone(), other.clone(), b.clone());
        tasks.push(tokio::spawn(async move {
            call(
                &app,
                &who,
                "POST",
                &format!("/v1/shelters/{shelter}/checkins"),
                Some(checkin(json!({ "photo_base64": photo() }))),
            )
            .await
        }));
    }
    for task in tasks {
        let (status, body) = task.await.unwrap();
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    assert_eq!(
        crate::test_support::all_docs(&w.app.state.db, COLLECTION)
            .await
            .len(),
        1
    );
    let person = stored(&w.app, &token_id(&w.app, "tok-1")).await;
    assert_eq!(person.stays.len(), 2, "{:?}", person.stays);
    assert_eq!(
        person.stays.iter().filter(|s| s.left_at.is_none()).count(),
        1
    );
    assert_eq!(person.current_shelter_id.as_deref(), Some(b.as_str()));
}

#[tokio::test]
async fn one_capture_used_by_concurrent_checkins_is_consumed_once() {
    let w = world().await;
    let capture_id = capture(&w.app, &w.volunteer, "checkin", "tok-1", &w.shelter).await;
    let mut tasks = Vec::new();
    for _ in 0..5 {
        let (app, who, shelter, capture_id) = (
            w.app.clone(),
            w.volunteer.clone(),
            w.shelter.clone(),
            capture_id.clone(),
        );
        tasks.push(tokio::spawn(async move {
            call(
                &app,
                &who,
                "POST",
                &format!("/v1/shelters/{shelter}/checkins"),
                Some(checkin(json!({ "capture_id": capture_id }))),
            )
            .await
        }));
    }
    let mut statuses = Vec::new();
    for task in tasks {
        let (status, body) = task.await.unwrap();
        statuses.push((status, body["error"].as_str().map(str::to_string)));
    }
    assert_eq!(
        statuses
            .iter()
            .filter(|s| s.0 == StatusCode::CREATED)
            .count(),
        1,
        "{statuses:?}"
    );
    assert!(
        statuses
            .iter()
            .filter(|s| s.0 != StatusCode::CREATED)
            .all(|s| s.1.as_deref() == Some("capture_consumed")),
        "{statuses:?}"
    );
    let capture: CaptureDoc = store::get(&w.app.state.db, CAPTURES, &capture_id)
        .await
        .unwrap()
        .unwrap();
    assert!(capture.consumed);
    assert_eq!(
        stored(&w.app, &token_id(&w.app, "tok-1")).await.stays.len(),
        1
    );
}

// criterion 36
#[tokio::test]
async fn a_volunteer_sees_only_that_there_are_other_stays_and_the_admin_sees_them() {
    let w = world().await;
    let (b, other) = second_shelter(&w).await;
    photos_are(&w.app, "tok-1", 2);
    call(
        &w.app,
        &other,
        "POST",
        &format!("/v1/shelters/{b}/checkins"),
        Some(checkin(json!({ "photo_base64": photo() }))),
    )
    .await;
    let (_, body) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(json!({ "photo_base64": photo() }))),
    )
    .await;
    assert_eq!(body["history"]["has_other_stays"], true);
    assert_eq!(body["history"]["stays"].as_array().unwrap().len(), 1);
    let id = body["person_id"].as_str().unwrap();
    let (_, as_volunteer) = call(
        &w.app,
        &w.volunteer,
        "GET",
        &format!("/v1/people/{id}"),
        None,
    )
    .await;
    assert_eq!(
        as_volunteer["person"]["history"]["stays"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(as_volunteer["person"]["history"]["has_other_stays"], true);
    let (_, as_admin) = call(&w.app, &w.admin, "GET", &format!("/v1/people/{id}"), None).await;
    let stays = as_admin["person"]["history"]["stays"].as_array().unwrap();
    assert_eq!(stays.len(), 2);
    assert!(stays.iter().any(|s| s["shelter_id"] == b.as_str()));
    // the other shelter's admin, no longer the current one, sees their own stay and the other as history
    let (_, as_other) = call(&w.app, &other, "GET", &format!("/v1/people/{id}"), None).await;
    assert_eq!(
        as_other["person"]["history"]["stays"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn an_unconfirmed_shelter_sees_only_its_own_stays() {
    let app = TestApp::new().await;
    let admin = app.actor("Admin").await;
    let confirmed = app
        .confirmed_shelter(&admin, "Confirmed", north_of_origin(0.0))
        .await;
    let other = app.actor("Other").await;
    let unconfirmed = app
        .create_shelter(&other, "Unconfirmed", north_of_origin(500.0))
        .await;
    photos_are(&app, "tok-1", 2);
    call(
        &app,
        &admin,
        "POST",
        &format!("/v1/shelters/{confirmed}/checkins"),
        Some(checkin(json!({ "photo_base64": photo() }))),
    )
    .await;
    let (_, body) = call(
        &app,
        &other,
        "POST",
        &format!("/v1/shelters/{unconfirmed}/checkins"),
        Some(checkin(json!({ "photo_base64": photo() }))),
    )
    .await;
    assert_eq!(body["history"]["has_other_stays"], false);
    assert_eq!(body["history"]["stays"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn people_nobody_of_the_viewer_registered_are_not_found() {
    let w = world().await;
    let id = new_id();
    call(
        &w.app,
        &w.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(json!({ "id": id, "no_biometrics": true }))),
    )
    .await;
    let stranger = w.app.actor("Stranger").await;
    for (method, body) in [
        ("GET", None),
        (
            "PATCH",
            Some(json!({ "data": {}, "client_edited_at": store::now().to_string() })),
        ),
    ] {
        let (status, _) = call(&w.app, &stranger, method, &format!("/v1/people/{id}"), body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method}");
    }
    let (status, _) = call(
        &w.app,
        &stranger,
        "PUT",
        &format!("/v1/people/{id}/biometrics"),
        Some(json!({ "photo_base64": photo() })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&w.app, &w.volunteer, "GET", "/v1/people/missing", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// criterion 38
#[tokio::test]
async fn needs_take_only_the_four_booleans_on_checkin_and_patch() {
    let w = world().await;
    let path = format!("/v1/shelters/{}/checkins", w.shelter);
    let (status, body) = call(&w.app, &w.volunteer, "POST", &path,
        Some(checkin(json!({ "no_biometrics": true, "data": { "needs": { "continuous_medication": "insulin 2x" } } })))).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_person")),
        "{body}"
    );
    let id = new_id();
    let (status, _) = call(&w.app, &w.volunteer, "POST", &path,
        Some(checkin(json!({ "id": id, "no_biometrics": true, "data": { "needs": { "reduced_mobility": true }, "vulnerabilities": { "pregnant": true, "disability": ["visual"], "migrant_language": "es" } } })))).await;
    assert_eq!(status, StatusCode::CREATED);
    let edited = store::now().to_string();
    let (status, _) = call(
        &w.app,
        &w.volunteer,
        "PATCH",
        &format!("/v1/people/{id}"),
        Some(json!({ "data": { "needs": { "allergy": "peanuts" } }, "client_edited_at": edited })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, body) = call(&w.app, &w.volunteer, "PATCH", &format!("/v1/people/{id}"),
        Some(json!({ "data": { "needs": { "dietary_restriction": true } }, "client_edited_at": edited }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let person = stored(&w.app, &id).await;
    assert!(person.needs.dietary_restriction && !person.needs.reduced_mobility);
    assert!(person.vulnerabilities.pregnant);
    assert_eq!(
        person.vulnerabilities.migrant_language.as_deref(),
        Some("es")
    );
}

#[tokio::test]
async fn the_last_edit_wins_and_an_older_one_is_ignored() {
    let w = world().await;
    let id = new_id();
    call(
        &w.app,
        &w.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(json!({ "id": id, "no_biometrics": true }))),
    )
    .await;
    let path = format!("/v1/people/{id}");
    let edit = |name: &str, time: &str| {
        Some(json!({ "data": { "name": name }, "client_edited_at": time }))
    };
    let (status, _) = call(
        &w.app,
        &w.volunteer,
        "PATCH",
        &path,
        edit("Second", "2026-10-21T12:00:00Z"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "PATCH",
        &path,
        edit("First", "2026-10-21T11:00:00Z"),
    )
    .await;
    assert_eq!(
        (status, body["person"]["name"].as_str()),
        (StatusCode::OK, Some("Second"))
    );
    assert_eq!(stored(&w.app, &id).await.name.as_deref(), Some("Second"));
    let (status, _) = call(
        &w.app,
        &w.volunteer,
        "PATCH",
        &path,
        Some(json!({ "data": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn child_status_changes_only_by_protection_or_civil_defense() {
    let w = world().await;
    let id = new_id();
    call(&w.app, &w.volunteer, "POST", &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(json!({ "id": id, "no_biometrics": true, "legal_basis": "child_protection", "data": { "is_child": true, "child_status": "unaccompanied" } })))).await;
    let path = format!("/v1/people/{id}");
    let patch = |status: &str| {
        Some(
            json!({ "data": { "child_status": status }, "client_edited_at": store::now().to_string() }),
        )
    };
    for who in [&w.volunteer, &w.admin] {
        let (status, body) = call(&w.app, who, "PATCH", &path, patch("with_guardian")).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::FORBIDDEN, Some("protection_role_required"))
        );
    }
    // the same value is not a change
    let (status, _) = call(&w.app, &w.volunteer, "PATCH", &path, patch("unaccompanied")).await;
    assert_eq!(status, StatusCode::OK);
    w.app
        .grant(&w.admin, crate::volunteers::GlobalRole::Protection)
        .await;
    let (status, body) = call(&w.app, &w.admin, "PATCH", &path, patch("with_guardian")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        stored(&w.app, &id).await.child_status,
        Some(ChildStatus::WithGuardian)
    );
}

#[tokio::test]
async fn an_ordinary_edit_cannot_clear_the_child_condition() {
    let w = world().await;
    let id = new_id();
    call(&w.app, &w.volunteer, "POST", &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(json!({ "id": id, "no_biometrics": true, "legal_basis": "child_protection", "data": { "child_status": "separated" } })))).await;
    let path = format!("/v1/people/{id}");
    for who in [&w.volunteer, &w.admin] {
        let (status, body) = call(&w.app, who, "PATCH", &path,
            Some(json!({ "data": { "is_child": false }, "client_edited_at": store::now().to_string() }))).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::FORBIDDEN, Some("protection_role_required")),
            "{body}"
        );
    }
    let person = stored(&w.app, &id).await;
    assert!(person.is_child);
    assert_eq!(person.child_status, Some(ChildStatus::Separated));
    // protection may lower both together
    w.app
        .grant(&w.admin, crate::volunteers::GlobalRole::Protection)
        .await;
    let (status, body) = call(
        &w.app,
        &w.admin,
        "PATCH",
        &path,
        Some(
            json!({ "data": { "is_child": false }, "client_edited_at": store::now().to_string() }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let person = stored(&w.app, &id).await;
    assert!(!person.is_child);
    assert_eq!(person.child_status, None);
}

// criteria 39 and 40, and the storage of the bytes
#[tokio::test]
async fn the_thumbnail_is_stored_encrypted_as_bytes_audited_on_read_and_dropped_with_a_name() {
    let w = world().await;
    let id = new_id();
    let upload = STANDARD.encode(JPEG);
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(
            json!({ "id": id, "no_biometrics": true, "thumbnail_base64": upload }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let person = stored(&w.app, &id).await;
    let sealed = person.thumbnail_encrypted.clone().unwrap();
    assert!(!sealed.windows(JPEG.len()).any(|w| w == JPEG));
    assert_eq!(
        thumbnail::decrypt(w.app.state.thumbnail_key(), &sealed).unwrap(),
        JPEG
    );

    // Firestore holds a bytes value, not an array of integers
    let db = &w.app.state.db;
    let url = format!(
        "http://{}/v1/{}/documents/people/{id}",
        std::env::var("FIRESTORE_EMULATOR_HOST").unwrap(),
        db.get_database_path()
    );
    let raw: Value = reqwest::Client::new()
        .get(url)
        .header("authorization", "Bearer owner")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        raw["fields"]["thumbnail_encrypted"]["bytesValue"].is_string(),
        "{raw}"
    );

    let path = format!("/v1/people/{id}/thumbnail");
    let auth = format!("Bearer {}", w.volunteer.secret);
    let (status, headers, _) = w
        .app
        .request("GET", &path, &[("authorization", auth.as_str())], None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/jpeg");
    assert!(
        audit_actions(db)
            .await
            .contains(&"thumbnail.view".to_string())
    );
    let (_, listed) = call(
        &w.app,
        &w.volunteer,
        "GET",
        &format!("/v1/people/{id}"),
        None,
    )
    .await;
    assert_eq!(listed["person"]["has_thumbnail"], true);
    assert!(!listed.to_string().contains("thumbnail_encrypted"));

    // a volunteer of another shelter: 403, and nothing is audited for it
    let (_, other) = second_shelter(&w).await;
    let views_before = audit_actions(db)
        .await
        .iter()
        .filter(|a| *a == "thumbnail.view")
        .count();
    let (status, _) = call(&w.app, &other, "GET", &path, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        audit_actions(db)
            .await
            .iter()
            .filter(|a| *a == "thumbnail.view")
            .count(),
        views_before
    );

    // the same write that fills the name removes the thumbnail
    let (status, _) = call(
        &w.app,
        &w.volunteer,
        "PATCH",
        &format!("/v1/people/{id}"),
        Some(json!({ "data": { "name": "Ana" }, "client_edited_at": store::now().to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(stored(&w.app, &id).await.thumbnail_encrypted.is_none());
    let (status, body) = call(&w.app, &w.volunteer, "GET", &path, None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("thumbnail_not_found"))
    );
}

#[tokio::test]
async fn a_thumbnail_is_not_kept_for_someone_with_a_name_or_wristband_and_must_be_a_jpeg() {
    let w = world().await;
    let path = format!("/v1/shelters/{}/checkins", w.shelter);
    let id = new_id();
    call(&w.app, &w.volunteer, "POST", &path, Some(checkin(json!({ "id": id, "no_biometrics": true, "data": { "wristband": "B-7" }, "thumbnail_base64": STANDARD.encode(JPEG) })))).await;
    assert!(stored(&w.app, &id).await.thumbnail_encrypted.is_none());
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &path,
        Some(checkin(
            json!({ "no_biometrics": true, "thumbnail_base64": STANDARD.encode(b"not a jpeg") }),
        )),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_thumbnail"))
    );
}

// ---- late biometrics (criterion 26) ----

fn household(members: &[&str], head: Option<&str>) -> HouseholdDoc {
    let now = store::now();
    HouseholdDoc {
        id: None,
        member_ids: members.iter().map(|m| m.to_string()).collect(),
        head_id: head.map(str::to_string),
        pets: vec![],
        card_code: None,
        created_by: "v".into(),
        created_shelter_id: "s".into(),
        client_created_at: now,
        created_at: now,
        updated_at: now,
    }
}

async fn put_household(app: &TestApp, id: &str, doc: &HouseholdDoc) {
    store::replace(&app.state.db, households::COLLECTION, id, doc)
        .await
        .unwrap();
    let mut person: PersonDoc = stored(app, doc.member_ids.first().unwrap()).await;
    person.household_id = Some(id.into());
    store::replace(
        &app.state.db,
        COLLECTION,
        doc.member_ids.first().unwrap(),
        &person,
    )
    .await
    .unwrap();
}

async fn register_without_biometrics(w: &World, name: &str) -> String {
    let id = new_id();
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(
            json!({ "id": id, "no_biometrics": true, "data": { "name": name } }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    id
}

#[tokio::test]
async fn late_biometrics_with_a_new_token_moves_the_record_and_every_reference() {
    let w = world().await;
    let old = register_without_biometrics(&w, "Ana").await;
    let kin = register_without_biometrics(&w, "Bia").await;
    put_household(&w.app, "h1", &household(&[&old, &kin], Some(&old))).await;
    // a child who lists the old record as guardian and documents of later phases
    let mut child = stored(&w.app, &kin).await;
    child.guardians.push(Guardian {
        person_id: old.clone(),
        link: GuardianLink::Declared,
        checked_by: None,
        checked_at: None,
    });
    child.sync_guardian_ids();
    store::replace(&w.app.state.db, COLLECTION, &kin, &child)
        .await
        .unwrap();
    for collection in ["deliveries", "rescues"] {
        store::replace(
            &w.app.state.db,
            collection,
            "d1",
            &PersonRef {
                id: None,
                person_id: old.clone(),
            },
        )
        .await
        .unwrap();
        store::replace(
            &w.app.state.db,
            collection,
            "d2",
            &PersonRef {
                id: None,
                person_id: kin.clone(),
            },
        )
        .await
        .unwrap();
    }

    let capture_id = capture(&w.app, &w.volunteer, "biometrics", "tok-ana", &old).await;
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "PUT",
        &format!("/v1/people/{old}/biometrics"),
        Some(json!({ "capture_id": capture_id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let new = token_id(&w.app, "tok-ana");
    assert_eq!(
        (body["person_id"].as_str(), body["level"].as_str()),
        (Some(new.as_str()), Some("verified"))
    );

    let moved = stored(&w.app, &new).await;
    assert_eq!(moved.name.as_deref(), Some("Ana"));
    assert_eq!(moved.former_ids, vec![old.clone()]);
    assert_eq!(moved.household_id.as_deref(), Some("h1"));
    assert_eq!(moved.stays.len(), 1);
    assert!(
        store::get::<PersonDoc>(&w.app.state.db, COLLECTION, &old)
            .await
            .unwrap()
            .is_none()
    );
    let h: HouseholdDoc = store::get(&w.app.state.db, households::COLLECTION, "h1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(h.member_ids, [new.clone(), kin.clone()]);
    assert_eq!(h.head_id.as_deref(), Some(new.as_str()));
    assert_eq!(stored(&w.app, &kin).await.guardians[0].person_id, new);
    for collection in ["deliveries", "rescues"] {
        let d1: PersonRef = store::get(&w.app.state.db, collection, "d1")
            .await
            .unwrap()
            .unwrap();
        let d2: PersonRef = store::get(&w.app.state.db, collection, "d2")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (d1.person_id, d2.person_id),
            (new.clone(), kin.clone()),
            "{collection}"
        );
    }
    let capture: CaptureDoc = store::get(&w.app.state.db, CAPTURES, &capture_id)
        .await
        .unwrap()
        .unwrap();
    assert!(capture.consumed);
    assert!(
        audit_actions(&w.app.state.db)
            .await
            .contains(&"person.biometrics".to_string())
    );
    // the old id now leads nowhere
    let (status, _) = call(
        &w.app,
        &w.volunteer,
        "GET",
        &format!("/v1/people/{old}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn late_biometrics_by_photo_makes_the_person_assisted() {
    let w = world().await;
    let old = register_without_biometrics(&w, "Ana").await;
    photos_are(&w.app, "tok-ana", 1);
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "PUT",
        &format!("/v1/people/{old}/biometrics"),
        Some(json!({ "photo_base64": photo() })),
    )
    .await;
    assert_eq!(
        (status, body["level"].as_str()),
        (StatusCode::OK, Some("assisted")),
        "{body}"
    );
    assert_eq!(
        stored(&w.app, &token_id(&w.app, "tok-ana")).await.level,
        Level::Assisted
    );
}

#[tokio::test]
async fn late_biometrics_of_a_token_that_already_exists_joins_the_stays() {
    let w = world().await;
    let (b, other) = second_shelter(&w).await;
    photos_are(&w.app, "tok-ana", 1);
    let (_, elsewhere) = call(
        &w.app,
        &other,
        "POST",
        &format!("/v1/shelters/{b}/checkins"),
        Some(checkin(
            json!({ "photo_base64": photo(), "client_created_at": "2026-10-20T10:00:00Z" }),
        )),
    )
    .await;
    let existing = elsewhere["person_id"].as_str().unwrap().to_string();
    let old = register_without_biometrics(&w, "Ana").await;
    let capture_id = capture(&w.app, &w.volunteer, "biometrics", "tok-ana", &old).await;
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "PUT",
        &format!("/v1/people/{old}/biometrics"),
        Some(json!({ "capture_id": capture_id })),
    )
    .await;
    assert_eq!(
        (status, body["person_id"].as_str(), body["level"].as_str()),
        (StatusCode::OK, Some(existing.as_str()), Some("verified")),
        "{body}"
    );
    let person = stored(&w.app, &existing).await;
    assert_eq!(person.stays.len(), 2);
    assert_eq!(
        person.stays.iter().filter(|s| s.left_at.is_none()).count(),
        1
    );
    assert_eq!(
        person.current_shelter_id.as_deref(),
        Some(w.shelter.as_str())
    );
    assert_eq!(person.former_ids, vec![old.clone()]);
    assert_eq!(person.name.as_deref(), Some("Ana"));
    assert_eq!(
        crate::test_support::all_docs(&w.app.state.db, COLLECTION)
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn two_different_households_keep_the_one_of_the_token_and_raise_a_conflict() {
    let w = world().await;
    photos_are(&w.app, "tok-ana", 1);
    let (_, first) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(json!({ "photo_base64": photo() }))),
    )
    .await;
    let existing = first["person_id"].as_str().unwrap().to_string();
    let old = register_without_biometrics(&w, "Ana").await;
    put_household(&w.app, "h-token", &household(&[&existing], Some(&existing))).await;
    put_household(&w.app, "h-old", &household(&[&old], Some(&old))).await;
    let capture_id = capture(&w.app, &w.volunteer, "biometrics", "tok-ana", &old).await;
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "PUT",
        &format!("/v1/people/{old}/biometrics"),
        Some(json!({ "capture_id": capture_id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        stored(&w.app, &existing).await.household_id.as_deref(),
        Some("h-token")
    );
    let kept: HouseholdDoc = store::get(&w.app.state.db, households::COLLECTION, "h-token")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept.member_ids, [existing.clone()]);
    let left: HouseholdDoc = store::get(&w.app.state.db, households::COLLECTION, "h-old")
        .await
        .unwrap()
        .unwrap();
    assert!(left.member_ids.is_empty());
    let alerts = crate::test_support::all_docs(&w.app.state.db, alerts::COLLECTION).await;
    let conflict: Vec<_> = alerts
        .iter()
        .filter(|a| a["kind"] == "household_conflict")
        .collect();
    assert_eq!(conflict.len(), 1, "{alerts:?}");
    assert_eq!(conflict[0]["shelter_ids"], json!([w.shelter]));
    assert_eq!(conflict[0]["target_id"], existing.as_str());
}

/// A request with the actor's session, device and an `Idempotency-Key`.
async fn keyed(
    app: &TestApp,
    who: &Actor,
    method: &str,
    path: &str,
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let auth = format!("Bearer {}", who.secret);
    let (status, _, json) = app
        .request(
            method,
            path,
            &[
                ("authorization", auth.as_str()),
                ("x-device-id", who.device.as_str()),
                ("idempotency-key", key),
            ],
            Some(body),
        )
        .await;
    (status, json)
}

#[tokio::test]
async fn a_photo_sent_again_after_a_failure_does_not_pay_unico_twice() {
    // Criterion 55 on the photo routes: the call to Unico comes before the transaction, so a
    // request refused after it and sent again with the same key must reuse its answer.
    let w = world().await;
    photos_are(&w.app, "tok-1", 1);
    let (_, first) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(json!({ "photo_base64": photo() }))),
    )
    .await;
    let person = first["person_id"].as_str().unwrap();
    let path = format!("/v1/people/{person}/biometrics");
    let paid = w.app.unico.post_count();

    // Someone else's face: refused after the paid call.
    photos_are(&w.app, "tok-2", 1);
    let (status, body) = keyed(
        &w.app,
        &w.volunteer,
        "PUT",
        &path,
        "key-1",
        json!({ "photo_base64": photo() }),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("person_mismatch"))
    );
    assert_eq!(w.app.unico.post_count(), paid + 1);
    for _ in 0..3 {
        let (status, body) = keyed(
            &w.app,
            &w.volunteer,
            "PUT",
            &path,
            "key-1",
            json!({ "photo_base64": photo() }),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::CONFLICT, Some("person_mismatch"))
        );
    }
    assert_eq!(
        w.app.unico.post_count(),
        paid + 1,
        "the retries made no paid call"
    );

    // Another key is another request, and asks again.
    photos_are(&w.app, "tok-2", 1);
    keyed(
        &w.app,
        &w.volunteer,
        "PUT",
        &path,
        "key-2",
        json!({ "photo_base64": photo() }),
    )
    .await;
    assert_eq!(w.app.unico.post_count(), paid + 2);
    // Without a key every call asks.
    photos_are(&w.app, "tok-2", 2);
    for _ in 0..2 {
        call(
            &w.app,
            &w.volunteer,
            "PUT",
            &path,
            Some(json!({ "photo_base64": photo() })),
        )
        .await;
    }
    assert_eq!(w.app.unico.post_count(), paid + 4);
}

#[tokio::test]
async fn a_photo_unico_refuses_is_a_422_that_is_not_repeated_nor_kept_as_paid() {
    let w = world().await;
    let refuse = |app: &TestApp, n: usize| {
        app.unico.with(|s| {
            (0..n).for_each(|_| {
                s.post_replies
                    .push_back((400, json!({ "code": 3, "message": "INVALID_REQUEST" })))
            })
        })
    };
    let path = format!("/v1/shelters/{}/checkins", w.shelter);
    let before = w.app.unico.post_count();

    // Check-in by photo: one call to Unico, no retry on the server, a 422 and nothing written.
    refuse(&w.app, 1);
    let (status, body) = keyed(
        &w.app,
        &w.volunteer,
        "POST",
        &path,
        "k-refused",
        checkin(json!({ "photo_base64": photo() })),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("photo_rejected")),
        "{body}"
    );
    assert_eq!(w.app.unico.post_count(), before + 1);
    assert!(
        crate::test_support::all_docs(&w.app.state.db, COLLECTION)
            .await
            .is_empty()
    );
    let kept = crate::test_support::all_docs(&w.app.state.db, "idempotency").await;
    assert!(
        kept.is_empty(),
        "a refusal is not kept as an answer, paid or not: {kept:?}"
    );

    // The same, late biometrics by photo.
    let person = register_without_biometrics(&w, "Ana").await;
    refuse(&w.app, 1);
    let (status, body) = keyed(
        &w.app,
        &w.volunteer,
        "PUT",
        &format!("/v1/people/{person}/biometrics"),
        "k-late",
        json!({ "photo_base64": photo() }),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("photo_rejected")),
        "{body}"
    );
    assert_eq!(stored(&w.app, &person).await.level, Level::NoBiometrics);
    assert!(
        crate::test_support::all_docs(&w.app.state.db, "idempotency")
            .await
            .is_empty()
    );

    // The codes Unico does identify keep their own.
    w.app.unico.with(|s| {
        s.post_replies
            .push_back((400, json!({ "code": 20532, "message": "no face" })))
    });
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &path,
        Some(checkin(json!({ "photo_base64": photo() }))),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("no_face"))
    );
}

#[tokio::test]
async fn a_kept_answer_is_not_handed_to_another_volunteer_or_route() {
    let w = world().await;
    photos_are(&w.app, "tok-1", 1);
    let path = format!("/v1/shelters/{}/checkins", w.shelter);
    let (status, first) = keyed(
        &w.app,
        &w.volunteer,
        "POST",
        &path,
        "key-x",
        checkin(json!({ "photo_base64": photo() })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    // A key reused by someone else gets the middleware's refusal and no kept answer.
    let (status, body) = keyed(
        &w.app,
        &w.admin,
        "POST",
        &path,
        "key-x",
        checkin(json!({ "photo_base64": photo() })),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("idempotency_key_mismatch")
        )
    );
    // The document is kept under its own id, with the id of the person and no token.
    let kept = crate::test_support::all_docs(&w.app.state.db, "idempotency").await;
    let photo_doc = kept
        .iter()
        .find(|d| d["_firestore_id"] == "photo~key-x")
        .unwrap();
    assert_eq!(photo_doc["person_id"], first["person_id"]);
    assert!(!photo_doc.to_string().contains("tok-1"));
    // `~` is not accepted in a key, so a client cannot aim at those documents.
    let (status, body) = keyed(
        &w.app,
        &w.volunteer,
        "POST",
        &path,
        "photo~key-x",
        checkin(json!({ "photo_base64": photo() })),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_idempotency_key"))
    );
}

#[tokio::test]
async fn late_biometrics_needs_one_proof_and_a_token_that_matches_the_record() {
    let w = world().await;
    let old = register_without_biometrics(&w, "Ana").await;
    let path = format!("/v1/people/{old}/biometrics");
    for bad in [
        json!({}),
        json!({ "capture_id": "c", "photo_base64": photo() }),
    ] {
        let (status, _) = call(&w.app, &w.volunteer, "PUT", &path, Some(bad)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    // a person who already has a token cannot be pointed at someone else's
    photos_are(&w.app, "tok-1", 1);
    let (_, first) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(json!({ "photo_base64": photo() }))),
    )
    .await;
    photos_are(&w.app, "tok-2", 1);
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "PUT",
        &format!(
            "/v1/people/{}/biometrics",
            first["person_id"].as_str().unwrap()
        ),
        Some(json!({ "photo_base64": photo() })),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("person_mismatch"))
    );
    // and a selfie of the same token only raises it
    let capture_id = capture(
        &w.app,
        &w.volunteer,
        "biometrics",
        "tok-1",
        first["person_id"].as_str().unwrap(),
    )
    .await;
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "PUT",
        &format!(
            "/v1/people/{}/biometrics",
            first["person_id"].as_str().unwrap()
        ),
        Some(json!({ "capture_id": capture_id })),
    )
    .await;
    assert_eq!(
        (status, body["level"].as_str()),
        (StatusCode::OK, Some("verified")),
        "{body}"
    );
}

#[tokio::test]
async fn the_unico_photo_limit_is_enforced_without_a_call() {
    let w = world().await;
    let calls_before = w.app.unico.post_count();
    let big = STANDARD.encode(vec![1u8; 800 * 1024 + 1]);
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "POST",
        &format!("/v1/shelters/{}/checkins", w.shelter),
        Some(checkin(json!({ "photo_base64": big }))),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, Some("image_too_large"))
    );
    assert_eq!(w.app.unico.post_count(), calls_before);
}

#[tokio::test]
async fn a_taken_over_item_records_the_original_author_in_the_audit() {
    let w = world().await;
    let id = new_id();
    let auth = format!("Bearer {}", w.admin.secret);
    let headers = [
        ("authorization", auth.as_str()),
        ("x-device-id", "dev-Admin"),
        ("x-taken-over-from", "volunteer-original"),
    ];
    let (status, _, body) = w
        .app
        .request(
            "POST",
            &format!("/v1/shelters/{}/checkins", w.shelter),
            &headers,
            Some(checkin(json!({ "id": id, "no_biometrics": true }))),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let patch = json!({ "data": { "name": "Ana" }, "client_edited_at": store::now().to_string() });
    let (status, _, _) = w
        .app
        .request("PATCH", &format!("/v1/people/{id}"), &headers, Some(patch))
        .await;
    assert_eq!(status, StatusCode::OK);
    let entries = crate::test_support::all_docs(&w.app.state.db, "audit_log").await;
    for action in ["person.checkin", "person.update"] {
        let entry = entries.iter().find(|e| e["action"] == action).unwrap();
        assert_eq!(entry["taken_over_from"], "volunteer-original", "{action}");
    }
}

// ---- invariants: what an ordinary write, a replay or a repeat cannot undo ----

async fn separated_alerts(app: &TestApp, person_id: &str) -> usize {
    crate::test_support::all_docs(&app.state.db, alerts::COLLECTION)
        .await
        .iter()
        .filter(|a| a["kind"] == "child_separated" && a["target_id"] == person_id)
        .count()
}

fn child_checkin(id: &str, data: Value) -> Value {
    checkin(json!({
        "id": id,
        "no_biometrics": true,
        "legal_basis": "child_protection",
        "data": data,
    }))
}

#[tokio::test]
async fn a_repeated_checkin_raises_the_separation_alert_once() {
    let w = world().await;
    let id = new_id();
    let path = format!("/v1/shelters/{}/checkins", w.shelter);
    for _ in 0..3 {
        let (status, body) = call(
            &w.app,
            &w.volunteer,
            "POST",
            &path,
            Some(child_checkin(&id, json!({ "child_status": "separated" }))),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    assert_eq!(separated_alerts(&w.app, &id).await, 1);
    assert_eq!(stored(&w.app, &id).await.stays.len(), 1);
}

#[tokio::test]
async fn replaying_a_checkin_cannot_lower_or_change_the_child_state() {
    let w = world().await;
    let id = new_id();
    let path = format!("/v1/shelters/{}/checkins", w.shelter);
    let first = child_checkin(&id, json!({ "child_status": "unaccompanied" }));
    let (status, _) = call(&w.app, &w.volunteer, "POST", &path, Some(first)).await;
    assert_eq!(status, StatusCode::CREATED);
    // protection settles the case; the old check-in is then replayed, and a worse one is tried
    w.app
        .grant(&w.admin, crate::volunteers::GlobalRole::Protection)
        .await;
    let (status, body) = call(
        &w.app,
        &w.admin,
        "PATCH",
        &format!("/v1/people/{id}"),
        Some(json!({ "data": { "child_status": "with_guardian" }, "client_edited_at": store::now().to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // none of them is refused: the stay is recorded and the child state stays as protection left it
    for data in [
        json!({ "child_status": "unaccompanied" }),
        json!({ "is_child": false }),
        // the same state, with an edit that is not about the child
        json!({ "child_status": "with_guardian", "name": "Ana" }),
    ] {
        let replay = child_checkin(&id, data.clone());
        let (status, body) = call(&w.app, &w.volunteer, "POST", &path, Some(replay)).await;
        assert_eq!(status, StatusCode::CREATED, "{data}: {body}");
        let person = stored(&w.app, &id).await;
        assert!(person.is_child, "{data}");
        assert_eq!(
            person.child_status,
            Some(ChildStatus::WithGuardian),
            "{data}"
        );
    }
    // the alert of the first check-in is the only one: no replay raised another
    assert_eq!(separated_alerts(&w.app, &id).await, 1);
}

#[tokio::test]
async fn an_older_edit_replayed_after_a_newer_one_changes_nothing() {
    let w = world().await;
    let id = register_without_biometrics(&w, "Ana").await;
    let path = format!("/v1/people/{id}");
    let edit = |status: &str, at: &str| {
        Some(json!({ "data": { "child_status": status }, "client_edited_at": at }))
    };
    w.app
        .grant(&w.admin, crate::volunteers::GlobalRole::Protection)
        .await;
    let newer = edit("with_guardian", "2026-10-21T12:00:00Z");
    let (status, _) = call(&w.app, &w.admin, "PATCH", &path, newer).await;
    assert_eq!(status, StatusCode::OK);
    // an older edit of the same author arrives late: ignored, no alert
    let older = edit("unaccompanied", "2026-10-21T11:00:00Z");
    let (status, _) = call(&w.app, &w.admin, "PATCH", &path, older).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        stored(&w.app, &id).await.child_status,
        Some(ChildStatus::WithGuardian)
    );
    assert_eq!(separated_alerts(&w.app, &id).await, 0);
}

#[tokio::test]
async fn a_late_biometrics_fusion_keeps_the_most_protected_child_state() {
    let w = world().await;
    let old = register_without_biometrics(&w, "Kid").await;
    // the old record is a separated child; the token's record is a child "with guardian"
    let mut doc = stored(&w.app, &old).await;
    doc.is_child = true;
    doc.child_status = Some(ChildStatus::Separated);
    store::replace(&w.app.state.db, COLLECTION, &old, &doc)
        .await
        .unwrap();
    let new = token_id(&w.app, "tok-kid");
    let mut existing = new_person(&new, Level::Assisted, LegalBasis::Consent, store::now());
    existing.is_child = true;
    existing.child_status = Some(ChildStatus::WithGuardian);
    store::replace(&w.app.state.db, COLLECTION, &new, &existing)
        .await
        .unwrap();
    let capture_id = capture(&w.app, &w.volunteer, "biometrics", "tok-kid", &old).await;
    let (status, body) = call(
        &w.app,
        &w.volunteer,
        "PUT",
        &format!("/v1/people/{old}/biometrics"),
        Some(json!({ "capture_id": capture_id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let merged = stored(&w.app, &new).await;
    assert_eq!(merged.child_status, Some(ChildStatus::Separated));
    assert!(merged.is_child);
    assert_eq!(separated_alerts(&w.app, &new).await, 1);
}

// ---- lists ----

#[test]
fn a_page_is_cut_at_the_limit_and_says_when_there_was_more() {
    let limit = 5;
    for (rows, kept, more) in [
        (limit - 1, limit - 1, false),
        (limit, limit, false),
        (limit + 1, limit, true),
    ] {
        let (page, has_more) = domain::page((0..rows).collect(), limit);
        assert_eq!((page.len(), has_more), (kept, more), "{rows} rows");
    }
}

/// Puts a person in the shelter, last updated at second `at_second` of a fixed minute.
async fn put_in_shelter(w: &World, level: Level, declined: bool, at_second: u32) -> String {
    let id = new_id();
    let mut person = new_person(&id, level, LegalBasis::Consent, store::now());
    person.biometrics_declined = declined;
    person.current_shelter_id = Some(w.shelter.clone());
    person.updated_at = at(&format!("2026-10-21T10:00:{at_second:02}Z"));
    store::replace(&w.app.state.db, COLLECTION, &id, &person)
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn the_people_list_is_one_page_and_has_more_is_exact_at_the_limit() {
    let w = world().await;
    for second in 0..4 {
        put_in_shelter(&w, Level::Verified, false, second).await;
    }
    let db = &w.app.state.db;
    // 4 people: limit - 1, limit and limit + 1 people
    for (limit, kept, more) in [(5, 4, false), (4, 4, false), (3, 3, true)] {
        let (page, has_more) = persist::people_of(db, &w.shelter, false, limit)
            .await
            .unwrap();
        assert_eq!((page.len(), has_more), (kept, more), "limit {limit}");
    }
    let (page, _) = persist::people_of(db, &w.shelter, false, 2).await.unwrap();
    let updated: Vec<String> = page.iter().map(|p| p.updated_at.to_string()).collect();
    assert_eq!(updated, ["2026-10-21T10:00:03Z", "2026-10-21T10:00:02Z"]);
    let path = format!("/v1/shelters/{}/people", w.shelter);
    let (status, body) = call(&w.app, &w.volunteer, "GET", &path, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["has_more"], false);
    assert_eq!(body["people"].as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn needs_selfie_is_filtered_by_the_database_before_the_page_is_cut() {
    let w = world().await;
    // the newest are verified: a page cut before the filter would be full of them
    for second in 40..45 {
        put_in_shelter(&w, Level::Verified, false, second).await;
    }
    let assisted_a = put_in_shelter(&w, Level::Assisted, false, 10).await;
    let assisted_b = put_in_shelter(&w, Level::Assisted, true, 11).await;
    put_in_shelter(&w, Level::NoBiometrics, true, 12).await;
    let waiting = put_in_shelter(&w, Level::NoBiometrics, false, 5).await;
    let db = &w.app.state.db;
    // 3 people need a selfie: limit + 1, limit and limit - 1
    for (limit, kept, more) in [(2, 2, true), (3, 3, false), (4, 3, false)] {
        let (page, has_more) = persist::people_of(db, &w.shelter, true, limit)
            .await
            .unwrap();
        assert_eq!((page.len(), has_more), (kept, more), "limit {limit}");
    }
    let path = format!("/v1/shelters/{}/people/needs-selfie", w.shelter);
    let (status, body) = call(&w.app, &w.volunteer, "GET", &path, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ids: Vec<&str> = body["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [assisted_b.as_str(), assisted_a.as_str(), waiting.as_str()]
    );
    assert_eq!(body["has_more"], false);
}

// ---- integrity ----

#[test]
fn a_stored_person_without_an_id_is_an_integrity_error_not_an_empty_id() {
    let mut person = new_person("p", Level::Assisted, LegalBasis::Consent, store::now());
    assert_eq!(person.stored_id().unwrap(), "p");
    person.id = None;
    let error = person.stored_id().unwrap_err();
    assert_eq!(
        (error.status, error.code),
        (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
    );
    let viewer = Volunteer {
        id: "v".into(),
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
    assert!(view::list_view(&[person], &viewer, "s", false).is_err());
}

#[test]
fn a_union_keeps_the_order_and_adds_each_item_once() {
    let mut into = vec!["b".to_string(), "a".to_string()];
    let from = ["a", "c", "c", "b", "d"].map(String::from).to_vec();
    domain::union(&mut into, from);
    assert_eq!(into, ["b", "a", "c", "d"]);
}

// ---- the size of a late-biometrics fusion ----

/// The record `Ana` with `references` things that point at her, split over the children who
/// list her as guardian, the deliveries and the rescues.
struct Fusion {
    w: World,
    old: String,
    capture_id: String,
    kids: Vec<String>,
    refs: Vec<(&'static str, String)>,
}

async fn fusion_with(references: usize) -> Fusion {
    let w = world().await;
    let old = register_without_biometrics(&w, "Ana").await;
    let db = &w.app.state.db;
    let share = |i: usize| references / 3 + usize::from(i < references % 3);
    let mut kids = Vec::new();
    for i in 0..share(0) {
        let id = format!("kid-{i}");
        let mut kid = new_person(&id, Level::NoBiometrics, LegalBasis::Consent, store::now());
        kid.guardians.push(Guardian {
            person_id: old.clone(),
            link: GuardianLink::Declared,
            checked_by: None,
            checked_at: None,
        });
        kid.sync_guardian_ids();
        store::replace(db, COLLECTION, &id, &kid).await.unwrap();
        kids.push(id);
    }
    let mut refs = Vec::new();
    for (n, collection) in ["deliveries", "rescues"].into_iter().enumerate() {
        for i in 0..share(n + 1) {
            let id = format!("{collection}-{i}");
            let doc = PersonRef {
                id: None,
                person_id: old.clone(),
            };
            store::replace(db, collection, &id, &doc).await.unwrap();
            refs.push((collection, id));
        }
    }
    let capture_id = capture(&w.app, &w.volunteer, "biometrics", "tok-ana", &old).await;
    Fusion {
        w,
        old,
        capture_id,
        kids,
        refs,
    }
}

impl Fusion {
    async fn run(&self) -> (StatusCode, Value) {
        let path = format!("/v1/people/{}/biometrics", self.old);
        let body = Some(json!({ "capture_id": self.capture_id }));
        call(&self.w.app, &self.w.volunteer, "PUT", &path, body).await
    }

    /// Where the references point now: `(at the old id, at the new id)`.
    async fn pointing(&self) -> (usize, usize) {
        let db = &self.w.app.state.db;
        let new = token_id(&self.w.app, "tok-ana");
        let (mut at_old, mut at_new) = (0, 0);
        for id in &self.kids {
            let kid = stored(&self.w.app, id).await;
            let target = &kid.guardians[0].person_id;
            at_old += usize::from(*target == self.old);
            at_new += usize::from(*target == new);
        }
        for (collection, id) in &self.refs {
            let doc: PersonRef = store::get(db, collection, id).await.unwrap().unwrap();
            at_old += usize::from(doc.person_id == self.old);
            at_new += usize::from(doc.person_id == new);
        }
        (at_old, at_new)
    }
}

#[tokio::test]
async fn a_fusion_with_hundreds_of_references_moves_every_one_in_a_single_transaction() {
    let limit = persist::MAX_MERGE_REFERENCES;
    let f = fusion_with(limit).await;
    let (status, body) = f.run().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.pointing().await, (0, limit));
    let db = &f.w.app.state.db;
    let gone = store::get::<PersonDoc>(db, COLLECTION, &f.old).await;
    assert!(gone.unwrap().is_none());
    let capture: CaptureDoc = store::get(db, CAPTURES, &f.capture_id)
        .await
        .unwrap()
        .unwrap();
    assert!(capture.consumed);
}

#[tokio::test]
async fn a_fusion_over_the_limit_is_refused_with_a_clear_error_and_writes_nothing() {
    let limit = persist::MAX_MERGE_REFERENCES;
    let f = fusion_with(limit + 1).await;
    let (status, body) = f.run().await;
    assert_eq!(
        (
            status,
            body["error"].as_str(),
            body["detail"]["limit"].as_u64()
        ),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("merge_too_large"),
            Some(limit as u64)
        ),
        "{body}"
    );
    // no partial state: every reference still points at the old record, which still exists,
    // nothing exists under the token and the capture is still usable
    assert_eq!(f.pointing().await, (limit + 1, 0));
    let db = &f.w.app.state.db;
    let old = store::get::<PersonDoc>(db, COLLECTION, &f.old).await;
    assert!(old.unwrap().is_some());
    let new = token_id(&f.w.app, "tok-ana");
    let created = store::get::<PersonDoc>(db, COLLECTION, &new).await;
    assert!(created.unwrap().is_none());
    let capture: CaptureDoc = store::get(db, CAPTURES, &f.capture_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!capture.consumed);
}
