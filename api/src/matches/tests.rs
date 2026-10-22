use super::domain::Decision;
use super::persist::name_of;
use super::*;
use crate::audit::LegalBasis;
use crate::matching::match_id;
use crate::people::{self, PersonDoc};
use crate::people::{Level, Needs, Origin, Vulnerabilities};
use crate::search_photos;
use crate::searches::Family;
use crate::searches::{self, SearchDoc};
use crate::shelters::ShelterDoc;
use crate::store::tx_replace;
use crate::test_support::{ORIGIN, TestApp, north_of_origin};
use crate::thumbnail;
use crate::volunteers::GlobalRole;
use crate::{shelters, store};
use axum::http::StatusCode;
use serde_json::{Value, json};

pub fn person(id: &str, shelter: Option<&str>, is_child: bool) -> PersonDoc {
    let now = store::now();
    PersonDoc {
        id: Some(id.into()),
        level: Level::Assisted,
        biometrics_declined: false,
        former_ids: vec![],
        name: Some("Maria Souza".into()),
        social_name: None,
        wristband: None,
        birth_year: None,
        is_child,
        child_status: None,
        guardians: vec![],
        guardian_ids: vec![],
        needs: Needs::default(),
        vulnerabilities: Vulnerabilities::default(),
        household_id: None,
        current_shelter_id: shelter.map(str::to_string),
        stays: vec![],
        event_ids: vec![],
        rescue_ids: vec![],
        thumbnail_encrypted: None,
        origin: Origin::Checkin,
        legal_basis: LegalBasis::VitalInterest,
        created_at: now,
        updated_at: now,
        client_edited_at: None,
    }
}

async fn shelter_doc(app: &TestApp, id: &str) -> ShelterDoc {
    store::get(&app.state.db, shelters::COLLECTION, id)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn the_queue_follows_the_child_flag_and_the_shelter_confirmation() {
    let app = TestApp::new().await;
    let admin = app.actor("Admin").await;
    let confirmed_id = app.confirmed_shelter(&admin, "Confirmed", ORIGIN).await;
    let confirmed = shelter_doc(&app, &confirmed_id).await;
    let other = app.actor("Other").await;
    let unconfirmed_id = app
        .create_shelter(&other, "Unconfirmed", north_of_origin(5_000.0))
        .await;
    let unconfirmed = shelter_doc(&app, &unconfirmed_id).await;

    let adult = person("p", Some(&confirmed_id), false);
    let child = person("c", Some(&confirmed_id), true);
    assert_eq!(
        queue_for(&adult, Some(&confirmed)),
        (MediatorKind::Shelter, Some(confirmed_id.clone()))
    );
    assert_eq!(
        queue_for(&adult, Some(&unconfirmed)),
        (MediatorKind::Central, None)
    );
    assert_eq!(queue_for(&adult, None), (MediatorKind::Central, None));
    // A child goes to protection in every situation.
    for shelter in [Some(&confirmed), Some(&unconfirmed), None] {
        assert_eq!(queue_for(&child, shelter), (MediatorKind::Protection, None));
    }
}

#[test]
fn the_match_id_is_the_hex_sha256_of_search_and_person() {
    assert_eq!(
        match_id("s1", "p1"),
        "c372e68eabd2d42d99bebd3d1912fe75e1bd41fff6f634e74d2d1c9b2ac96774"
    );
    assert_ne!(match_id("s1", "p1"), match_id("s1", "p2"));
}

// ---- routes ----

use crate::searches::{MissingPerson, SearchStatus, SearchVia};
use crate::test_support::{Actor, all_docs};
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use tower::ServiceExt;

const PHONE: &str = "+55 (51) 99999-0000";
const JPEG: [u8; 6] = [0xFF, 0xD8, 0xFF, 0xE0, 1, 2];

struct World {
    app: TestApp,
    admin_a: Actor,
    shelter_a: String,
    admin_b: Actor,
    admin_u: Actor,
    volunteer_a: Actor,
    mediator: Actor,
    protection: Actor,
    civil_defense: Actor,
    validator: Actor,
}

async fn world() -> World {
    let app = TestApp::new().await;
    let admin_a = app.actor("AdminA").await;
    let shelter_a = app.confirmed_shelter(&admin_a, "A", ORIGIN).await;
    let admin_b = app.actor("AdminB").await;
    app.confirmed_shelter(&admin_b, "B", north_of_origin(5_000.0))
        .await;
    let admin_u = app.actor("AdminU").await;
    app.create_shelter(&admin_u, "U", north_of_origin(10_000.0))
        .await;
    let volunteer_a = app
        .join(
            &app.invite(&admin_a, &shelter_a, "qr", "volunteer").await,
            "VolA",
        )
        .await;
    let mut roles = vec![];
    for (name, role) in [
        ("Med", GlobalRole::Mediator),
        ("Prot", GlobalRole::Protection),
        ("DC", GlobalRole::CivilDefense),
        ("Val", GlobalRole::Validator),
    ] {
        let actor = app.actor(name).await;
        app.grant(&actor, role).await;
        roles.push(actor);
    }
    let validator = roles.pop().unwrap();
    let civil_defense = roles.pop().unwrap();
    let protection = roles.pop().unwrap();
    let mediator = roles.pop().unwrap();
    World {
        app,
        admin_a,
        shelter_a,
        admin_b,
        admin_u,
        volunteer_a,
        mediator,
        protection,
        civil_defense,
        validator,
    }
}

/// A person, a search with the family's data and the open match between them.
async fn seed(
    app: &TestApp,
    kind: MediatorKind,
    shelter: Option<&str>,
    is_child: bool,
    language: &str,
) -> String {
    let person_id = store::now().as_microsecond().to_string() + &uuid::Uuid::new_v4().to_string();
    let mut p = person(&person_id, shelter, is_child);
    p.thumbnail_encrypted = Some(thumbnail::encrypt(app.state.thumbnail_key(), &JPEG).unwrap());
    store::replace(&app.state.db, people::COLLECTION, &person_id, &p)
        .await
        .unwrap();
    let search_id = uuid::Uuid::new_v4().to_string();
    let search = SearchDoc {
        id: None,
        created_by: "someone".into(),
        via: SearchVia::Volunteer,
        origin_shelter_id: None,
        event_id: None,
        person_ids: vec![person_id.clone()],
        photos_sent: 1,
        usable_photos: 1,
        retained_photos: 0,
        missing_person: MissingPerson {
            name: "Maria de Souza".into(),
            nickname: Some("Mari".into()),
            notes: "scar on the left hand".into(),
        },
        family: Family {
            name: "Joana".into(),
            phone: PHONE.into(),
            relationship: "sister".into(),
            language: language.into(),
        },
        status: SearchStatus::Active,
        expiry_notified_at: None,
        created_at: store::now(),
    };
    store::replace(&app.state.db, searches::COLLECTION, &search_id, &search)
        .await
        .unwrap();
    let id = match_id(&search_id, &person_id);
    let m = MatchDoc {
        id: None,
        search_id,
        person_id,
        is_child,
        mediator_kind: kind,
        mediator_shelter_id: shelter
            .filter(|_| kind == MediatorKind::Shelter)
            .map(str::to_string),
        status: MatchStatus::Open,
        decided_by: None,
        decided_by_volunteer: None,
        decided_at: None,
        note: None,
        created_at: store::now(),
    };
    store::replace(&app.state.db, COLLECTION, &id, &m)
        .await
        .unwrap();
    id
}

async fn listed(app: &TestApp, who: &Actor) -> Vec<String> {
    let (status, body) = app.send("GET", "/v1/matches", &who.secret, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect()
}

async fn status_of(app: &TestApp, method: &str, path: &str, who: &Actor) -> StatusCode {
    app.send(method, path, &who.secret, None).await.0
}

async fn decide(app: &TestApp, who: &Actor, id: &str, decision: &str) -> (StatusCode, Value) {
    app.send(
        "POST",
        &format!("/v1/matches/{id}/decision"),
        &who.secret,
        Some(json!({ "decision": decision, "decided_by": "person", "note": " ok " })),
    )
    .await
}

async fn stored(app: &TestApp, id: &str) -> MatchDoc {
    store::get(&app.state.db, COLLECTION, id)
        .await
        .unwrap()
        .unwrap()
}

async fn audit_of(app: &TestApp, action: &str) -> Vec<Value> {
    all_docs(&app.state.db, "audit_log")
        .await
        .into_iter()
        .filter(|e| e["action"] == action)
        .collect()
}

async fn raw_get(app: &TestApp, path: &str, who: &Actor) -> (StatusCode, String, Vec<u8>) {
    let request = Request::builder()
        .uri(path)
        .header("authorization", format!("Bearer {}", who.secret))
        .body(Body::empty())
        .unwrap();
    let response = crate::router(app.state.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let cache = response
        .headers()
        .get("cache-control")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, cache, bytes.to_vec())
}

#[tokio::test]
async fn each_role_lists_only_the_queues_it_attends() {
    let w = world().await;
    let shelter_match = seed(
        &w.app,
        MediatorKind::Shelter,
        Some(&w.shelter_a),
        false,
        "en",
    )
    .await;
    let central = seed(&w.app, MediatorKind::Central, None, false, "en").await;
    let child = seed(
        &w.app,
        MediatorKind::Protection,
        Some(&w.shelter_a),
        true,
        "en",
    )
    .await;
    let sorted = |mut ids: Vec<String>| {
        ids.sort();
        ids
    };
    let all = sorted(vec![shelter_match.clone(), central.clone(), child.clone()]);
    // The admin of the shelter: its own queue only; never the child's match (criterion 46).
    assert_eq!(listed(&w.app, &w.admin_a).await, [shelter_match.clone()]);
    // Mediator: central and every shelter queue; no protection.
    assert_eq!(
        sorted(listed(&w.app, &w.mediator).await),
        sorted(vec![shelter_match.clone(), central.clone()])
    );
    assert_eq!(listed(&w.app, &w.protection).await, [child.clone()]);
    assert_eq!(sorted(listed(&w.app, &w.civil_defense).await), all);
    // Nobody else has a queue: another shelter's admin, an unconfirmed shelter's admin, a
    // volunteer, a validator alone, someone with no shelter.
    let stranger = w.app.actor("Ze").await;
    for who in [
        &w.admin_b,
        &w.admin_u,
        &w.volunteer_a,
        &w.validator,
        &stranger,
    ] {
        let expect = if who.id == w.admin_b.id {
            StatusCode::OK
        } else {
            StatusCode::FORBIDDEN
        };
        assert_eq!(
            status_of(&w.app, "GET", "/v1/matches", who).await,
            expect,
            "{}",
            who.id
        );
    }
    assert!(listed(&w.app, &w.admin_b).await.is_empty());
    assert_eq!(
        w.app.request("GET", "/v1/matches", &[], None).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_child_match_is_closed_to_the_shelter_admins_in_every_route() {
    let w = world().await;
    let child = seed(
        &w.app,
        MediatorKind::Protection,
        Some(&w.shelter_a),
        true,
        "en",
    )
    .await;
    for who in [&w.admin_a, &w.volunteer_a, &w.mediator, &w.validator] {
        for (method, path) in [
            ("GET", format!("/v1/matches/{child}")),
            ("GET", format!("/v1/matches/{child}/thumbnail")),
            ("POST", format!("/v1/matches/{child}/decision")),
        ] {
            let body =
                (method == "POST").then(|| json!({ "decision": "accept", "decided_by": "person" }));
            let (status, answer) = w.app.send(method, &path, &who.secret, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{} {path}", who.id);
            assert_eq!(answer["error"], "forbidden");
        }
    }
    assert_eq!(stored(&w.app, &child).await.status, MatchStatus::Open);
    for who in [&w.protection, &w.civil_defense] {
        assert_eq!(
            status_of(&w.app, "GET", &format!("/v1/matches/{child}"), who).await,
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn open_matches_come_first_and_the_list_has_no_contact() {
    let w = world().await;
    let first = seed(
        &w.app,
        MediatorKind::Shelter,
        Some(&w.shelter_a),
        false,
        "en",
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let second = seed(
        &w.app,
        MediatorKind::Shelter,
        Some(&w.shelter_a),
        false,
        "en",
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let third = seed(
        &w.app,
        MediatorKind::Shelter,
        Some(&w.shelter_a),
        false,
        "en",
    )
    .await;
    assert_eq!(
        decide(&w.app, &w.admin_a, &third, "decline").await.0,
        StatusCode::OK
    );
    let (_, body) = w
        .app
        .send("GET", "/v1/matches", &w.admin_a.secret, None)
        .await;
    let ids: Vec<&str> = body["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [second.as_str(), first.as_str(), third.as_str()]);
    let item = &body["matches"][0];
    assert_eq!(
        (
            &item["status"],
            &item["mediator_kind"],
            &item["is_child"],
            &item["missing_person_name"],
            &item["shelter_name"]
        ),
        (
            &json!("open"),
            &json!("shelter"),
            &json!(false),
            &json!("Maria de Souza"),
            &json!("A")
        )
    );
    assert_eq!(body["matches"][2]["status"], "declined");
    assert!(!body.to_string().contains("99999"));
}

async fn listing(app: &TestApp, who: &Actor) -> (usize, Value) {
    let (status, body) = app.send("GET", "/v1/matches", &who.secret, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (
        body["matches"].as_array().unwrap().len(),
        body["has_more"].clone(),
    )
}

#[tokio::test]
async fn the_listing_says_when_a_queue_holds_more_than_it_shows() {
    let w = world().await;
    let limit = LIST_LIMIT as usize;
    for _ in 1..limit {
        seed(&w.app, MediatorKind::Central, None, false, "en").await;
    }
    assert_eq!(
        listing(&w.app, &w.mediator).await,
        (limit - 1, json!(false))
    );
    seed(&w.app, MediatorKind::Central, None, false, "en").await;
    assert_eq!(listing(&w.app, &w.mediator).await, (limit, json!(false)));
    seed(&w.app, MediatorKind::Central, None, false, "en").await;
    assert_eq!(listing(&w.app, &w.mediator).await, (limit, json!(true)));
}

#[tokio::test]
async fn a_match_whose_search_was_purged_is_not_listed() {
    let w = world().await;
    let kept = seed(&w.app, MediatorKind::Central, None, false, "en").await;
    let mut orphan = stored(&w.app, &kept).await;
    orphan.search_id = "purged".into();
    store::replace(&w.app.state.db, COLLECTION, "orphan", &orphan)
        .await
        .unwrap();
    assert_eq!(listed(&w.app, &w.mediator).await, [kept]);
}

#[tokio::test]
async fn the_detail_has_the_check_list_and_never_the_family_contact() {
    let w = world().await;
    let id = seed(
        &w.app,
        MediatorKind::Shelter,
        Some(&w.shelter_a),
        false,
        "es",
    )
    .await;
    let (status, body) = w
        .app
        .send("GET", &format!("/v1/matches/{id}"), &w.admin_a.secret, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({
            "id": id,
            "status": "open",
            "mediator_kind": "shelter",
            "is_child": false,
            "missing_person": {
                "name": "Maria de Souza", "nickname": "Mari", "notes": "scar on the left hand"
            },
            "family": { "name": "Joana", "relationship": "sister", "language": "es" },
            "person": {
                "name": "Maria Souza", "social_name": null, "wristband": null,
                "has_thumbnail": true
            },
            "search_photos": 0,
            "shelter_name": "A",
            "created_at": body["created_at"],
        })
    );
    assert!(!body.to_string().contains("99999"));
    // the admin does not get the person's history; the view is audited
    assert!(body.get("history").is_none());
    assert_eq!(audit_of(&w.app, "match.view").await.len(), 1);
    assert_eq!(
        status_of(&w.app, "GET", "/v1/matches/nope", &w.admin_a).await,
        StatusCode::NOT_FOUND
    );
    for who in [&w.admin_b, &w.admin_u, &w.volunteer_a, &w.protection] {
        assert_eq!(
            status_of(&w.app, "GET", &format!("/v1/matches/{id}"), who).await,
            StatusCode::FORBIDDEN,
            "{}",
            who.id
        );
    }
}

#[tokio::test]
async fn the_history_goes_to_who_attends_and_only_while_the_match_is_open() {
    let w = world().await;
    let id = seed(
        &w.app,
        MediatorKind::Shelter,
        Some(&w.shelter_a),
        false,
        "en",
    )
    .await;
    let path = format!("/v1/matches/{id}");
    for who in [&w.mediator, &w.civil_defense] {
        let (_, body) = w.app.send("GET", &path, &who.secret, None).await;
        assert!(body["history"]["stays"].is_array(), "{}", who.id);
    }
    assert_eq!(
        decide(&w.app, &w.admin_a, &id, "decline").await.0,
        StatusCode::OK
    );
    let (_, body) = w.app.send("GET", &path, &w.mediator.secret, None).await;
    assert!(body.get("history").is_none());
    assert_eq!(body["status"], "declined");
}

/// Keeps `photos` for the search of the match, as registering a search with a match does.
async fn keep_photos(app: &TestApp, match_id: &str, photos: &[Vec<u8>]) -> String {
    let search_id = stored(app, match_id).await.search_id;
    let key = app.state.thumbnail_key().to_vec();
    store::transact(&app.state.db, |db, tx| {
        let (search_id, key, photos) = (search_id.clone(), key.clone(), photos.to_vec());
        Box::pin(async move {
            let mut search: SearchDoc = store::get(&db, searches::COLLECTION, &search_id)
                .await?
                .unwrap();
            search.retained_photos = search_photos::write(&db, tx, &key, &search_id, &photos)?;
            tx_replace(&db, tx, searches::COLLECTION, &search_id, &search)
        })
    })
    .await
    .unwrap();
    search_id
}

#[tokio::test]
async fn the_family_photos_go_to_who_attends_while_open_and_every_view_is_audited() {
    let w = world().await;
    let id = seed(
        &w.app,
        MediatorKind::Shelter,
        Some(&w.shelter_a),
        false,
        "en",
    )
    .await;
    let png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1];
    let search_id = keep_photos(&w.app, &id, &[JPEG.to_vec(), png.clone()]).await;
    // Encrypted at rest.
    let stored_photo: search_photos::PhotoDoc = store::get(
        &w.app.state.db,
        search_photos::COLLECTION,
        &search_photos::photo_id(&search_id, 0),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !stored_photo
            .photo_encrypted
            .unwrap()
            .windows(JPEG.len())
            .any(|x| x == JPEG)
    );

    let (_, view) = w
        .app
        .send("GET", &format!("/v1/matches/{id}"), &w.admin_a.secret, None)
        .await;
    assert_eq!(view["search_photos"], 2);
    assert!(
        !view
            .to_string()
            .contains(&stored(&w.app, &id).await.person_id)
    );

    let path = |n: u32| format!("/v1/matches/{id}/search-photos/{n}");
    let (status, cache, bytes) = raw_get(&w.app, &path(0), &w.admin_a).await;
    assert_eq!(
        (status, cache.as_str(), bytes),
        (StatusCode::OK, "no-store", JPEG.to_vec())
    );
    let (status, _, bytes) = raw_get(&w.app, &path(1), &w.mediator).await;
    assert_eq!((status, bytes), (StatusCode::OK, png));
    assert_eq!(
        raw_get(&w.app, &path(2), &w.admin_a).await.0,
        StatusCode::NOT_FOUND
    );
    for who in [&w.admin_b, &w.volunteer_a, &w.protection] {
        assert_eq!(
            raw_get(&w.app, &path(0), who).await.0,
            StatusCode::FORBIDDEN,
            "{}",
            who.id
        );
    }
    let views = audit_of(&w.app, "search_photo.view").await;
    assert_eq!(views.len(), 2);
    assert!(
        views
            .iter()
            .all(|e| e["legal_basis"] == "vital_interest" && e["shelter_id"] == json!(w.shelter_a))
    );
}

#[tokio::test]
async fn accepting_the_last_open_match_erases_the_photos_and_a_decline_keeps_them() {
    for (decision, erased) in [
        ("accept", true),
        ("decline", false),
        ("not_the_person", false),
    ] {
        let w = world().await;
        let id = seed(
            &w.app,
            MediatorKind::Shelter,
            Some(&w.shelter_a),
            false,
            "en",
        )
        .await;
        let search_id = keep_photos(&w.app, &id, &[JPEG.to_vec(), JPEG.to_vec()]).await;
        assert_eq!(
            decide(&w.app, &w.admin_a, &id, decision).await.0,
            StatusCode::OK
        );
        let left = search_photos::all(&w.app.state.db).await.len();
        assert_eq!(left, if erased { 0 } else { 2 }, "{decision}");
        let search: SearchDoc = store::get(&w.app.state.db, searches::COLLECTION, &search_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(search.retained_photos, if erased { 0 } else { 2 });
        // Nobody sees them once the match is decided, kept or not.
        assert_eq!(
            raw_get(
                &w.app,
                &format!("/v1/matches/{id}/search-photos/0"),
                &w.admin_a
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            audit_of(&w.app, "search_photos.discard.found").await.len(),
            usize::from(erased)
        );
    }
}

#[tokio::test]
async fn the_photos_stay_until_a_match_is_accepted_and_none_is_open() {
    let w = world().await;
    let first = seed(
        &w.app,
        MediatorKind::Shelter,
        Some(&w.shelter_a),
        false,
        "en",
    )
    .await;
    keep_photos(&w.app, &first, &[JPEG.to_vec()]).await;
    let mut second = stored(&w.app, &first).await;
    second.person_id = "another-person".into();
    store::replace(&w.app.state.db, COLLECTION, "second-match", &second)
        .await
        .unwrap();

    // Accepted, but another match is still open.
    assert_eq!(
        decide(&w.app, &w.admin_a, &first, "accept").await.0,
        StatusCode::OK
    );
    assert_eq!(search_photos::all(&w.app.state.db).await.len(), 1);
    // The last open one is declined: someone was found, the photos go.
    assert_eq!(
        decide(&w.app, &w.admin_a, "second-match", "decline")
            .await
            .0,
        StatusCode::OK
    );
    assert!(search_photos::all(&w.app.state.db).await.is_empty());
}

#[tokio::test]
async fn the_thumbnail_goes_to_who_attends_and_every_view_is_audited() {
    let w = world().await;
    let id = seed(
        &w.app,
        MediatorKind::Shelter,
        Some(&w.shelter_a),
        false,
        "en",
    )
    .await;
    let path = format!("/v1/matches/{id}/thumbnail");
    let (status, cache, bytes) = raw_get(&w.app, &path, &w.admin_a).await;
    assert_eq!(
        (status, cache.as_str(), bytes),
        (StatusCode::OK, "no-store", JPEG.to_vec())
    );
    assert_eq!(raw_get(&w.app, &path, &w.mediator).await.0, StatusCode::OK);
    for who in [&w.admin_b, &w.volunteer_a, &w.protection] {
        assert_eq!(
            raw_get(&w.app, &path, who).await.0,
            StatusCode::FORBIDDEN,
            "{}",
            who.id
        );
    }
    assert_eq!(
        raw_get(&w.app, "/v1/matches/nope/thumbnail", &w.admin_a)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let views = audit_of(&w.app, "thumbnail.view").await;
    assert_eq!(views.len(), 2);
    assert!(
        views
            .iter()
            .all(|e| e["legal_basis"] == "vital_interest" && e["shelter_id"] == json!(w.shelter_a))
    );
    // no thumbnail on the person: 404 thumbnail_not_found, nothing audited
    let bare = seed(&w.app, MediatorKind::Central, None, false, "en").await;
    let mut person_doc: PersonDoc = store::get(
        &w.app.state.db,
        people::COLLECTION,
        &stored(&w.app, &bare).await.person_id,
    )
    .await
    .unwrap()
    .unwrap();
    person_doc.thumbnail_encrypted = None;
    store::replace(
        &w.app.state.db,
        people::COLLECTION,
        &stored(&w.app, &bare).await.person_id,
        &person_doc,
    )
    .await
    .unwrap();
    let (status, answer) = w
        .app
        .send(
            "GET",
            &format!("/v1/matches/{bare}/thumbnail"),
            &w.mediator.secret,
            None,
        )
        .await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("thumbnail_not_found"))
    );
    assert_eq!(audit_of(&w.app, "thumbnail.view").await.len(), 2);
    // a child's thumbnail: only protection and the Civil Defense (criterion 46)
    let child = seed(
        &w.app,
        MediatorKind::Protection,
        Some(&w.shelter_a),
        true,
        "en",
    )
    .await;
    let child_path = format!("/v1/matches/{child}/thumbnail");
    assert_eq!(
        raw_get(&w.app, &child_path, &w.admin_a).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        raw_get(&w.app, &child_path, &w.protection).await.0,
        StatusCode::OK
    );
    assert_eq!(
        raw_get(&w.app, &child_path, &w.civil_defense).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn accept_gives_the_contact_in_the_family_language_and_closes_the_match() {
    let w = world().await;
    for (language, starts) in [
        ("pt-BR", "Ol%C3%A1%21"),
        ("en", "Hello%21"),
        ("es", "%C2%A1Hola%21"),
        ("fr", "Ol%C3%A1%21"),
    ] {
        let id = seed(
            &w.app,
            MediatorKind::Shelter,
            Some(&w.shelter_a),
            false,
            language,
        )
        .await;
        let (status, body) = decide(&w.app, &w.admin_a, &id, "accept").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["family"],
            json!({ "name": "Joana", "phone": PHONE, "relationship": "sister", "language": language })
        );
        let link = body["whatsapp_link"].as_str().unwrap();
        assert!(
            link.starts_with(&format!("https://wa.me/5551999990000?text={starts}")),
            "{language}: {link}"
        );
        assert!(!link.contains(' ') && !link.contains('+'));
        let m = stored(&w.app, &id).await;
        assert_eq!(
            (
                m.status,
                m.decided_by,
                m.decided_by_volunteer.as_deref(),
                m.note.as_deref()
            ),
            (
                MatchStatus::Accepted,
                Some(DecidedBy::Person),
                Some(w.admin_a.id.as_str()),
                Some("ok")
            )
        );
        assert!(m.decided_at.is_some());
        // the contact is only in that answer; the detail has none, and a second decision is a 409
        let (_, detail) = w
            .app
            .send("GET", &format!("/v1/matches/{id}"), &w.admin_a.secret, None)
            .await;
        assert!(!detail.to_string().contains("99999"));
        for who in [&w.admin_a, &w.mediator] {
            let (status, body) = decide(&w.app, who, &id, "accept").await;
            assert_eq!(
                (status, body["error"].as_str()),
                (StatusCode::CONFLICT, Some("match_already_decided"))
            );
            assert!(body.get("family").is_none());
        }
    }
    let accepts = audit_of(&w.app, "match.accept").await;
    assert_eq!(accepts.len(), 4);
    assert!(accepts.iter().all(|e| e["legal_basis"] == "vital_interest"
        && e["target_kind"] == "match"
        && e["volunteer_id"] == json!(w.admin_a.id)));
    // the audit carries ids only, never the contact
    assert!(
        !json!(all_docs(&w.app.state.db, "audit_log").await)
            .to_string()
            .contains("99999")
    );
}

#[tokio::test]
async fn decline_and_not_the_person_send_nothing_and_keep_the_search_active() {
    let w = world().await;
    for (decision, status) in [
        ("decline", MatchStatus::Declined),
        ("not_the_person", MatchStatus::NotThePerson),
    ] {
        let id = seed(&w.app, MediatorKind::Central, None, false, "en").await;
        let (code, body) = w
            .app
            .send(
                "POST",
                &format!("/v1/matches/{id}/decision"),
                &w.mediator.secret,
                Some(json!({ "decision": decision, "decided_by": "operator" })),
            )
            .await;
        assert_eq!((code, &body), (StatusCode::OK, &json!({})), "{decision}");
        let m = stored(&w.app, &id).await;
        assert_eq!(
            (m.status, m.decided_by),
            (status, Some(DecidedBy::Operator))
        );
        // the match stays, closed: the idempotent creation never opens another for the pair
        assert_eq!(
            m.id.as_deref(),
            Some(match_id(&m.search_id, &m.person_id).as_str())
        );
        let search: SearchDoc = store::get(&w.app.state.db, searches::COLLECTION, &m.search_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(search.status, SearchStatus::Active);
        assert_eq!(
            audit_of(&w.app, &format!("match.{decision}")).await.len(),
            1
        );
    }
    // `not_the_person` is counted apart from the refusals (criterion 50)
    let docs = all_docs(&w.app.state.db, COLLECTION).await;
    let count = |s: &str| docs.iter().filter(|d| d["status"] == s).count();
    assert_eq!((count("not_the_person"), count("declined")), (1, 1));
}

#[tokio::test]
async fn only_who_attends_decides_and_a_bad_body_is_refused() {
    let w = world().await;
    let id = seed(
        &w.app,
        MediatorKind::Shelter,
        Some(&w.shelter_a),
        false,
        "en",
    )
    .await;
    let path = format!("/v1/matches/{id}/decision");
    for who in [
        &w.admin_b,
        &w.admin_u,
        &w.volunteer_a,
        &w.protection,
        &w.validator,
    ] {
        assert_eq!(
            decide(&w.app, who, &id, "accept").await.0,
            StatusCode::FORBIDDEN,
            "{}",
            who.id
        );
    }
    assert_eq!(
        decide(&w.app, &w.admin_a, "nope", "accept").await.0,
        StatusCode::NOT_FOUND
    );
    for body in [
        json!({}),
        json!({ "decision": "maybe", "decided_by": "person" }),
        json!({ "decision": "accept" }),
        json!({ "decision": "accept", "decided_by": "robot" }),
        json!({ "decision": "accept", "decided_by": "person", "extra": 1 }),
        json!({ "decision": "accept", "decided_by": "person", "note": "x".repeat(501) }),
    ] {
        let (status, answer) = w
            .app
            .send("POST", &path, &w.admin_a.secret, Some(body.clone()))
            .await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_request")),
            "{body}"
        );
    }
    assert_eq!(
        w.app
            .request(
                "POST",
                &path,
                &[],
                Some(json!({ "decision": "accept", "decided_by": "person" }))
            )
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(stored(&w.app, &id).await.status, MatchStatus::Open);
    assert!(audit_of(&w.app, "match.accept").await.is_empty());
}

#[tokio::test]
async fn concurrent_decisions_have_exactly_one_winner() {
    let w = world().await;
    let id = seed(
        &w.app,
        MediatorKind::Shelter,
        Some(&w.shelter_a),
        false,
        "en",
    )
    .await;
    let deciders = [
        (&w.admin_a, "accept"),
        (&w.mediator, "decline"),
        (&w.civil_defense, "not_the_person"),
        (&w.admin_a, "decline"),
        (&w.mediator, "accept"),
        (&w.civil_defense, "accept"),
    ];
    let tasks: Vec<_> = deciders
        .iter()
        .map(|(who, decision)| {
            let (app, who, id, decision) = (w.app.clone(), (*who).clone(), id.clone(), *decision);
            tokio::spawn(async move {
                let (status, body) = decide(&app, &who, &id, decision).await;
                (who.id, decision, status, body)
            })
        })
        .collect();
    let mut winners = vec![];
    for task in tasks {
        let (volunteer, decision, status, body) = task.await.unwrap();
        match status {
            StatusCode::OK => winners.push((volunteer, decision, body)),
            StatusCode::CONFLICT => assert_eq!(body["error"], "match_already_decided"),
            other => panic!("unexpected {other}: {body}"),
        }
    }
    assert_eq!(winners.len(), 1);
    let (volunteer, decision, body) = &winners[0];
    let m = stored(&w.app, &id).await;
    assert_eq!(m.decided_by_volunteer.as_deref(), Some(volunteer.as_str()));
    assert_eq!(
        name_of(m.status),
        name_of(Decision::status(match *decision {
            "accept" => Decision::Accept,
            "decline" => Decision::Decline,
            _ => Decision::NotThePerson,
        }))
    );
    // only the winner's accept carries the contact
    assert_eq!(body.get("family").is_some(), *decision == "accept");
    let decisions = all_docs(&w.app.state.db, "audit_log")
        .await
        .into_iter()
        .filter(|e| {
            e["action"].as_str().unwrap().starts_with("match.") && e["action"] != "match.view"
        })
        .count();
    assert_eq!(decisions, 1);
}

/// The rules of `domain`, tested without Firestore.
mod domain_rules {
    use crate::auth::Membership;
    use crate::auth::Volunteer;
    use crate::matches::domain::*;
    use crate::matches::*;
    use crate::members::ShelterRole;
    use crate::searches::Family;
    use crate::shelters::Confirmation;
    use crate::store;
    use crate::volunteers::GlobalRole;
    use crate::volunteers::VolunteerDoc;

    fn volunteer(roles: &[GlobalRole], admin_of: &[(&str, Confirmation)]) -> Volunteer {
        let now = store::now();
        Volunteer {
            id: "v".into(),
            doc: VolunteerDoc {
                name: "V".into(),
                phone: None,
                show_phone: false,
                language: "pt-BR".into(),
                global_roles: roles.to_vec(),
                role_grants: vec![],
                banned: false,
                banned_by: None,
                banned_at: None,
                flagged: false,
                flag_reasons: vec![],
                created_at: now,
                last_seen_at: now,
            },
            memberships: admin_of
                .iter()
                .map(|(shelter, confirmation)| Membership {
                    shelter_id: shelter.to_string(),
                    name: String::new(),
                    role: ShelterRole::Admin,
                    confirmation: *confirmation,
                    event_id: None,
                })
                .collect(),
            session_id: "s".into(),
        }
    }

    fn open_match(kind: MediatorKind, shelter: Option<&str>) -> MatchDoc {
        MatchDoc {
            id: None,
            search_id: "s".into(),
            person_id: "p".into(),
            is_child: kind == MediatorKind::Protection,
            mediator_kind: kind,
            mediator_shelter_id: shelter.map(str::to_string),
            status: MatchStatus::Open,
            decided_by: None,
            decided_by_volunteer: None,
            decided_at: None,
            note: None,
            created_at: store::now(),
        }
    }

    #[test]
    fn a_page_read_with_one_extra_row_is_cut_and_flagged_only_past_the_limit() {
        for (rows, kept, more) in [(99, 99, false), (100, 100, false), (101, 100, true)] {
            let (page, flag) = cap((0..rows).collect::<Vec<_>>(), 100);
            assert_eq!((page.len(), flag), (kept, more), "{rows} rows");
        }
        // The first rows are the ones kept: the page is newest first.
        assert_eq!(cap(vec![1, 2, 3], 2), (vec![1, 2], true));
    }

    /// Every queue a volunteer is sent to by `queues_of` is one they attend, and a match they
    /// attend lives in one of those queues: nothing attended is out of reach of the listing.
    #[test]
    fn the_queues_listed_are_exactly_the_ones_attended() {
        let volunteers = [
            volunteer(&[GlobalRole::CivilDefense], &[]),
            volunteer(&[GlobalRole::Mediator], &[]),
            volunteer(&[GlobalRole::Protection], &[]),
            volunteer(&[GlobalRole::Validator], &[]),
            volunteer(&[], &[("a", Confirmation::Confirmed)]),
            volunteer(&[], &[("a", Confirmation::Unconfirmed)]),
            volunteer(&[], &[]),
        ];
        let matches = [
            open_match(MediatorKind::Protection, None),
            open_match(MediatorKind::Central, None),
            open_match(MediatorKind::Shelter, Some("a")),
            open_match(MediatorKind::Shelter, Some("b")),
        ];
        for v in &volunteers {
            let queues = queues_of(v);
            for m in &matches {
                let reached = queues.iter().any(|(kind, shelter)| {
                    *kind == m.mediator_kind
                        && shelter
                            .as_deref()
                            .is_none_or(|s| m.mediator_shelter_id.as_deref() == Some(s))
                });
                assert_eq!(attends(v, m), reached, "{:?} {:?}", v.memberships, m);
            }
        }
    }

    #[test]
    fn only_who_attends_decides_and_only_once() {
        let admin = volunteer(&[], &[("a", Confirmation::Confirmed)]);
        let other = volunteer(&[], &[("b", Confirmation::Confirmed)]);
        let m = open_match(MediatorKind::Shelter, Some("a"));
        let at = store::now();
        let decide_as = |who: &Volunteer, m: MatchDoc| {
            decide(who, m, Decision::Decline, DecidedBy::Person, None, at)
        };
        assert_eq!(decide_as(&other, m.clone()).unwrap_err().code, "forbidden");
        let decided = decide_as(&admin, m).unwrap();
        assert_eq!(
            (
                decided.status,
                decided.decided_by,
                decided.decided_by_volunteer.as_deref()
            ),
            (MatchStatus::Declined, Some(DecidedBy::Person), Some("v"))
        );
        // Deciding it again, whatever the decision, changes nothing and says so.
        for decision in [Decision::Accept, Decision::Decline, Decision::NotThePerson] {
            let again = decide(
                &admin,
                decided.clone(),
                decision,
                DecidedBy::Operator,
                None,
                at,
            );
            assert_eq!(again.unwrap_err().code, "match_already_decided");
        }
    }

    #[test]
    fn a_note_is_trimmed_and_bounded() {
        assert_eq!(clean_note(None).unwrap(), None);
        assert_eq!(clean_note(Some("  ".into())).unwrap(), None);
        assert_eq!(
            clean_note(Some(" ok ".into())).unwrap().as_deref(),
            Some("ok")
        );
        assert!(clean_note(Some("x".repeat(MAX_NOTE_CHARS))).is_ok());
        assert_eq!(
            clean_note(Some("x".repeat(MAX_NOTE_CHARS + 1)))
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }

    #[test]
    fn the_whatsapp_link_keeps_only_the_digits_and_encodes_the_text() {
        let family = |phone: &str, language: &str| Family {
            name: "J".into(),
            phone: phone.into(),
            relationship: "x".into(),
            language: language.into(),
        };
        let link = whatsapp_link(&family("+55 (51) 99999-0000", "en"));
        let (head, text) = link.split_once("?text=").unwrap();
        assert_eq!(head, "https://wa.me/5551999990000");
        assert!(text.starts_with("Hello%21%20"));
        assert!(
            text.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"%-._~".contains(&b))
        );
        assert_ne!(whatsapp_message("pt-BR"), whatsapp_message("en"));
        assert_ne!(whatsapp_message("es"), whatsapp_message("en"));
        // no place, no shelter, no name in the first message
        for language in ["pt-BR", "en", "es"] {
            let text = whatsapp_message(language).to_lowercase();
            assert!(
                !text.contains("abrigo") && !text.contains("shelter") && !text.contains("refugio")
            );
        }
    }
}
