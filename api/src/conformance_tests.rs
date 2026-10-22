//! The final conformance of phase 6: what must hold of the whole API, not of one module.
//! Criterion 13 (nothing of spec 16 exists), 16 (an `unconfirmed` shelter), 59 (no raw Unico
//! token and no person photo in any response or document) and the audit review of spec 4.10:
//! every write route, and every read of a thumbnail or of match data, leaves an `audit_log`
//! entry with its legal basis. Nothing here calls the Unico: the fake answers.

use std::sync::Mutex;

use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use crate::alerts::{self, AlertDoc, AlertKind};
use crate::events::{self, EventDoc};
use crate::shelters::{self, ShelterDoc};
use crate::store;
use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, new_id, north_of_origin};
use crate::volunteers::GlobalRole;
use crate::{degraded, jobs};

const MAIN: &str = include_str!("main.rs");
const METHODS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

/// `(METHOD, path)` of every route `main.rs` registers in `router`.
fn registered_routes() -> Vec<(String, String)> {
    let start = MAIN.find("fn router(").expect("router in main.rs");
    let end = MAIN[start..].find(".fallback(").expect("fallback") + start;
    let mut routes = vec![];
    for piece in MAIN[start..end].split(".route(").skip(1) {
        let mut quoted = piece.split('"');
        let path = quoted.nth(1).expect("a path").to_string();
        for method in METHODS {
            let call = format!("{method}(");
            let found = piece
                .match_indices(&call)
                .any(|(at, _)| !piece[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_'));
            if found {
                routes.push((method.to_uppercase(), path.clone()));
            }
        }
    }
    routes
}

/// Every write route and the audit action it records (spec 4.10). A route added to `main.rs`
/// without a row here fails `every_write_route_has_an_audit_action`.
const WRITE_AUDIT: &[(&str, &str, &str)] = &[
    ("POST", "/v1/captures", "capture.create"),
    ("POST", "/v1/sessions", "session.create"),
    ("DELETE", "/v1/sessions/current", "session.delete"),
    ("PATCH", "/v1/me", "volunteer.update"),
    ("POST", "/v1/shelters", "shelter.create"),
    ("PATCH", "/v1/shelters/{id}", "shelter.update"),
    ("POST", "/v1/shelters/{id}/close", "shelter.close"),
    ("POST", "/v1/shelters/{id}/confirm", "shelter.confirm"),
    ("POST", "/v1/shelters/{id}/revoke", "shelter.revoke"),
    ("PATCH", "/v1/events/{id}", "event.update"),
    (
        "POST",
        "/v1/shelters/{id}/endorsements/in-person/code",
        "endorsement.code",
    ),
    ("POST", "/v1/endorsements/in-person", "endorsement.create"),
    ("POST", "/v1/shelters/{id}/invites", "invite.create"),
    ("DELETE", "/v1/invites/{code}", "invite.cancel"),
    ("POST", "/v1/invites/{code}/accept", "invite.accept"),
    (
        "PATCH",
        "/v1/shelters/{id}/members/{volunteer_id}",
        "member.promote",
    ),
    (
        "DELETE",
        "/v1/shelters/{id}/members/{volunteer_id}",
        "member.remove",
    ),
    ("POST", "/v1/shelters/{id}/leave", "member.leave"),
    ("POST", "/v1/volunteers/{id}/ban", "volunteer.ban"),
    ("POST", "/v1/shelters/{id}/checkins", "person.checkin"),
    ("PATCH", "/v1/people/{person_id}", "person.update"),
    (
        "PUT",
        "/v1/people/{person_id}/biometrics",
        "person.biometrics",
    ),
    (
        "POST",
        "/v1/people/{person_id}/guardians",
        "person.guardian",
    ),
    ("POST", "/v1/shelters/{id}/exits", "person.exit"),
    ("POST", "/v1/shelters/{id}/households", "household.create"),
    ("PATCH", "/v1/households/{id}", "household.update"),
    ("POST", "/v1/households/{id}/cards", "card.issue"),
    ("POST", "/v1/cards/{code}/cancel", "card.cancel"),
    ("POST", "/v1/rescues", "rescue.create"),
    ("PATCH", "/v1/rescues/{id}", "rescue.update"),
    ("POST", "/v1/searches", "search.create"),
    ("POST", "/v1/matches/{id}/decision", "match.accept"),
    ("POST", "/v1/volunteers/{id}/roles", "role.grant"),
    ("DELETE", "/v1/volunteers/{id}/roles/{role}", "role.revoke"),
    ("PUT", "/v1/shelters/{id}/items", "shelter.items"),
    ("POST", "/v1/shelters/{id}/deliveries", "delivery.create"),
    ("POST", "/v1/deliveries/{id}/review", "delivery.review"),
    ("POST", "/v1/search-links", "search_link.create"),
    ("DELETE", "/v1/search-links/{code}", "search_link.cancel"),
    (
        "POST",
        "/v1/search-links/{code}/submit",
        "search_link.submit",
    ),
    ("POST", "/v1/alerts/{id}/resolve", "alert.resolve"),
];

/// The reads that are audited too (spec 4.10: every access to a thumbnail, to the data of a match
/// or to the export of a shelter) and the action each records.
const READ_AUDIT: &[(&str, &str, &str)] = &[
    ("GET", "/v1/shelters/{id}/export.csv", "export.shelter"),
    ("GET", "/v1/matches", "match.list"),
    ("GET", "/v1/matches/{id}", "match.view"),
    ("GET", "/v1/matches/{id}/thumbnail", "thumbnail.view"),
    (
        "GET",
        "/v1/matches/{id}/search-photos/{n}",
        "search_photo.view",
    ),
    ("GET", "/v1/people/{person_id}/thumbnail", "thumbnail.view"),
];

/// The source of every module under `src/`, test modules left out (`conformance_tests.rs` and
/// every `tests.rs` are test code; the others are cut at their `#[cfg(test)]` module). The name is
/// the path relative to `src/`.
fn sources() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![];
    walk(&root, &mut files);
    files.sort();
    files
        .into_iter()
        .filter(|p| {
            let name = p.file_name().unwrap();
            name != "conformance_tests.rs" && name != "tests.rs"
        })
        .map(|p| {
            let text = std::fs::read_to_string(&p).unwrap();
            // Test modules close each file: the action has to be in the code that runs.
            let production = text.split("#[cfg(test)]\nmod ").next().unwrap().to_string();
            (
                p.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                production,
            )
        })
        .collect()
}

#[test]
fn the_route_table_is_read_from_main() {
    let routes = registered_routes();
    assert!(routes.len() > 60, "{} routes", routes.len());
    for expected in [
        ("POST", "/v1/captures"),
        ("DELETE", "/v1/search-links/{code}"),
        ("GET", "/v1/shelters/{id}/export.csv"),
        ("PATCH", "/v1/shelters/{id}/members/{volunteer_id}"),
    ] {
        assert!(
            routes.contains(&(expected.0.into(), expected.1.into())),
            "{expected:?}"
        );
    }
}

#[test]
fn every_write_route_has_an_audit_action() {
    let writes: Vec<(String, String)> = registered_routes()
        .into_iter()
        .filter(|(method, _)| method != "GET")
        .collect();
    let mapped: Vec<(String, String)> = WRITE_AUDIT
        .iter()
        .map(|(m, p, _)| (m.to_string(), p.to_string()))
        .collect();
    for route in &writes {
        assert!(
            mapped.contains(route),
            "write route without audit row: {route:?}"
        );
    }
    for route in &mapped {
        assert!(
            writes.contains(route),
            "audit row of a route that is gone: {route:?}"
        );
    }
}

#[test]
fn every_audited_read_is_a_route_and_is_written_by_some_module() {
    let routes = registered_routes();
    let sources = sources();
    for (method, path, action) in READ_AUDIT {
        assert!(
            routes.contains(&(method.to_string(), path.to_string())),
            "{method} {path} is not a route"
        );
        let literal = format!("\"{action}\"");
        assert!(
            sources.iter().any(|(_, text)| text.contains(&literal)),
            "{path}: nothing writes {action}"
        );
    }
}

#[test]
fn every_audit_action_of_the_table_is_written_by_some_module() {
    let sources = sources();
    for (_, path, action) in WRITE_AUDIT {
        // `role.grant` and `role.revoke` are built as `role.{verb}.{role}`, `delivery.create` as
        // `delivery.{kind}{offline}{clamped}`.
        let literal = if action.starts_with("role.") {
            "role.{verb}".to_string()
        } else if *action == "delivery.create" {
            "\"delivery.{}{}{}\"".to_string()
        } else {
            format!("\"{action}\"")
        };
        assert!(
            sources.iter().any(|(_, text)| text.contains(&literal)),
            "{path}: nothing writes {action}"
        );
    }
}

// ---- criterion 13: nothing of spec 16 exists ----

#[test]
fn no_route_of_spec_16_is_registered() {
    for (method, path) in registered_routes() {
        // The photos of the family's search are not the photos of the site (spec 16.4).
        let path = path.replace("search-photos", "search-files");
        for banned in [
            "merge",
            "remote",
            "safe",
            "photo",
            "shelters-to-endorse",
            "events/nearby",
        ] {
            assert!(
                !path.contains(banned),
                "{method} {path}: spec 16 is built after the pilot"
            );
        }
    }
}

#[tokio::test]
async fn the_routes_of_spec_16_answer_404() {
    let app = TestApp::new().await;
    let who = app.actor("Ana").await;
    for (method, path) in [
        ("POST", "/v1/shelters/s1/merges"),
        ("POST", "/v1/merges/m1/decision"),
        ("POST", "/v1/shelters/s1/endorsements/remote"),
        ("GET", "/v1/events/e1/shelters-to-endorse"),
        ("POST", "/v1/safe-check"),
        ("GET", "/safe"),
        ("POST", "/v1/shelters/s1/photos"),
        ("POST", "/v1/shelters/s1/site-photos"),
    ] {
        let (status, _, _) = app.request(method, path, &[], None).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{method} {path} without session"
        );
        let (status, _) = app.send(method, path, &who.secret, Some(json!({}))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
    }
}

#[test]
fn no_collection_of_spec_16_is_written() {
    for (file, text) in sources() {
        for banned in [
            "\"merges\"",
            "\"safe_checks\"",
            "\"safe_check\"",
            "\"photos\"",
            "declared_event_ids",
        ] {
            assert!(!text.contains(banned), "{file} mentions {banned}");
        }
    }
}

// ---- the tour: one full flow, then the audit review and criteria 16 and 59 ----

const JPEG: [u8; 6] = [0xFF, 0xD8, 0xFF, 0xE0, 1, 2];
/// What the "photos" of the tour hold: it must never reach a response or a document.
const FACE: &[u8] = b"a face";
const TOKENS: &[&str] = &[
    "RAWTOKEN-P1",
    "RAWTOKEN-P3",
    "RAWTOKEN-P5",
    "RAWTOKEN-LATE",
    "RAWTOKEN-P6",
    // The tokens `TestApp::actor` and `join` give the volunteers of the tour.
    "tok-Admin",
    "tok-Vol",
    "tok-Defense",
    "tok-Second",
    "tok-Third",
    "tok-Fourth",
];

struct Tour {
    app: TestApp,
    bodies: Mutex<Vec<String>>,
}

impl Tour {
    async fn call(
        &self,
        who: &Actor,
        method: &str,
        path: &str,
        body: Option<Value>,
        expected: StatusCode,
    ) -> Value {
        let auth = format!("Bearer {}", who.secret);
        let (status, _, answer) = self
            .app
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
        self.bodies.lock().unwrap().push(answer.to_string());
        // Some routes answer `204` where others answer `200`.
        let no_content = expected == StatusCode::OK && status == StatusCode::NO_CONTENT;
        assert!(
            status == expected || no_content,
            "{method} {path}: {status} {answer}"
        );
        answer
    }

    /// The next photos sent to the (fake) Unico come back as these tokens, in order.
    fn tokens(&self, tokens: &[&str]) {
        self.app.unico.with(|s| {
            for token in tokens {
                s.post_replies.push_back((
                    200,
                    json!({ "id": "img", "state": "PROCESS_STATE_FINISHED",
                        "data": { "humanToken": { "value": token } } }),
                ));
            }
        });
    }

    async fn checkin(&self, who: &Actor, shelter: &str, extra: Value) -> String {
        let mut body = json!({
            "id": new_id(),
            "legal_basis": "consent",
            "client_created_at": store::now().to_string(),
            "data": {},
        });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        let answer = self
            .call(
                who,
                "POST",
                &format!("/v1/shelters/{shelter}/checkins"),
                Some(body),
                StatusCode::CREATED,
            )
            .await;
        answer["person_id"].as_str().unwrap().to_string()
    }
}

fn search_body(photos: usize) -> Value {
    json!({
        "id": new_id(),
        "photos_base64": (0..photos).map(|_| json!(STANDARD.encode(FACE))).collect::<Vec<_>>(),
        "missing_person": { "name": "Joao", "nickname": "Jo", "notes": "camisa azul" },
        "family": { "name": "Maria", "phone": "+55 51 99999-1234", "relationship": "mae",
            "language": "pt-BR" },
    })
}

/// Walks a document; `visit` gets every string with the key it sits under.
fn strings_of(value: &Value, key: &str, visit: &mut impl FnMut(&str, &str)) {
    match value {
        Value::String(s) => visit(key, s),
        Value::Array(items) => items.iter().for_each(|v| strings_of(v, key, visit)),
        Value::Object(map) => map.iter().for_each(|(k, v)| strings_of(v, k, visit)),
        _ => {}
    }
}

/// Criterion 59 for one text: no raw token, no photo. `thumbnail_encrypted` is the only place a
/// person's image may be, and only encrypted.
fn assert_clean(what: &str, key: &str, text: &str) {
    if key == "thumbnail_encrypted" {
        return;
    }
    for token in TOKENS.iter() {
        assert!(!text.contains(token), "{what}: raw token {token}");
    }
    for photo in [STANDARD.encode(FACE), STANDARD.encode(JPEG)] {
        assert!(!text.contains(&photo), "{what}: a photo in `{key}`");
    }
    // A real photo is far longer than any id, hash or name that happens to hold these letters.
    for magic in ["/9j/", "iVBOR", "data:image"] {
        assert!(
            text.len() < 200 || !text.contains(magic),
            "{what}: image data in `{key}`"
        );
    }
}

/// The documents of a collection as JSON. A person's encrypted thumbnail is bytes, which JSON has
/// no value for: it is left out here and checked on its own.
async fn documents(db: &firestore::FirestoreDb, collection: &str) -> Vec<Value> {
    // Photos of a search are encrypted bytes, checked on their own in `matches`.
    if collection == "search_photos" {
        return vec![];
    }
    if collection != "people" {
        return all_docs(db, collection).await;
    }
    let people: Vec<crate::people::PersonDoc> = db
        .fluent()
        .select()
        .from(collection)
        .obj()
        .query()
        .await
        .unwrap();
    people
        .into_iter()
        .map(|mut person| {
            person.thumbnail_encrypted = None;
            serde_json::to_value(person).unwrap()
        })
        .collect()
}

async fn actions_with_basis(app: &TestApp) -> Vec<(String, Option<String>)> {
    all_docs(&app.state.db, "audit_log")
        .await
        .iter()
        .map(|e| {
            (
                e["action"].as_str().unwrap().to_string(),
                e["legal_basis"].as_str().map(str::to_string),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_tour_audits_every_write_and_leaves_no_token_or_photo_anywhere() {
    let mut app = TestApp::new().await;
    // No delivery is drawn for the Unico: the review of the tour is the duplicate one.
    std::sync::Arc::make_mut(&mut app.state.config).delivery_sample_rate = 0.0;
    let t = Tour {
        app,
        bodies: Mutex::new(vec![]),
    };
    let app = &t.app;
    let created = StatusCode::CREATED;
    let ok = StatusCode::OK;

    // People and a shelter that a validator confirmed.
    let admin = app.actor("Admin").await;
    let shelter = app.confirmed_shelter(&admin, "Abrigo", ORIGIN).await;
    let vol = app
        .join(
            &app.invite(&admin, &shelter, "qr", "volunteer").await,
            "Vol",
        )
        .await;
    let civil_defense = app.actor("Defense").await;
    app.grant(&civil_defense, GlobalRole::CivilDefense).await;

    // A login capture: its answer is one of the responses that must not hold a token.
    let capture = t
        .call(
            &vol,
            "POST",
            "/v1/captures",
            Some(json!({ "purpose": "login" })),
            created,
        )
        .await;
    assert!(capture["capture_id"].is_string());

    // Volunteer and shelter.
    t.call(
        &vol,
        "PATCH",
        "/v1/me",
        Some(json!({ "name": "Vol Dois" })),
        ok,
    )
    .await;
    t.call(
        &admin,
        "PATCH",
        &format!("/v1/shelters/{shelter}"),
        Some(json!({ "capacity": 50 })),
        ok,
    )
    .await;
    let shelter_doc: ShelterDoc = store::get(&app.state.db, shelters::COLLECTION, &shelter)
        .await
        .unwrap()
        .unwrap();
    let event_id = shelter_doc.event_id.clone().unwrap();
    t.call(
        &admin,
        "PATCH",
        &format!("/v1/events/{event_id}"),
        Some(json!({ "name": "Enchente" })),
        ok,
    )
    .await;

    // Members: invite, cancel, promote, remove, leave.
    let cancelled = app.invite(&admin, &shelter, "qr", "volunteer").await;
    t.call(
        &admin,
        "DELETE",
        &format!("/v1/invites/{cancelled}"),
        None,
        ok,
    )
    .await;
    let second = app
        .join(
            &app.invite(&admin, &shelter, "qr", "volunteer").await,
            "Second",
        )
        .await;
    t.call(
        &admin,
        "PATCH",
        &format!("/v1/shelters/{shelter}/members/{}", second.id),
        Some(json!({ "role": "admin" })),
        ok,
    )
    .await;
    let third = app
        .join(
            &app.invite(&admin, &shelter, "qr", "volunteer").await,
            "Third",
        )
        .await;
    t.call(
        &admin,
        "DELETE",
        &format!("/v1/shelters/{shelter}/members/{}", third.id),
        None,
        ok,
    )
    .await;
    let fourth = app
        .join(
            &app.invite(&admin, &shelter, "qr", "volunteer").await,
            "Fourth",
        )
        .await;
    t.call(
        &fourth,
        "POST",
        &format!("/v1/shelters/{shelter}/leave"),
        None,
        ok,
    )
    .await;

    // Check-ins: by photo (token P1) and without biometrics, with a thumbnail.
    t.tokens(&["RAWTOKEN-P1"]);
    let p1 = t
        .checkin(
            &vol,
            &shelter,
            json!({ "photo_base64": STANDARD.encode(FACE) }),
        )
        .await;
    let p2 = t
        .checkin(
            &vol,
            &shelter,
            json!({ "no_biometrics": true, "thumbnail_base64": STANDARD.encode(JPEG) }),
        )
        .await;
    t.call(
        &vol,
        "PATCH",
        &format!("/v1/people/{p1}"),
        Some(json!({ "data": { "name": "Ana" }, "client_edited_at": store::now().to_string() })),
        ok,
    )
    .await;
    t.call(&vol, "GET", &format!("/v1/people/{p2}/thumbnail"), None, ok)
        .await;

    // Household and card.
    let household = new_id();
    t.call(
        &vol,
        "POST",
        &format!("/v1/shelters/{shelter}/households"),
        Some(json!({ "id": household, "member_ids": [p1, p2],
            "client_created_at": "2026-10-21T10:00:00Z" })),
        created,
    )
    .await;
    t.call(
        &vol,
        "PATCH",
        &format!("/v1/households/{household}"),
        Some(json!({ "pets": [{ "species": "gato", "count": 1 }] })),
        ok,
    )
    .await;
    t.call(
        &vol,
        "POST",
        &format!("/v1/households/{household}/cards"),
        Some(json!({ "code": "ABCD2345", "client_issued_at": "2026-10-21T10:00:00Z" })),
        created,
    )
    .await;
    // Items, two deliveries of the same item (the second goes to review) and the admin's decision.
    t.call(
        &admin,
        "PUT",
        &format!("/v1/shelters/{shelter}/items"),
        Some(
            json!({ "items": { "kit": { "scope": "per_household", "period_days": 7,
            "once_per_event": false, "high_value": false } } }),
        ),
        ok,
    )
    .await;
    let deliver = |id: String| {
        json!({ "id": id, "card_code": "ABCD2345", "person_id": p1, "item": "kit", "quantity": 1,
            "delivered_at": store::now().to_string() })
    };
    t.call(
        &vol,
        "POST",
        &format!("/v1/shelters/{shelter}/deliveries"),
        Some(deliver(new_id())),
        created,
    )
    .await;
    let second_delivery = t
        .call(
            &vol,
            "POST",
            &format!("/v1/shelters/{shelter}/deliveries"),
            Some(deliver(new_id())),
            created,
        )
        .await;
    assert_eq!(
        second_delivery["status"],
        json!("in_review"),
        "{second_delivery}"
    );
    t.call(
        &admin,
        "POST",
        &format!(
            "/v1/deliveries/{}/review",
            second_delivery["delivery_id"].as_str().unwrap()
        ),
        Some(json!({ "decision": "reviewed_ok", "note": "familia grande" })),
        ok,
    )
    .await;
    t.call(
        &vol,
        "POST",
        "/v1/cards/ABCD2345/cancel",
        Some(json!({})),
        ok,
    )
    .await;

    // A search that meets a later rescue; the Civil Defense lists, reads and decides.
    t.tokens(&["RAWTOKEN-P3"]);
    t.call(
        &vol,
        "POST",
        "/v1/searches",
        Some(search_body(1)),
        StatusCode::ACCEPTED,
    )
    .await;
    t.tokens(&["RAWTOKEN-P3"]);
    t.call(
        &vol,
        "POST",
        "/v1/rescues",
        Some(
            json!({ "id": new_id(), "lat": ORIGIN.0, "lon": ORIGIN.1, "accuracy": 10.0,
            "captured_at": store::now().to_string(),
            "photo_base64": STANDARD.encode(FACE) }),
        ),
        created,
    )
    .await;
    let listed = t.call(&civil_defense, "GET", "/v1/matches", None, ok).await;
    let match_id = listed["matches"][0]["id"]
        .as_str()
        .expect("a match")
        .to_string();
    t.call(
        &civil_defense,
        "GET",
        &format!("/v1/matches/{match_id}"),
        None,
        ok,
    )
    .await;
    t.call(
        &civil_defense,
        "POST",
        &format!("/v1/matches/{match_id}/decision"),
        Some(json!({ "decision": "accept", "decided_by": "person" })),
        ok,
    )
    .await;

    // A family's link, its submission and its cancelation.
    let link = t
        .call(
            &vol,
            "POST",
            "/v1/search-links",
            Some(json!({ "shelter_id": shelter })),
            created,
        )
        .await;
    let code = link["code"].as_str().unwrap().to_string();
    t.tokens(&["RAWTOKEN-P5"]);
    let (status, _, answer) = app
        .request(
            "POST",
            &format!("/v1/search-links/{code}/submit"),
            &[("x-device-id", "dev-family")],
            Some(search_body(1)),
        )
        .await;
    t.bodies.lock().unwrap().push(answer.to_string());
    assert_eq!(status, StatusCode::ACCEPTED, "{answer}");
    // A link is used once; another one is canceled before anyone uses it.
    let unused = t
        .call(
            &vol,
            "POST",
            "/v1/search-links",
            Some(json!({ "shelter_id": shelter })),
            created,
        )
        .await;
    let unused = unused["code"].as_str().unwrap();
    t.call(
        &vol,
        "DELETE",
        &format!("/v1/search-links/{unused}"),
        None,
        ok,
    )
    .await;

    // An alert the admin resolves, and a role the Civil Defense grants and takes back.
    let alert_id = new_id();
    let alert = AlertDoc::new(AlertKind::VolunteerRemoved, "volunteer", "x", json!({}))
        .for_shelters(vec![shelter.clone()]);
    store::replace(&app.state.db, alerts::COLLECTION, &alert_id, &alert)
        .await
        .unwrap();
    t.call(
        &admin,
        "POST",
        &format!("/v1/alerts/{alert_id}/resolve"),
        None,
        ok,
    )
    .await;
    t.call(
        &civil_defense,
        "POST",
        &format!("/v1/volunteers/{}/roles", vol.id),
        Some(json!({ "role": "mediator" })),
        ok,
    )
    .await;
    t.call(
        &civil_defense,
        "DELETE",
        &format!("/v1/volunteers/{}/roles/mediator", vol.id),
        None,
        ok,
    )
    .await;

    // A child with a verified guardian, late biometrics, an exit and an edited rescue.
    let child = t
        .checkin(
            &vol,
            &shelter,
            json!({ "no_biometrics": true, "legal_basis": "child_protection",
                "data": { "name": "Menina", "is_child": true, "child_status": "with_guardian" } }),
        )
        .await;
    // A guardian is a person verified by liveness, which only the SDK gives: the document is put.
    let guardian = new_id();
    let mut guardian_doc =
        crate::exits::fixtures::person(crate::people::Level::Verified, Some(&shelter));
    guardian_doc.event_ids = vec![event_id.clone()];
    store::replace(&app.state.db, "people", &guardian, &guardian_doc)
        .await
        .unwrap();
    t.call(
        &vol,
        "POST",
        &format!("/v1/people/{child}/guardians"),
        Some(json!({ "guardian_id": guardian, "link": "declared" })),
        ok,
    )
    .await;
    let p6 = t
        .checkin(&vol, &shelter, json!({ "no_biometrics": true }))
        .await;
    let capture = t
        .call(
            &vol,
            "POST",
            "/v1/captures",
            Some(json!({ "purpose": "biometrics", "expected_person_id": p6 })),
            created,
        )
        .await;
    let capture_id = capture["capture_id"].as_str().unwrap();
    let stored: crate::auth::CaptureDoc = store::get(&app.state.db, "captures", capture_id)
        .await
        .unwrap()
        .unwrap();
    app.unico
        .finish_with_token(&stored.process_id, "RAWTOKEN-P6");
    t.call(
        &vol,
        "PUT",
        &format!("/v1/people/{p6}/biometrics"),
        Some(json!({ "capture_id": capture_id })),
        ok,
    )
    .await;
    let leaving = t
        .checkin(&vol, &shelter, json!({ "no_biometrics": true }))
        .await;
    t.call(
        &vol,
        "POST",
        &format!("/v1/shelters/{shelter}/exits"),
        Some(json!({ "person_id": leaving, "exit_destination": "own_home" })),
        ok,
    )
    .await;
    let rescue_id = new_id();
    t.tokens(&["RAWTOKEN-P3"]);
    t.call(
        &vol,
        "POST",
        "/v1/rescues",
        Some(
            json!({ "id": rescue_id, "lat": ORIGIN.0, "lon": ORIGIN.1, "accuracy": 10.0,
            "captured_at": store::now().to_string(),
            "photo_base64": STANDARD.encode(FACE) }),
        ),
        created,
    )
    .await;
    t.call(
        &vol,
        "PATCH",
        &format!("/v1/rescues/{rescue_id}"),
        Some(json!({ "landing_point": "Cais" })),
        ok,
    )
    .await;

    // An in-person endorsement of a new shelter, the Civil Defense revoking it, a ban and the export.
    let newcomer = app.actor("Newcomer").await;
    let new_shelter = app
        .create_shelter(&newcomer, "Novo", north_of_origin(20_000.0))
        .await;
    let code = t
        .call(
            &newcomer,
            "POST",
            &format!("/v1/shelters/{new_shelter}/endorsements/in-person/code"),
            Some(json!({ "lat": north_of_origin(20_000.0).0, "lon": ORIGIN.1, "accuracy": 10.0 })),
            created,
        )
        .await;
    t.call(
        &admin,
        "POST",
        "/v1/endorsements/in-person",
        Some(
            json!({ "code": code["code"], "endorser_shelter_id": shelter,
            "lat": north_of_origin(20_000.0).0, "lon": ORIGIN.1, "accuracy": 10.0 }),
        ),
        ok,
    )
    .await;
    t.call(
        &civil_defense,
        "POST",
        &format!("/v1/shelters/{new_shelter}/revoke"),
        Some(json!({ "reason": "o local não existe" })),
        ok,
    )
    .await;
    t.call(
        &civil_defense,
        "POST",
        &format!("/v1/volunteers/{}/ban", fourth.id),
        Some(json!({ "reason": "fraude" })),
        ok,
    )
    .await;
    let (status, _, exported) = app
        .request(
            "GET",
            &format!("/v1/shelters/{shelter}/export.csv"),
            &[
                ("authorization", format!("Bearer {}", admin.secret).as_str()),
                ("x-device-id", admin.device.as_str()),
            ],
            None,
        )
        .await;
    t.bodies.lock().unwrap().push(exported.to_string());
    assert_eq!(status, ok, "{exported}");

    // The `taba unico resume` path.
    degraded::enter_limit(&app.state.db).await.unwrap();
    assert!(degraded::resume(&app.state.db).await.unwrap());

    // Criterion 59: no response and no document holds a raw token or a photo.
    for body in t.bodies.lock().unwrap().iter() {
        assert_clean("a response", "", body);
    }
    let mut collections: Vec<String> = app
        .state
        .db
        .fluent()
        .list()
        .collections()
        .get_page()
        .await
        .unwrap()
        .collection_ids;
    collections.sort();
    for needed in [
        "people",
        "searches",
        "matches",
        "rescues",
        "audit_log",
        "volunteers",
    ] {
        assert!(collections.contains(&needed.to_string()), "{collections:?}");
    }
    for collection in &collections {
        for doc in documents(&app.state.db, collection).await {
            strings_of(&doc, "", &mut |key, text| {
                assert_clean(&format!("{collection} document"), key, text)
            });
        }
    }
    // The only image kept is the thumbnail, encrypted.
    let person: crate::people::PersonDoc = store::get(&app.state.db, "people", &p2)
        .await
        .unwrap()
        .unwrap();
    let stored = person.thumbnail_encrypted.expect("the thumbnail is kept");
    assert!(!stored.windows(JPEG.len()).any(|w| w == JPEG));

    // Close the event and run the hourly job: `event.close` and `person.thumbnail_purge`.
    t.call(
        &admin,
        "POST",
        &format!("/v1/shelters/{shelter}/close"),
        None,
        ok,
    )
    .await;
    let mut event: EventDoc = store::get(&app.state.db, events::COLLECTION, &event_id)
        .await
        .unwrap()
        .unwrap();
    event.last_activity_at = firestore::FirestoreInstant::from_microsecond(
        store::now().as_microsecond() - 31 * 86_400 * 1_000_000,
    )
    .unwrap();
    store::replace(&app.state.db, events::COLLECTION, &event_id, &event)
        .await
        .unwrap();
    jobs::run_once(&app.state).await.unwrap();
    t.call(&vol, "DELETE", "/v1/sessions/current", None, ok)
        .await;

    // The audit review: every entry names its legal basis, and the QA's list is there.
    let entries = actions_with_basis(app).await;
    for (action, basis) in &entries {
        assert!(basis.is_some(), "{action} has no legal basis");
    }
    for expected in [
        ("capture.create", "fraud_prevention"),
        ("session.create", "fraud_prevention"),
        ("session.delete", "fraud_prevention"),
        ("volunteer.update", "fraud_prevention"),
        ("shelter.update", "fraud_prevention"),
        ("shelter.confirm", "fraud_prevention"),
        ("shelter.close", "fraud_prevention"),
        ("event.update", "public_policy"),
        ("event.close", "public_policy"),
        ("invite.create", "fraud_prevention"),
        ("invite.cancel", "fraud_prevention"),
        ("invite.accept", "fraud_prevention"),
        ("member.promote", "fraud_prevention"),
        ("member.remove", "fraud_prevention"),
        ("member.leave", "fraud_prevention"),
        ("person.checkin", "consent"),
        ("person.update", "consent"),
        ("thumbnail.view", "vital_interest"),
        ("person.thumbnail_purge", "public_policy"),
        ("household.create", "fraud_prevention"),
        ("household.update", "fraud_prevention"),
        ("card.issue", "fraud_prevention"),
        ("card.cancel", "fraud_prevention"),
        ("search.create", "vital_interest"),
        ("rescue.create", "vital_interest"),
        ("match.create", "vital_interest"),
        ("match.list", "vital_interest"),
        ("match.view", "vital_interest"),
        ("match.accept", "vital_interest"),
        ("search_link.create", "vital_interest"),
        ("search_link.submit", "family_consent"),
        ("search_link.cancel", "vital_interest"),
        ("alert.resolve", "public_policy"),
        ("role.grant.mediator", "public_policy"),
        ("role.revoke.mediator", "public_policy"),
        ("unico.resume", "public_policy"),
        ("shelter.create", "fraud_prevention"),
        ("shelter.revoke", "fraud_prevention"),
        ("shelter.items", "fraud_prevention"),
        ("endorsement.create", "fraud_prevention"),
        ("volunteer.ban", "fraud_prevention"),
        ("delivery.create", "fraud_prevention"),
        ("delivery.review", "fraud_prevention"),
        ("person.exit", "consent"),
        ("person.biometrics", "consent"),
        ("person.guardian", "child_protection"),
        ("rescue.update", "vital_interest"),
        ("export.shelter", "public_policy"),
    ] {
        assert!(
            entries.contains(&(expected.0.to_string(), Some(expected.1.to_string()))),
            "{expected:?} is not in the audit log: {:?}",
            entries.iter().map(|(a, _)| a).collect::<Vec<_>>()
        );
    }
}

// ---- criterion 16: an `unconfirmed` shelter ----

#[tokio::test]
async fn an_unconfirmed_shelter_cannot_search_nor_export_and_sees_only_its_own_stays() {
    let t = Tour {
        app: TestApp::new().await,
        bodies: Mutex::new(vec![]),
    };
    let app = &t.app;
    let admin = app.actor("Admin").await;
    let confirmed = app
        .confirmed_shelter(&admin, "Confirmed", north_of_origin(0.0))
        .await;
    let other = app.actor("Other").await;
    let unconfirmed = app
        .create_shelter(&other, "Unconfirmed", north_of_origin(500.0))
        .await;

    // The same person is checked in at the confirmed shelter, then at the unconfirmed one.
    t.tokens(&["RAWTOKEN-LATE", "RAWTOKEN-LATE"]);
    let photo = json!({ "photo_base64": STANDARD.encode(FACE) });
    t.checkin(&admin, &confirmed, photo.clone()).await;
    let history = {
        let mut body = json!({
            "id": new_id(), "legal_basis": "consent",
            "client_created_at": store::now().to_string(), "data": {},
        });
        body["photo_base64"] = photo["photo_base64"].clone();
        t.call(
            &other,
            "POST",
            &format!("/v1/shelters/{unconfirmed}/checkins"),
            Some(body),
            StatusCode::CREATED,
        )
        .await["history"]
            .clone()
    };
    assert_eq!(history["has_other_stays"], json!(false));
    let stays = history["stays"].as_array().unwrap();
    assert_eq!(stays.len(), 1);
    assert_eq!(stays[0]["shelter_id"], json!(unconfirmed));

    // No search.
    let sent = app.unico.post_count();
    t.call(
        &other,
        "POST",
        "/v1/searches",
        Some(search_body(1)),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(
        app.unico.post_count(),
        sent,
        "the refused search sent nothing to the Unico"
    );

    // No export.
    let (status, _, answer) = app
        .request(
            "GET",
            &format!("/v1/shelters/{unconfirmed}/export.csv"),
            &[("authorization", &format!("Bearer {}", other.secret))],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
}

// ---- the scan itself must see what it looks for ----

#[test]
#[should_panic(expected = "raw token")]
fn the_scan_sees_a_raw_token() {
    assert_clean("doc", "note", "x RAWTOKEN-P1 y");
}

#[test]
#[should_panic(expected = "a photo")]
fn the_scan_sees_a_photo() {
    assert_clean("doc", "photo", &STANDARD.encode(FACE));
}

#[test]
#[should_panic(expected = "image data")]
fn the_scan_sees_a_long_image_payload() {
    assert_clean("doc", "photo", &format!("/9j/{}", "A".repeat(300)));
}

#[test]
fn the_scan_lets_the_encrypted_thumbnail_and_short_ids_through() {
    let long = format!("/9j/{}", "A".repeat(300));
    assert_clean("doc", "thumbnail_encrypted", &long);
    assert_clean("doc", "id", "iVBOR-1");
}
