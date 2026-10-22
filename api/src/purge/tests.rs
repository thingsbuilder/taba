use firestore::FirestoreInstant;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

use super::domain::{DAY_MICROS, DELETES_PER_TX, day_of, micros_ahead};
use super::model::{DAILY_DOC, DailyDoc, LOCK_DOC, LockDoc, PSEUDONYMS, PseudonymDoc, STATE};
use super::persist::{acquire_lock, daily_doc, run, run_with};
use crate::alerts::{AlertDoc, AlertKind};
use crate::cards::{CardDoc, CardStatus};
use crate::deliveries::{self, DeliveryDoc};
use crate::deliveries::{DeliveryStatus, ItemScope, Verification};
use crate::events::EventStats;
use crate::events::{self, EventDoc, EventStatus};
use crate::exits::fixtures::person;
use crate::households::HouseholdDoc;
use crate::matches::{MatchDoc, MatchStatus, MediatorKind};
use crate::members::{self, MembershipDoc, ShelterRole};
use crate::people::{Level, PersonDoc};
use crate::rescues::RescueDoc;
use crate::searches::{Family, MissingPerson, SearchDoc, SearchStatus, SearchVia};
use crate::shelters;
use crate::store;
use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, new_id};
use crate::volunteers::VolunteerDoc;
use crate::volunteers::{self, GlobalRole};
use crate::{alerts, cards, households, matches, people, rescues, search_photos, searches};

const AUDIT_LOG: &str = "audit_log";

struct World {
    app: TestApp,
    admin: Actor,
    shelter: String,
    event: String,
}

fn ago(days: i64, extra_seconds: i64) -> FirestoreInstant {
    micros_ahead(store::now(), -days * DAY_MICROS - extra_seconds * 1_000_000)
}

fn enable(app: &mut TestApp, on: bool) {
    let mut config = (*app.state.config).clone();
    config.purge_enabled = on;
    app.state.config = Arc::new(config);
}

async fn event_of(app: &TestApp, shelter: &str) -> String {
    let doc: shelters::ShelterDoc = store::get(&app.state.db, shelters::COLLECTION, shelter)
        .await
        .unwrap()
        .unwrap();
    doc.event_id.unwrap()
}

async fn event(app: &TestApp, id: &str) -> EventDoc {
    store::get(&app.state.db, events::COLLECTION, id)
        .await
        .unwrap()
        .unwrap()
}

/// Closes the event; a negative `purge_in_days` puts `purge_at` in the past.
async fn close(app: &TestApp, id: &str, purge_in_days: i64, delivery_in_days: i64) {
    let mut doc = event(app, id).await;
    doc.status = EventStatus::Closed;
    doc.closed_at = Some(ago(31, 0));
    doc.purge_at = Some(micros_ahead(
        store::now(),
        purge_in_days * DAY_MICROS - 1_000_000,
    ));
    doc.delivery_purge_at = Some(micros_ahead(
        store::now(),
        delivery_in_days * DAY_MICROS - 1_000_000,
    ));
    store::replace(&app.state.db, events::COLLECTION, id, &doc)
        .await
        .unwrap();
}

async fn world_with(on: bool) -> World {
    let mut app = TestApp::new().await;
    enable(&mut app, on);
    let admin = app.actor("Admin").await;
    let shelter = app.create_shelter(&admin, "A", ORIGIN).await;
    let event = event_of(&app, &shelter).await;
    World {
        app,
        admin,
        shelter,
        event,
    }
}

async fn world() -> World {
    world_with(true).await
}

async fn put<T: Serialize + serde::de::DeserializeOwned + Sync + Send>(
    app: &TestApp,
    collection: &str,
    id: &str,
    doc: &T,
) {
    store::replace(&app.state.db, collection, id, doc)
        .await
        .unwrap();
}

async fn person_in(w: &World, events: &[&str], household: Option<&str>) -> String {
    let id = new_id();
    let mut doc = person(Level::NoBiometrics, Some(&w.shelter));
    doc.event_ids = events.iter().map(|e| e.to_string()).collect();
    doc.household_id = household.map(str::to_string);
    put(&w.app, people::COLLECTION, &id, &doc).await;
    id
}

async fn household_of(w: &World, members: &[&str], code: Option<&str>) -> String {
    let id = new_id();
    let now = store::now();
    let doc = HouseholdDoc {
        id: None,
        member_ids: members.iter().map(|m| m.to_string()).collect(),
        head_id: None,
        pets: vec![],
        card_code: code.map(str::to_string),
        created_by: w.admin.id.clone(),
        created_shelter_id: w.shelter.clone(),
        client_created_at: now,
        created_at: now,
        updated_at: now,
    };
    put(&w.app, households::COLLECTION, &id, &doc).await;
    if let Some(code) = code {
        let card = CardDoc {
            code: None,
            household_id: id.clone(),
            shelter_id: w.shelter.clone(),
            status: CardStatus::Active,
            issued_by: w.admin.id.clone(),
            client_issued_at: now,
            created_at: now,
            canceled_at: None,
            replaced_by: None,
        };
        put(&w.app, cards::COLLECTION, code, &card).await;
    }
    id
}

async fn delivery_of(w: &World, person: &str, household: &str, code: &str) -> String {
    let id = new_id();
    let now = store::now();
    let doc = DeliveryDoc {
        shelter_id: w.shelter.clone(),
        shelter_name: "A".into(),
        event_id: Some(w.event.clone()),
        person_id: Some(person.into()),
        household_id: Some(household.into()),
        household_pseudonym: None,
        card_code: Some(code.into()),
        item: "water".into(),
        scope: ItemScope::PerPerson,
        quantity: 2,
        delivered_by: w.admin.id.clone(),
        delivered_at: now,
        received_at: now,
        verification: Verification::Card,
        approved_by: Some(w.admin.id.clone()),
        status: DeliveryStatus::Ok,
        reasons: vec![],
        offline_claimed: false,
        delivered_at_clamped: false,
        reviewed_by: Some(w.admin.id.clone()),
        review_note: Some("checked by hand".into()),
        pseudonymized: false,
    };
    put(&w.app, deliveries::COLLECTION, &id, &doc).await;
    id
}

async fn delivery(app: &TestApp, id: &str) -> Option<DeliveryDoc> {
    store::get(&app.state.db, deliveries::COLLECTION, id)
        .await
        .unwrap()
}

async fn alert_in(w: &World) {
    let alert =
        AlertDoc::new(AlertKind::EventOverlap, "event", &w.event, json!({})).in_event(&w.event);
    put(&w.app, alerts::COLLECTION, &new_id(), &alert).await;
}

async fn rescue_in(w: &World) -> String {
    let id = new_id();
    let now = store::now();
    let doc = RescueDoc {
        person_id: new_id(),
        volunteer_id: w.admin.id.clone(),
        device_id: None,
        lat: 0.0,
        lon: 0.0,
        accuracy_m: 5.0,
        landing_point: None,
        captured_at: now,
        received_at: now,
        event_id: Some(w.event.clone()),
        destination_shelter_id: None,
        note: None,
    };
    put(&w.app, rescues::COLLECTION, &id, &doc).await;
    id
}

/// A search of the event with one match; returns `(search_id, match_id)`.
async fn search_in(w: &World) -> (String, String) {
    let (search_id, match_id) = (new_id(), new_id());
    let search = SearchDoc {
        id: None,
        created_by: w.admin.id.clone(),
        via: SearchVia::Volunteer,
        origin_shelter_id: None,
        event_id: Some(w.event.clone()),
        person_ids: vec![new_id()],
        photos_sent: 1,
        usable_photos: 1,
        retained_photos: 0,
        missing_person: MissingPerson {
            name: "Fulano".into(),
            nickname: None,
            notes: String::new(),
        },
        family: Family {
            name: "Familia".into(),
            phone: "+5551999990000".into(),
            relationship: "mother".into(),
            language: "pt-BR".into(),
        },
        status: SearchStatus::Active,
        expiry_notified_at: None,
        created_at: store::now(),
    };
    put(&w.app, searches::COLLECTION, &search_id, &search).await;
    let found = MatchDoc {
        id: None,
        search_id: search_id.clone(),
        person_id: new_id(),
        is_child: false,
        mediator_kind: MediatorKind::Central,
        mediator_shelter_id: None,
        status: MatchStatus::Open,
        decided_by: None,
        decided_by_volunteer: None,
        decided_at: None,
        note: None,
        created_at: store::now(),
    };
    put(&w.app, matches::COLLECTION, &match_id, &found).await;
    (search_id, match_id)
}

async fn exists(app: &TestApp, collection: &str, id: &str) -> bool {
    store::get::<Value>(&app.state.db, collection, id)
        .await
        .unwrap()
        .is_some()
}

async fn count(app: &TestApp, collection: &str) -> usize {
    all_docs(&app.state.db, collection).await.len()
}

async fn audited(app: &TestApp, action: &str) -> Vec<Value> {
    all_docs(&app.state.db, AUDIT_LOG)
        .await
        .into_iter()
        .filter(|e| e["action"] == action)
        .collect()
}

/// Everything one event owns: two people in two households (one card each), two deliveries of
/// the first household and one of the second, a rescue, an alert, a search with a match.
struct Data {
    people: Vec<String>,
    households: Vec<String>,
    cards: Vec<&'static str>,
    deliveries: Vec<String>,
    rescue: String,
    search: (String, String),
}

async fn fill(w: &World) -> Data {
    let (a, b) = (new_id(), new_id());
    let h1 = household_of(w, &[&a], Some("CARD0001")).await;
    let h2 = household_of(w, &[&b], Some("CARD0002")).await;
    for (id, h) in [(&a, &h1), (&b, &h2)] {
        let mut doc = person(Level::Assisted, Some(&w.shelter));
        doc.event_ids = vec![w.event.clone()];
        doc.household_id = Some(h.clone());
        put(&w.app, people::COLLECTION, id, &doc).await;
    }
    let deliveries = vec![
        delivery_of(w, &a, &h1, "CARD0001").await,
        delivery_of(w, &a, &h1, "CARD0001").await,
        delivery_of(w, &b, &h2, "CARD0002").await,
    ];
    alert_in(w).await;
    Data {
        people: vec![a, b],
        households: vec![h1, h2],
        cards: vec!["CARD0001", "CARD0002"],
        deliveries,
        rescue: rescue_in(w).await,
        search: search_in(w).await,
    }
}

// ---- dry run ----

#[tokio::test]
async fn without_the_key_the_pass_only_counts_and_deletes_nothing() {
    let w = world_with(false).await;
    let data = fill(&w).await;
    close(&w.app, &w.event, -1, 60).await;

    let report = run(&w.app.state).await.unwrap();
    assert!(report.dry_run);
    assert!(
        report.deleted > 0 && report.pseudonymized == 3,
        "{report:?}"
    );
    assert_eq!(event(&w.app, &w.event).await.status, EventStatus::Closed);
    assert!(event(&w.app, &w.event).await.stats.is_none());
    for id in &data.people {
        assert!(exists(&w.app, people::COLLECTION, id).await);
    }
    for id in &data.households {
        assert!(exists(&w.app, households::COLLECTION, id).await);
    }
    for code in &data.cards {
        assert!(exists(&w.app, cards::COLLECTION, code).await);
    }
    assert!(exists(&w.app, rescues::COLLECTION, &data.rescue).await);
    assert!(exists(&w.app, searches::COLLECTION, &data.search.0).await);
    assert!(exists(&w.app, matches::COLLECTION, &data.search.1).await);
    assert!(exists(&w.app, shelters::COLLECTION, &w.shelter).await);
    for id in &data.deliveries {
        let doc = delivery(&w.app, id).await.unwrap();
        assert!(doc.person_id.is_some() && !doc.pseudonymized);
    }

    // Audited once a day, with counts and no id of a person.
    run(&w.app.state).await.unwrap();
    let dry = audited(&w.app, "purge.dry_run").await;
    assert_eq!(dry.len(), 1);
    assert_eq!(dry[0]["target_id"], json!(w.event));
    assert_eq!(dry[0]["counts"]["people"], json!(2));
    assert!(audited(&w.app, "purge.event").await.is_empty());
}

#[tokio::test]
async fn the_dry_run_counts_old_data_without_an_event_and_nothing_deletes_it() {
    for enabled in [false, true] {
        let w = world_with(enabled).await;
        let (old, recent) = (ago(31, 0), ago(1, 0));
        let (search, _) = search_in(&w).await;
        let mut doc: SearchDoc = store::get(&w.app.state.db, searches::COLLECTION, &search)
            .await
            .unwrap()
            .unwrap();
        doc.event_id = None;
        doc.created_at = old;
        put(&w.app, searches::COLLECTION, &search, &doc).await;
        let recent_search = new_id();
        doc.created_at = recent;
        put(&w.app, searches::COLLECTION, &recent_search, &doc).await;
        let rescue = rescue_in(&w).await;
        let mut doc: RescueDoc = store::get(&w.app.state.db, rescues::COLLECTION, &rescue)
            .await
            .unwrap()
            .unwrap();
        doc.event_id = None;
        doc.received_at = old;
        put(&w.app, rescues::COLLECTION, &rescue, &doc).await;
        let orphan = person_in(&w, &[], None).await;
        let mut doc: PersonDoc = store::get(&w.app.state.db, people::COLLECTION, &orphan)
            .await
            .unwrap()
            .unwrap();
        doc.updated_at = old;
        put(&w.app, people::COLLECTION, &orphan, &doc).await;
        // A person of an event is not data without an event, however old.
        let owned = person_in(&w, &[&w.event], None).await;
        let mut doc: PersonDoc = store::get(&w.app.state.db, people::COLLECTION, &owned)
            .await
            .unwrap()
            .unwrap();
        doc.updated_at = old;
        put(&w.app, people::COLLECTION, &owned, &doc).await;

        run(&w.app.state).await.unwrap();
        for (collection, id) in [
            (searches::COLLECTION, &search),
            (rescues::COLLECTION, &rescue),
            (people::COLLECTION, &orphan),
        ] {
            assert!(exists(&w.app, collection, id).await);
        }
        let dry = audited(&w.app, "purge.dry_run").await;
        if enabled {
            assert!(dry.is_empty());
            continue;
        }
        assert_eq!(dry.len(), 1);
        assert_eq!(dry[0]["target_id"], json!("no_event"));
        assert_eq!(dry[0]["counts"]["searches_without_event"], json!(1));
        assert_eq!(dry[0]["counts"]["rescues_without_event"], json!(1));
        assert_eq!(dry[0]["counts"]["people_without_event"], json!(1));

        // The second pass of the day does not read them again: with the audit mark cleared, a
        // new old search would show up in a second entry if it did.
        let mut today = daily_doc(
            &w.app.state.db,
            &day_of(store::now(), &w.app.state.config.tz_pilot),
        )
        .await
        .unwrap();
        assert!(today.without_event_counted);
        today.dry_run_logged.clear();
        put(&w.app, STATE, DAILY_DOC, &today).await;
        let mut late: SearchDoc = store::get(&w.app.state.db, searches::COLLECTION, &search)
            .await
            .unwrap()
            .unwrap();
        late.created_at = old;
        put(&w.app, searches::COLLECTION, &new_id(), &late).await;
        run(&w.app.state).await.unwrap();
        assert_eq!(audited(&w.app, "purge.dry_run").await.len(), 1);
    }
}

#[tokio::test]
async fn the_dry_run_counts_what_the_real_run_deletes() {
    let mut w = world_with(false).await;
    fill(&w).await;
    close(&w.app, &w.event, -1, 60).await;
    let dry = run(&w.app.state).await.unwrap();
    enable(&mut w.app, true);
    let real = run(&w.app.state).await.unwrap();
    assert!(!real.dry_run);
    // The real run also deletes the two household pseudonyms it made, which the dry run
    // cannot know.
    assert_eq!(
        (dry.deleted + 2, dry.pseudonymized),
        (real.deleted, real.pseudonymized)
    );
}

// ---- the discard ----

#[tokio::test]
async fn the_discard_deletes_the_event_data_and_pseudonymizes_the_deliveries() {
    let w = world().await;
    let data = fill(&w).await;
    // A document of another event stays.
    let far = w
        .app
        .create_shelter(&w.admin, "Far", (-23.55, -46.63))
        .await;
    let far_event = event_of(&w.app, &far).await;
    let other = person_in(&w, &[&far_event], None).await;
    close(&w.app, &w.event, -1, 60).await;

    let report = run(&w.app.state).await.unwrap();
    assert!(!report.dry_run);
    assert_eq!(report.pseudonymized, 3);

    let kept = event(&w.app, &w.event).await;
    assert_eq!(kept.status, EventStatus::Purged);
    assert!(kept.purged_at.is_some() && kept.purge_at.is_some() && kept.closed_at.is_some());
    assert_eq!(
        kept.stats,
        Some(EventStats {
            shelters: 1,
            unique_people: 2,
            households: 2,
            rescues: 1,
            deliveries: 3
        })
    );
    for id in &data.people {
        assert!(!exists(&w.app, people::COLLECTION, id).await);
    }
    for id in &data.households {
        assert!(!exists(&w.app, households::COLLECTION, id).await);
    }
    for code in &data.cards {
        assert!(!exists(&w.app, cards::COLLECTION, code).await);
    }
    assert!(!exists(&w.app, rescues::COLLECTION, &data.rescue).await);
    assert!(!exists(&w.app, searches::COLLECTION, &data.search.0).await);
    assert!(!exists(&w.app, matches::COLLECTION, &data.search.1).await);
    assert!(!exists(&w.app, shelters::COLLECTION, &w.shelter).await);
    assert_eq!(count(&w.app, alerts::COLLECTION).await, 0);
    let membership = members::membership_id(&w.shelter, &w.admin.id);
    assert!(!exists(&w.app, members::COLLECTION, &membership).await);
    assert_eq!(count(&w.app, PSEUDONYMS).await, 0);
    assert!(exists(&w.app, people::COLLECTION, &other).await);
    assert!(exists(&w.app, shelters::COLLECTION, &far).await);
    assert_eq!(event(&w.app, &far_event).await.status, EventStatus::Active);

    let mut docs = Vec::new();
    for id in &data.deliveries {
        docs.push(delivery(&w.app, id).await.unwrap());
    }
    for d in &docs {
        assert!(d.pseudonymized);
        assert!(d.person_id.is_none() && d.card_code.is_none() && d.household_id.is_none());
        assert!(d.delivered_by.is_empty() && d.approved_by.is_none());
        assert!(d.reviewed_by.is_none() && d.review_note.is_none());
        assert_eq!((d.item.as_str(), d.quantity), ("water", 2));
        assert_eq!(d.shelter_name, "A");
    }
    let (p1, p2, p3) = (
        docs[0].household_pseudonym.clone().unwrap(),
        docs[1].household_pseudonym.clone().unwrap(),
        docs[2].household_pseudonym.clone().unwrap(),
    );
    assert_eq!(p1, p2);
    assert_ne!(p1, p3);
    assert!(!data.households.contains(&p1) && !data.households.contains(&p3));

    // The audit log keeps its entries and gains the discard's, with counts only.
    let entries = audited(&w.app, "purge.event").await;
    assert!(!entries.is_empty());
    let people_deleted: u64 = entries
        .iter()
        .filter_map(|e| e["counts"]["people"].as_u64())
        .sum();
    assert_eq!(people_deleted, 2);
    let text = serde_json::to_string(&entries).unwrap();
    for id in data.people.iter().chain(&data.households) {
        assert!(!text.contains(id.as_str()));
    }

    // A second pass changes nothing and audits nothing more.
    let before = count(&w.app, AUDIT_LOG).await;
    let again = run(&w.app.state).await.unwrap();
    assert_eq!((again.deleted, again.pseudonymized), (0, 0));
    assert_eq!(count(&w.app, AUDIT_LOG).await, before);
}

#[tokio::test]
async fn only_closed_events_past_their_purge_date_are_discarded() {
    let w = world().await;
    let data = fill(&w).await;
    // Active.
    run(&w.app.state).await.unwrap();
    assert!(exists(&w.app, people::COLLECTION, &data.people[0]).await);
    // Closed, but 1 day short of the purge.
    close(&w.app, &w.event, 1, 60).await;
    let report = run(&w.app.state).await.unwrap();
    assert_eq!((report.deleted, report.pseudonymized), (0, 0));
    assert!(exists(&w.app, people::COLLECTION, &data.people[0]).await);
    let doc = delivery(&w.app, &data.deliveries[0]).await.unwrap();
    assert!(doc.person_id.is_some());
    assert_eq!(event(&w.app, &w.event).await.status, EventStatus::Closed);
}

#[tokio::test]
async fn a_person_with_an_expired_event_and_an_active_one_is_kept_until_both_are_due() {
    let w = world().await;
    let far = w
        .app
        .create_shelter(&w.admin, "Far", (-23.55, -46.63))
        .await;
    let far_event = event_of(&w.app, &far).await;
    let both = new_id();
    let household = household_of(&w, &[&both], Some("CARD0009")).await;
    let mut doc = person(Level::Assisted, Some(&w.shelter));
    doc.event_ids = vec![w.event.clone(), far_event.clone()];
    doc.household_id = Some(household.clone());
    put(&w.app, people::COLLECTION, &both, &doc).await;
    let only = person_in(&w, &[&w.event], None).await;
    close(&w.app, &w.event, -1, 60).await;

    run(&w.app.state).await.unwrap();

    assert_eq!(event(&w.app, &w.event).await.status, EventStatus::Purged);
    assert!(!exists(&w.app, people::COLLECTION, &only).await);
    assert!(exists(&w.app, people::COLLECTION, &both).await);
    assert!(exists(&w.app, households::COLLECTION, &household).await);
    assert!(exists(&w.app, cards::COLLECTION, "CARD0009").await);

    // The other event closes and is due: now the person, the household and the card go.
    close(&w.app, &far_event, -1, 60).await;
    run(&w.app.state).await.unwrap();
    assert!(!exists(&w.app, people::COLLECTION, &both).await);
    assert!(!exists(&w.app, households::COLLECTION, &household).await);
    assert!(!exists(&w.app, cards::COLLECTION, "CARD0009").await);
    assert_eq!(event(&w.app, &far_event).await.status, EventStatus::Purged);
}

#[tokio::test]
async fn a_person_with_an_event_that_is_closed_but_not_due_is_kept() {
    let w = world().await;
    let far = w
        .app
        .create_shelter(&w.admin, "Far", (-23.55, -46.63))
        .await;
    let far_event = event_of(&w.app, &far).await;
    let both = person_in(&w, &[&w.event, &far_event], None).await;
    close(&w.app, &w.event, -1, 60).await;
    close(&w.app, &far_event, 5, 60).await;
    run(&w.app.state).await.unwrap();
    assert!(exists(&w.app, people::COLLECTION, &both).await);
}

#[tokio::test]
async fn the_discard_takes_the_family_photos_of_the_event_searches() {
    let w = world().await;
    let (search, _) = search_in(&w).await;
    let photo = search_photos::PhotoDoc {
        search_id: search.clone(),
        photo_encrypted: Some(vec![1, 2, 3]),
        created_at: store::now(),
    };
    put(
        &w.app,
        search_photos::COLLECTION,
        &search_photos::photo_id(&search, 0),
        &photo,
    )
    .await;
    close(&w.app, &w.event, -1, 60).await;
    run(&w.app.state).await.unwrap();
    assert_eq!(count(&w.app, search_photos::COLLECTION).await, 0);
    assert!(!exists(&w.app, searches::COLLECTION, &search).await);
}

#[tokio::test]
async fn a_resumed_pass_gives_a_household_the_pseudonym_it_already_has() {
    let w = world().await;
    let data = fill(&w).await;
    let known = PseudonymDoc {
        event_id: w.event.clone(),
        pseudonym: "already-given".into(),
    };
    put(&w.app, PSEUDONYMS, &data.households[0], &known).await;
    close(&w.app, &w.event, -1, 60).await;
    run(&w.app.state).await.unwrap();
    for id in &data.deliveries[..2] {
        let doc = delivery(&w.app, id).await.unwrap();
        assert_eq!(doc.household_pseudonym.as_deref(), Some("already-given"));
    }
    assert_eq!(count(&w.app, PSEUDONYMS).await, 0);
}

// ---- batches, daily cap, resume, concurrency ----

#[tokio::test]
async fn more_than_one_batch_of_documents_is_deleted() {
    let w = world().await;
    for _ in 0..450 {
        alert_in(&w).await;
    }
    close(&w.app, &w.event, -1, 60).await;
    let report = run(&w.app.state).await.unwrap();
    assert_eq!(count(&w.app, alerts::COLLECTION).await, 0);
    // The alerts, the membership and the shelter.
    assert_eq!(report.deleted, 450 + 2);
    let batches = audited(&w.app, "purge.event").await;
    assert!(batches.len() >= 3, "{}", batches.len());
    for entry in &batches {
        let n: u64 = entry["counts"]
            .as_object()
            .unwrap()
            .values()
            .filter_map(Value::as_u64)
            .sum();
        assert!(n as usize <= DELETES_PER_TX);
    }
    assert_eq!(event(&w.app, &w.event).await.status, EventStatus::Purged);
}

async fn forget_today(app: &TestApp) {
    let mut today: DailyDoc = store::get(&app.state.db, STATE, DAILY_DOC)
        .await
        .unwrap()
        .unwrap();
    today.day = "2000-01-01".into();
    put(app, STATE, DAILY_DOC, &today).await;
}

#[tokio::test]
async fn the_pass_stops_at_the_daily_cap_and_the_next_day_resumes() {
    let w = world().await;
    for _ in 0..450 {
        alert_in(&w).await;
    }
    close(&w.app, &w.event, -1, 60).await;

    let first = run_with(&w.app.state, 300).await.unwrap();
    assert_eq!(first.deleted, 300);
    assert_eq!(count(&w.app, alerts::COLLECTION).await, 150);
    assert_eq!(event(&w.app, &w.event).await.status, EventStatus::Closed);
    let today: DailyDoc = store::get(&w.app.state.db, STATE, DAILY_DOC)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(today.deleted, 300);
    assert_eq!(
        today.day,
        day_of(store::now(), &w.app.state.config.tz_pilot)
    );

    // The next hour of the same day does nothing.
    let same_day = run_with(&w.app.state, 300).await.unwrap();
    assert_eq!(same_day.deleted, 0);
    assert_eq!(count(&w.app, alerts::COLLECTION).await, 150);

    // The next day it finishes the event, and the counter starts again.
    forget_today(&w.app).await;
    let next = run_with(&w.app.state, 300).await.unwrap();
    assert_eq!(next.deleted, 152);
    assert_eq!(count(&w.app, alerts::COLLECTION).await, 0);
    assert_eq!(event(&w.app, &w.event).await.status, EventStatus::Purged);
    let today: DailyDoc = store::get(&w.app.state.db, STATE, DAILY_DOC)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(today.deleted, 152);
}

#[tokio::test]
async fn a_pass_stopped_halfway_is_finished_by_the_next_one() {
    let w = world().await;
    let data = fill(&w).await;
    close(&w.app, &w.event, -1, 60).await;
    // A cap of 4 stops it right after the search, its match and two more documents.
    let first = run_with(&w.app.state, 4).await.unwrap();
    assert_eq!(first.deleted, 4);
    assert_eq!(event(&w.app, &w.event).await.status, EventStatus::Closed);
    // The deliveries were already pseudonymized and the stats are on the event.
    assert!(event(&w.app, &w.event).await.stats.is_some());
    for id in &data.deliveries {
        assert!(delivery(&w.app, id).await.unwrap().pseudonymized);
    }
    forget_today(&w.app).await;
    run(&w.app.state).await.unwrap();
    assert_eq!(event(&w.app, &w.event).await.status, EventStatus::Purged);
    for id in &data.people {
        assert!(!exists(&w.app, people::COLLECTION, id).await);
    }
    assert!(!exists(&w.app, shelters::COLLECTION, &w.shelter).await);
    let docs = [
        delivery(&w.app, &data.deliveries[0]).await.unwrap(),
        delivery(&w.app, &data.deliveries[1]).await.unwrap(),
    ];
    assert_eq!(docs[0].household_pseudonym, docs[1].household_pseudonym);
}

#[tokio::test]
async fn a_pass_does_not_run_while_another_holds_the_lease() {
    let w = world().await;
    let data = fill(&w).await;
    close(&w.app, &w.event, -1, 60).await;
    assert!(acquire_lock(&w.app.state.db).await.unwrap());
    let report = run(&w.app.state).await.unwrap();
    assert_eq!((report.deleted, report.pseudonymized), (0, 0));
    assert!(exists(&w.app, people::COLLECTION, &data.people[0]).await);
    // A lease left by a pass that died expires.
    let stale = LockDoc {
        locked_until: ago(0, 1),
    };
    put(&w.app, STATE, LOCK_DOC, &stale).await;
    let report = run(&w.app.state).await.unwrap();
    assert!(report.deleted > 0);
    // And the pass let it go.
    assert!(acquire_lock(&w.app.state.db).await.unwrap());
}

#[tokio::test]
async fn two_passes_at_once_discard_the_event_once() {
    let w = world().await;
    let data = fill(&w).await;
    for _ in 0..30 {
        alert_in(&w).await;
    }
    close(&w.app, &w.event, -1, 60).await;
    let (a, b) = tokio::join!(run(&w.app.state), run(&w.app.state));
    let (a, b) = (a.unwrap(), b.unwrap());
    // Whoever got the lease did everything; the other did nothing.
    // 2 people, 2 households, 2 cards, 1 rescue, 1 search, 1 match, 31 alerts, the
    // membership, the shelter and the 2 pseudonyms.
    assert_eq!(
        a.deleted + b.deleted,
        2 + 2 + 2 + 1 + 1 + 1 + 31 + 1 + 1 + 2
    );
    assert_eq!(a.pseudonymized + b.pseudonymized, 3);
    let finished: Vec<Value> = audited(&w.app, "purge.event")
        .await
        .into_iter()
        .filter(|e| e["counts"]["events_purged"] == json!(1))
        .collect();
    assert_eq!(finished.len(), 1);
    assert_eq!(event(&w.app, &w.event).await.status, EventStatus::Purged);
    assert!(!exists(&w.app, people::COLLECTION, &data.people[0]).await);
}

// ---- delivery retention ----

#[tokio::test]
async fn pseudonymized_deliveries_go_at_the_delivery_purge_date_and_only_those() {
    let w = world().await;
    let data = fill(&w).await;
    close(&w.app, &w.event, -1, 60).await;
    run(&w.app.state).await.unwrap();
    for id in &data.deliveries {
        assert!(delivery(&w.app, id).await.is_some());
    }

    // 90 days after closing: the pseudonymized deliveries are deleted.
    let mut doc = event(&w.app, &w.event).await;
    doc.delivery_purge_at = Some(ago(0, 5));
    put(&w.app, events::COLLECTION, &w.event, &doc).await;
    // One that was never pseudonymized (written after the discard) is not touched.
    let late = delivery_of(&w, "p", "h", "CARDLATE").await;
    run(&w.app.state).await.unwrap();
    for id in &data.deliveries {
        assert!(delivery(&w.app, id).await.is_none());
    }
    assert!(delivery(&w.app, &late).await.is_some());
    assert!(!audited(&w.app, "purge.deliveries").await.is_empty());
}

#[tokio::test]
async fn a_closed_event_before_its_delivery_date_keeps_its_deliveries() {
    let w = world().await;
    let data = fill(&w).await;
    close(&w.app, &w.event, 5, 60).await;
    run(&w.app.state).await.unwrap();
    for id in &data.deliveries {
        assert!(delivery(&w.app, id).await.is_some());
    }
    // Past the delivery date but before the purge: not pseudonymized yet, so not deleted.
    close(&w.app, &w.event, 5, -1).await;
    run(&w.app.state).await.unwrap();
    for id in &data.deliveries {
        let doc = delivery(&w.app, id).await.unwrap();
        assert!(!doc.pseudonymized && doc.person_id.is_some());
    }
}

// ---- volunteers ----

async fn last_seen(app: &TestApp, id: &str, days: i64) {
    let mut doc: VolunteerDoc = store::get(&app.state.db, volunteers::COLLECTION, id)
        .await
        .unwrap()
        .unwrap();
    doc.last_seen_at = ago(days, 0);
    put(app, volunteers::COLLECTION, id, &doc).await;
}

#[tokio::test]
async fn volunteer_profiles_go_a_year_after_the_last_visit_with_their_exceptions() {
    let w = world().await;
    let mut joined = Vec::new();
    for name in ["Gone", "Recent", "Banned", "Validator", "Helper"] {
        let code = w.app.invite(&w.admin, &w.shelter, "qr", "volunteer").await;
        joined.push(w.app.join(&code, name).await);
    }
    let [gone, recent, banned, validator, helper] = <[Actor; 5]>::try_from(joined).ok().unwrap();
    let db = &w.app.state.db;

    let mut doc: VolunteerDoc = store::get(db, volunteers::COLLECTION, &banned.id)
        .await
        .unwrap()
        .unwrap();
    doc.banned = true;
    put(&w.app, volunteers::COLLECTION, &banned.id, &doc).await;
    let mut doc: VolunteerDoc = store::get(db, volunteers::COLLECTION, &validator.id)
        .await
        .unwrap()
        .unwrap();
    doc.global_roles = vec![GlobalRole::Validator];
    put(&w.app, volunteers::COLLECTION, &validator.id, &doc).await;
    for (who, days) in [
        (&gone, 366),
        (&recent, 364),
        (&banned, 400),
        (&validator, 400),
        (&w.admin, 400),
        (&helper, 366),
    ] {
        last_seen(&w.app, &who.id, days).await;
    }
    run(&w.app.state).await.unwrap();
    // The only admin of an open shelter stays, as do banned, validator and recent.
    for (who, kept) in [
        (&gone, false),
        (&recent, true),
        (&banned, true),
        (&validator, true),
        (&w.admin, true),
        (&helper, false),
    ] {
        assert_eq!(
            exists(&w.app, volunteers::COLLECTION, &who.id).await,
            kept,
            "{}",
            who.id
        );
    }
    // The memberships of a deleted profile go with it.
    let gone_membership = members::membership_id(&w.shelter, &gone.id);
    assert!(!exists(&w.app, members::COLLECTION, &gone_membership).await);
    let recent_membership = members::membership_id(&w.shelter, &recent.id);
    assert!(exists(&w.app, members::COLLECTION, &recent_membership).await);
    assert!(!audited(&w.app, "purge.volunteers").await.is_empty());

    // With a second admin, the creator is no longer the only one and can go.
    let mut membership: MembershipDoc = store::get(db, members::COLLECTION, &recent_membership)
        .await
        .unwrap()
        .unwrap();
    membership.role = ShelterRole::Admin;
    put(&w.app, members::COLLECTION, &recent_membership, &membership).await;
    run(&w.app.state).await.unwrap();
    assert!(!exists(&w.app, volunteers::COLLECTION, &w.admin.id).await);
    assert!(exists(&w.app, volunteers::COLLECTION, &recent.id).await);
}

#[tokio::test]
async fn volunteer_profiles_are_only_counted_without_the_key() {
    let w = world_with(false).await;
    last_seen(&w.app, &w.admin.id, 400).await;
    let other = w.app.actor("Other").await;
    last_seen(&w.app, &other.id, 400).await;
    let report = run(&w.app.state).await.unwrap();
    assert!(report.dry_run);
    assert_eq!(report.deleted, 1, "only the one who is not a sole admin");
    assert!(exists(&w.app, volunteers::COLLECTION, &other.id).await);
}
