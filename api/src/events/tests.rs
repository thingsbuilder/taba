use std::time::{Duration, Instant};

use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde_json::{Value, json};

use super::domain::{EventPatch, apply, auto_name};
use super::persist::activity_due;
use super::*;
use crate::shelters::{self, Address, ShelterDoc};
use crate::store;
use crate::volunteers::GlobalRole;

fn at(rfc3339: &str) -> FirestoreInstant {
    rfc3339.parse().unwrap()
}

#[test]
fn automatic_names_follow_city_state_and_month() {
    let when = at("2026-05-09T12:00:00Z");
    assert_eq!(
        auto_name(Some("Porto Alegre"), Some("RS"), when),
        "Porto Alegre/RS – 05/2026"
    );
    assert_eq!(auto_name(Some("Canoas"), None, when), "Canoas – 05/2026");
    assert_eq!(auto_name(None, Some("RS"), when), "Evento – 05/2026");
    assert_eq!(
        auto_name(None, None, at("2026-12-31T23:59:59Z")),
        "Evento – 12/2026"
    );
}

fn event(name: &str, auto_named: bool) -> EventDoc {
    EventDoc {
        name: name.into(),
        auto_named,
        status: EventStatus::Active,
        created_at: at("2026-05-09T12:00:00Z"),
        last_activity_at: at("2026-05-09T12:00:00Z"),
        closed_at: None,
        closure_reason: None,
        purge_at: None,
        delivery_purge_at: None,
        purged_at: None,
        stats: None,
        reference_contacts: ReferenceContacts::default(),
    }
}

fn address(city: Option<&str>, state: Option<&str>) -> Address {
    Address {
        text: None,
        city: city.map(Into::into),
        state: state.map(Into::into),
        pending: false,
    }
}

#[test]
fn only_a_placeholder_name_is_redone_when_the_address_arrives() {
    let placeholder = event("Evento – 05/2026", true);
    assert_eq!(
        placeholder
            .renamed_for(&address(Some("Canoas"), Some("RS")))
            .as_deref(),
        Some("Canoas/RS – 05/2026")
    );
    // An address without a city cannot name it yet.
    assert_eq!(placeholder.renamed_for(&address(None, Some("RS"))), None);
    // Already named after a city, or renamed by a person: left alone.
    assert_eq!(
        event("Canoas/RS – 05/2026", true).renamed_for(&address(Some("Porto Alegre"), Some("RS"))),
        None
    );
    assert_eq!(
        event("Evento – 05/2026", false).renamed_for(&address(Some("Canoas"), None)),
        None
    );
}

#[test]
fn rename_validation_and_contacts() {
    let mut e = event("Evento – 05/2026", true);
    let patch = |value: Value| serde_json::from_value::<EventPatch>(value).unwrap();
    assert!(apply(&mut e, &patch(json!({ "name": "  " }))).is_err());
    assert!(apply(&mut e, &patch(json!({ "name": "x".repeat(101) }))).is_err());
    apply(&mut e, &patch(json!({ "name": " Cheia do Guaíba " }))).unwrap();
    assert_eq!((e.name.as_str(), e.auto_named), ("Cheia do Guaíba", false));

    apply(&mut e, &patch(json!({ "reference_contacts": { "civil_defense_phone": "199", "child_protection_phone": "100" } }))).unwrap();
    assert_eq!(
        e.reference_contacts.civil_defense_phone.as_deref(),
        Some("199")
    );
    // A key left out keeps its phone; null clears; too long is refused.
    apply(
        &mut e,
        &patch(json!({ "reference_contacts": { "child_protection_phone": null } })),
    )
    .unwrap();
    assert_eq!(
        e.reference_contacts,
        ReferenceContacts {
            civil_defense_phone: Some("199".into()),
            child_protection_phone: None
        }
    );
    assert!(
        apply(
            &mut e,
            &patch(json!({ "reference_contacts": { "civil_defense_phone": "1".repeat(31) } }))
        )
        .is_err()
    );
}

#[tokio::test]
async fn activity_is_written_at_most_once_an_hour_per_event() {
    let app = crate::test_support::TestApp::new().await;
    let t0 = Instant::now();
    assert!(activity_due(&app.state, "e1", t0));
    assert!(!activity_due(
        &app.state,
        "e1",
        t0 + Duration::from_secs(10)
    ));
    assert!(!activity_due(
        &app.state,
        "e1",
        t0 + Duration::from_secs(3599)
    ));
    assert!(
        activity_due(&app.state, "e2", t0 + Duration::from_secs(10)),
        "another event"
    );
    assert!(activity_due(
        &app.state,
        "e1",
        t0 + Duration::from_secs(3600)
    ));
    assert!(!activity_due(
        &app.state,
        "e1",
        t0 + Duration::from_secs(3601)
    ));
}

mod patch_tests {
    use super::*;
    use crate::test_support::{ORIGIN, TestApp, audit_actions, new_id, north_of_origin};

    async fn event_of(app: &TestApp, shelter: &str) -> String {
        let doc: crate::shelters::ShelterDoc =
            store::get(&app.state.db, crate::shelters::COLLECTION, shelter)
                .await
                .unwrap()
                .unwrap();
        doc.event_id.unwrap()
    }

    #[tokio::test]
    async fn the_name_is_changed_by_an_admin_of_the_event_or_the_civil_defense_only() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.create_shelter(&admin, "A", ORIGIN).await;
        let event = event_of(&app, &shelter).await;
        let volunteer = app
            .join(
                &app.invite(&admin, &shelter, "qr", "volunteer").await,
                "Vol",
            )
            .await;
        // An admin of a shelter in another event.
        let far_admin = app.actor("Far").await;
        app.create_shelter(&far_admin, "Longe", north_of_origin(200_000.0))
            .await;
        let validator = app.actor("Val").await;
        app.grant(&validator, GlobalRole::Validator).await;
        let civil_defense = app.actor("DC").await;
        app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let path = format!("/v1/events/{event}");
        let rename = |name: &str| Some(json!({ "name": name }));

        for who in [&volunteer, &far_admin, &validator] {
            assert_eq!(
                app.send("PATCH", &path, &who.secret, rename("Nova"))
                    .await
                    .0,
                StatusCode::FORBIDDEN,
                "{}",
                who.id
            );
        }
        let (status, body) = app
            .send("PATCH", &path, &admin.secret, rename("Cheia de outubro"))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["event"],
            json!({ "id": event, "name": "Cheia de outubro", "auto_named": false, "status": "active", "reference_contacts": {} })
        );
        assert_eq!(
            app.send("PATCH", &path, &civil_defense.secret, rename("Cheia 2026"))
                .await
                .0,
            StatusCode::OK
        );
        assert!(
            audit_actions(&app.state.db)
                .await
                .contains(&"event.update".to_string())
        );
        assert_eq!(
            app.request("PATCH", &path, &[], rename("x")).await.0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn the_reference_contacts_are_set_by_a_validator_or_the_civil_defense_only() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.create_shelter(&admin, "A", ORIGIN).await;
        let event = event_of(&app, &shelter).await;
        let path = format!("/v1/events/{event}");
        let contacts = Some(
            json!({ "reference_contacts": { "civil_defense_phone": "199", "child_protection_phone": "100" } }),
        );
        // Not even the admin of the event's shelter.
        assert_eq!(
            app.send("PATCH", &path, &admin.secret, contacts.clone())
                .await
                .0,
            StatusCode::FORBIDDEN
        );

        let validator = app.actor("Val").await;
        app.grant(&validator, GlobalRole::Validator).await;
        let (status, body) = app.send("PATCH", &path, &validator.secret, contacts).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["event"]["reference_contacts"],
            json!({ "civil_defense_phone": "199", "child_protection_phone": "100" })
        );
        // The validator is no admin, so the name stays out of reach, even in the same request.
        let (status, _) = app
            .send(
                "PATCH",
                &path,
                &validator.secret,
                Some(
                    json!({ "name": "x", "reference_contacts": { "civil_defense_phone": "190" } }),
                ),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let stored: EventDoc = store::get(&app.state.db, COLLECTION, &event)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.reference_contacts.civil_defense_phone.as_deref(),
            Some("199"),
            "a refused request changes nothing"
        );
        // The shelter's own view carries the contacts for the child alerts.
        let (_, seen) = app
            .send(
                "GET",
                &format!("/v1/shelters/{shelter}"),
                &admin.secret,
                None,
            )
            .await;
        assert_eq!(
            seen["shelter"]["event"]["reference_contacts"]["child_protection_phone"],
            "100"
        );
    }

    #[tokio::test]
    async fn invalid_values_and_unknown_events_are_refused() {
        let app = TestApp::new().await;
        let civil_defense = app.actor("DC").await;
        app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let admin = app.actor("Admin").await;
        let event = event_of(&app, &app.create_shelter(&admin, "A", ORIGIN).await).await;
        for bad in [
            json!({ "name": "  " }),
            json!({ "name": "x".repeat(101) }),
            json!({ "reference_contacts": { "civil_defense_phone": "1".repeat(31) } }),
        ] {
            let (status, body) = app
                .send(
                    "PATCH",
                    &format!("/v1/events/{event}"),
                    &civil_defense.secret,
                    Some(bad),
                )
                .await;
            assert_eq!(
                (status, body["error"].as_str()),
                (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_event"))
            );
        }
        let (status, body) = app
            .send(
                "PATCH",
                &format!("/v1/events/{}", new_id()),
                &civil_defense.secret,
                Some(json!({ "name": "x" })),
            )
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::NOT_FOUND, Some("event_not_found"))
        );
    }

    #[tokio::test]
    async fn the_activity_write_goes_through_the_api_without_touching_the_name() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.create_shelter(&admin, "A", ORIGIN).await;
        let event = event_of(&app, &shelter).await;
        let civil_defense = app.actor("DC").await;
        app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        app.send(
            "PATCH",
            &format!("/v1/events/{event}"),
            &civil_defense.secret,
            Some(
                json!({ "name": "Cheia", "reference_contacts": { "civil_defense_phone": "199" } }),
            ),
        )
        .await;
        app.state.event_activity.lock().unwrap().clear();
        touch(&app.state, Some(&event)).await;
        let stored: EventDoc = store::get(&app.state.db, COLLECTION, &event)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                stored.name.as_str(),
                stored.auto_named,
                stored.reference_contacts.civil_defense_phone.as_deref()
            ),
            ("Cheia", false, Some("199"))
        );
        // An event that does not exist is not created by the activity write.
        touch(&app.state, Some("ghost")).await;
        assert!(
            store::get::<EventDoc>(&app.state.db, COLLECTION, "ghost")
                .await
                .unwrap()
                .is_none()
        );
        touch(&app.state, None).await;
    }

    /// Closes the event of `shelter_id` the way the hourly task does.
    async fn close_event_of(app: &crate::test_support::TestApp, shelter_id: &str) -> String {
        let shelter: ShelterDoc = store::get(&app.state.db, shelters::COLLECTION, shelter_id)
            .await
            .unwrap()
            .unwrap();
        let event_id = shelter.event_id.unwrap();
        let mut event: EventDoc = store::get(&app.state.db, COLLECTION, &event_id)
            .await
            .unwrap()
            .unwrap();
        event.status = EventStatus::Closed;
        event.closed_at = Some(store::now());
        store::replace(&app.state.db, COLLECTION, &event_id, &event)
            .await
            .unwrap();
        event_id
    }

    #[tokio::test]
    async fn every_write_of_a_shelter_of_a_closed_event_is_409_and_reads_still_work() {
        use crate::exits::fixtures::{child, person, put};
        use crate::people::{ChildStatus, Level};
        use crate::test_support::{ORIGIN, TestApp, new_id};

        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.create_shelter(&admin, "A", ORIGIN).await;

        let (adult, kid, guardian) = (new_id(), new_id(), new_id());
        put(&app, &adult, &person(Level::Assisted, Some(&shelter))).await;
        put(&app, &guardian, &person(Level::Verified, Some(&shelter))).await;
        put(
            &app,
            &kid,
            &child(&shelter, ChildStatus::Unaccompanied, vec![]),
        )
        .await;
        let household = new_id();
        let (status, body) = app
            .send(
                "POST",
                &format!("/v1/shelters/{shelter}/households"),
                &admin.secret,
                Some(json!({ "id": household, "member_ids": [adult], "client_created_at": "2026-10-21T10:00:00Z" })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let card = "ABCD2345";
        let issue = json!({ "code": card, "client_issued_at": "2026-10-21T10:00:00Z" });
        let (status, body) = app
            .send(
                "POST",
                &format!("/v1/households/{household}/cards"),
                &admin.secret,
                Some(issue),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");

        close_event_of(&app, &shelter).await;

        let at = "2026-10-21T11:00:00Z";
        let writes = [
            (
                "POST",
                format!("/v1/shelters/{shelter}/checkins"),
                json!({ "id": new_id(), "legal_basis": "consent", "client_created_at": at, "data": {}, "no_biometrics": true }),
            ),
            (
                "PATCH",
                format!("/v1/people/{adult}"),
                json!({ "data": {}, "client_edited_at": at }),
            ),
            (
                "PUT",
                format!("/v1/people/{adult}/biometrics"),
                json!({ "photo_base64": "AAAA" }),
            ),
            (
                "POST",
                format!("/v1/people/{kid}/guardians"),
                json!({ "guardian_id": guardian, "link": "declared" }),
            ),
            (
                "POST",
                format!("/v1/shelters/{shelter}/exits"),
                json!({ "person_id": adult, "exit_destination": "own_home" }),
            ),
            (
                "POST",
                format!("/v1/shelters/{shelter}/households"),
                json!({ "id": new_id(), "member_ids": [guardian], "client_created_at": at }),
            ),
            (
                "PATCH",
                format!("/v1/households/{household}"),
                json!({ "add_member_ids": [guardian] }),
            ),
            (
                "POST",
                format!("/v1/households/{household}/cards"),
                json!({ "code": "EFGH6789", "client_issued_at": at }),
            ),
            ("POST", format!("/v1/cards/{card}/cancel"), json!({})),
        ];
        for (method, path, body) in writes {
            let (status, answer) = app.send(method, &path, &admin.secret, Some(body)).await;
            assert_eq!(
                (status, answer["error"].as_str()),
                (StatusCode::CONFLICT, Some("event_closed")),
                "{method} {path}"
            );
        }
        let (status, _) = app
            .send("GET", &format!("/v1/people/{adult}"), &admin.secret, None)
            .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = app
            .send(
                "GET",
                &format!("/v1/households/{household}"),
                &admin.secret,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn ensure_open_lets_active_missing_and_absent_events_through() {
        let db = crate::test_support::test_db().await;
        let mut doc = EventDoc::new(
            &Address {
                text: None,
                city: None,
                state: None,
                pending: false,
            },
            store::now(),
        );
        store::replace(&db, COLLECTION, "open", &doc).await.unwrap();
        doc.status = EventStatus::Closed;
        store::replace(&db, COLLECTION, "shut", &doc).await.unwrap();
        assert!(ensure_open(&db, None).await.is_ok());
        assert!(ensure_open(&db, Some("open")).await.is_ok());
        assert!(ensure_open(&db, Some("ghost")).await.is_ok());
        let err = ensure_open(&db, Some("shut")).await.unwrap_err();
        assert_eq!(
            (err.status, err.code),
            (StatusCode::CONFLICT, "event_closed")
        );
    }
}
