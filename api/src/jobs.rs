//! The hourly task of `serve` (spec 6.2.6 items 1, 2, 4): closes events, raises `search_expiring`,
//! deletes thumbnails, expires captures, invites and endorsement codes. Every step is idempotent
//! (it reads what is still to do inside its own transaction), so a run that repeats or overlaps
//! another one changes nothing twice. The discard at `purge_at` is `purge.rs`.

use std::collections::HashSet;
use std::time::Duration;

use firestore::{FirestoreDb, FirestoreInstant, FirestoreTimestamp, path};
use serde::Deserialize;
use serde_json::json;

use crate::alerts::{self, AlertDoc, AlertKind};
use crate::audit::{self, AuditEntry};
use crate::auth::AppState;
use crate::endorsements;
use crate::error::ApiError;
use crate::events::{self, ClosureReason, EventDoc, EventStatus};
use crate::invites::{self, InviteStatus};
use crate::matches::{self, MatchDoc, MatchStatus};
use crate::people::{self, PersonDoc};
use crate::searches::{self, SearchDoc, SearchStatus};
use crate::shelters::{self, ShelterDoc, ShelterStatus};
use crate::store::{self, fs_error, tx_delete, tx_replace};

const INTERVAL: Duration = Duration::from_secs(3600);
/// Writes of one transaction (Firestore takes 500).
const BATCH_WRITES: usize = 400;
const DAY_MICROS: i64 = 86_400_000_000;

/// Starts the loop; `serve` calls it once.
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(INTERVAL);
        loop {
            tick.tick().await;
            if let Err(err) = run_once(&state).await {
                tracing::error!(code = err.code, "hourly task failed");
            }
        }
    });
}

/// One pass of the task; tests call it directly. A step that fails does not stop the others;
/// the first failure is returned.
pub async fn run_once(state: &AppState) -> Result<(), ApiError> {
    let steps = [
        close_events(state).await,
        notify_expiring_searches(state).await,
        delete_thumbnails(state).await,
        delete_expired(state).await,
        crate::purge::run(state).await.map(|_| ()),
    ];
    steps.into_iter().collect()
}

/// A document with its id, for queries over types that do not carry one.
#[derive(Deserialize)]
struct Listed<T> {
    #[serde(alias = "_firestore_id")]
    id: String,
    #[serde(flatten)]
    doc: T,
}

fn plus_days(at: FirestoreInstant, days: u32) -> FirestoreInstant {
    FirestoreInstant::from_microsecond(at.as_microsecond() + i64::from(days) * DAY_MICROS)
        .expect("a date within range")
}

fn minus_days(at: FirestoreInstant, days: u32) -> FirestoreInstant {
    FirestoreInstant::from_microsecond(at.as_microsecond() - i64::from(days) * DAY_MICROS)
        .expect("a date within range")
}

async fn events_with(
    db: &FirestoreDb,
    status: EventStatus,
) -> Result<Vec<Listed<EventDoc>>, ApiError> {
    db.fluent()
        .select()
        .from(events::COLLECTION)
        .filter(|q| q.for_all([q.field(path!(EventDoc::status)).eq(status)]))
        .obj()
        .query()
        .await
        .map_err(fs_error)
}

// ---- 1. close events (spec 4.3 item 4) ----

/// Why an active event is due to close: all its shelters are closed, or no write for
/// `inactivity_days`. Reads through `db`, so inside a transaction the shelters belong to it.
async fn closure_reason(
    db: &FirestoreDb,
    event_id: &str,
    event: &EventDoc,
    now: FirestoreInstant,
    inactivity_days: u32,
) -> Result<Option<ClosureReason>, ApiError> {
    let shelters: Vec<ShelterDoc> = db
        .fluent()
        .select()
        .from(shelters::COLLECTION)
        .filter(|q| q.for_all([q.field(path!(ShelterDoc::event_id)).eq(event_id)]))
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    if !shelters.is_empty() && shelters.iter().all(|s| s.status == ShelterStatus::Closed) {
        return Ok(Some(ClosureReason::AllSheltersClosed));
    }
    let idle = now.as_microsecond() - event.last_activity_at.as_microsecond();
    Ok((idle >= i64::from(inactivity_days) * DAY_MICROS).then_some(ClosureReason::Inactivity))
}

async fn close_events(state: &AppState) -> Result<(), ApiError> {
    let (inactivity_days, purge_days, delivery_days) = (
        state.config.inactivity_days,
        state.config.purge_after_days,
        state.config.delivery_retention_days,
    );
    // ponytail: every active event is listed each hour; fine for a pilot's handful of events.
    for event in events_with(&state.db, EventStatus::Active).await? {
        store::transact(&state.db, |db, tx| {
            let id = event.id.clone();
            Box::pin(async move {
                let Some(mut event) = store::get::<EventDoc>(&db, events::COLLECTION, &id).await?
                else {
                    return Ok(());
                };
                let now = store::now();
                if event.status != EventStatus::Active {
                    return Ok(());
                }
                let Some(reason) = closure_reason(&db, &id, &event, now, inactivity_days).await?
                else {
                    return Ok(());
                };
                event.status = EventStatus::Closed;
                event.closed_at = Some(now);
                event.closure_reason = Some(reason);
                event.purge_at = Some(plus_days(now, purge_days));
                event.delivery_purge_at = Some(plus_days(now, delivery_days));
                tx_replace(&db, tx, events::COLLECTION, &id, &event)?;
                audit::add(&db, tx, &AuditEntry::new("event.close", "event", &id))
            })
        })
        .await?;
    }
    Ok(())
}

// ---- 1b. search_expiring (spec 3.7 item 9) ----

async fn has_accepted_match(db: &FirestoreDb, search_id: &str) -> Result<bool, ApiError> {
    let accepted: Vec<MatchDoc> = db
        .fluent()
        .select()
        .from(matches::COLLECTION)
        .filter(|q| {
            q.for_all([
                q.field(path!(MatchDoc::search_id)).eq(search_id),
                q.field(path!(MatchDoc::status)).eq(MatchStatus::Accepted),
            ])
        })
        .limit(1)
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    Ok(!accepted.is_empty())
}

/// Tells the volunteer of every active search without an accepted match, once, that the event
/// closed and when its data goes away. It looks at every closed event, so a run that stopped
/// halfway is finished by the next one.
// ponytail: the active searches of closed events are read each hour until their purge; a search
// whose match was accepted is checked every time. Fine for a pilot; close the search when its
// match is accepted to take it out of this query.
async fn notify_expiring_searches(state: &AppState) -> Result<(), ApiError> {
    for event in events_with(&state.db, EventStatus::Closed).await? {
        let Some(purge_at) = event.doc.purge_at else {
            continue;
        };
        let active: Vec<SearchDoc> = state
            .db
            .fluent()
            .select()
            .from(searches::COLLECTION)
            .filter(|q| {
                q.for_all([
                    q.field(path!(SearchDoc::event_id)).eq(event.id.as_str()),
                    q.field(path!(SearchDoc::status)).eq(SearchStatus::Active),
                ])
            })
            .obj()
            .query()
            .await
            .map_err(fs_error)?;
        let pending: Vec<String> = active
            .into_iter()
            .filter(|s| s.expiry_notified_at.is_none())
            .filter_map(|s| s.id)
            .collect();
        // A search and its alert: two writes.
        for chunk in pending.chunks(BATCH_WRITES / 2) {
            store::transact(&state.db, |db, tx| {
                let (chunk, event_id) = (chunk.to_vec(), event.id.clone());
                Box::pin(async move {
                    for search_id in chunk {
                        let Some(mut search) =
                            store::get::<SearchDoc>(&db, searches::COLLECTION, &search_id).await?
                        else {
                            continue;
                        };
                        if search.status != SearchStatus::Active
                            || search.expiry_notified_at.is_some()
                            || has_accepted_match(&db, &search_id).await?
                        {
                            continue;
                        }
                        search.expiry_notified_at = Some(store::now());
                        tx_replace(&db, tx, searches::COLLECTION, &search_id, &search)?;
                        let alert = AlertDoc::personal(
                            AlertKind::SearchExpiring,
                            "search",
                            &search_id,
                            &search.created_by,
                            json!({ "purge_at": purge_at.to_string() }),
                        )
                        .in_event(&event_id);
                        alerts::add(&db, tx, &alert)?;
                    }
                    Ok(())
                })
            })
            .await?;
        }
    }
    Ok(())
}

// ---- 2. thumbnails (spec 4.6) ----

/// Whether every event in the list is closed; read through `db`. A person with no event yet
/// (a rescue without shelter) is not decided here: see `orphan_expired`.
async fn all_events_closed(db: &FirestoreDb, event_ids: &[String]) -> Result<bool, ApiError> {
    for id in event_ids {
        let event: Option<EventDoc> = store::get(db, events::COLLECTION, id).await?;
        if event.is_some_and(|e| e.status == EventStatus::Active) {
            return Ok(false);
        }
    }
    Ok(!event_ids.is_empty())
}

#[derive(Deserialize)]
struct PersonEvents {
    #[serde(default)]
    event_ids: Vec<String>,
    #[serde(default)]
    current_shelter_id: Option<String>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    updated_at: FirestoreInstant,
}

/// A person with no event and no shelter (a rescue the landing point never sent anywhere) has no
/// event whose end would delete the thumbnail: it goes `PURGE_AFTER_DAYS` after the last change
/// to the person (spec 4.6), which is as long as an event keeps it after it closes.
fn orphan_expired(
    event_ids: &[String],
    current_shelter_id: Option<&str>,
    updated_at: FirestoreInstant,
    cutoff: FirestoreInstant,
) -> bool {
    event_ids.is_empty() && current_shelter_id.is_none() && updated_at < cutoff
}

/// Deletes the thumbnail of every person whose events are all closed.
// ponytail: every person with a thumbnail is listed each hour (their event ids only); fine while
// the unidentified people of the open events number in the hundreds.
async fn delete_thumbnails(state: &AppState) -> Result<(), ApiError> {
    let active: HashSet<String> = events_with(&state.db, EventStatus::Active)
        .await?
        .into_iter()
        .map(|e| e.id)
        .collect();
    let with_thumbnail: Vec<Listed<PersonEvents>> = state
        .db
        .fluent()
        .select()
        .fields(["event_ids", "current_shelter_id", "updated_at"])
        .from(people::COLLECTION)
        .filter(|q| q.for_all([q.field(path!(PersonDoc::thumbnail_encrypted)).is_not_null()]))
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    let cutoff = minus_days(store::now(), state.config.purge_after_days);
    let due: Vec<String> = with_thumbnail
        .into_iter()
        .filter(|p| {
            (!p.doc.event_ids.is_empty() && p.doc.event_ids.iter().all(|e| !active.contains(e)))
                || orphan_expired(
                    &p.doc.event_ids,
                    p.doc.current_shelter_id.as_deref(),
                    p.doc.updated_at,
                    cutoff,
                )
        })
        .map(|p| p.id)
        .collect();
    // A person and the audit entry: two writes.
    for chunk in due.chunks(BATCH_WRITES / 2) {
        store::transact(&state.db, |db, tx| {
            let chunk = chunk.to_vec();
            Box::pin(async move {
                for person_id in chunk {
                    let Some(mut person) =
                        store::get::<PersonDoc>(&db, people::COLLECTION, &person_id).await?
                    else {
                        continue;
                    };
                    // The person may have checked in to a new event since the listing.
                    if person.thumbnail_encrypted.is_none() {
                        continue;
                    }
                    let orphan = orphan_expired(
                        &person.event_ids,
                        person.current_shelter_id.as_deref(),
                        person.updated_at,
                        cutoff,
                    );
                    if !orphan && !all_events_closed(&db, &person.event_ids).await? {
                        continue;
                    }
                    person.thumbnail_encrypted = None;
                    person.updated_at = store::now();
                    tx_replace(&db, tx, people::COLLECTION, &person_id, &person)?;
                    audit::add(
                        &db,
                        tx,
                        &AuditEntry::new("person.thumbnail_purge", "person", &person_id),
                    )?;
                }
                Ok(())
            })
        })
        .await?;
    }
    Ok(())
}

// ---- 3. expired captures, invites and endorsement codes (spec 6.2.6 item 4) ----

/// What the expiry needs of a document of any of the three collections.
#[derive(Deserialize)]
struct Expiring {
    #[serde(alias = "_firestore_id", default)]
    id: Option<String>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    expires_at: FirestoreInstant,
    #[serde(default)]
    used: bool,
    #[serde(default)]
    status: Option<String>,
}

/// Deletes the listed documents that are still expired and still unused when read inside the
/// transaction, `BATCH_WRITES` at a time. A used invite or endorsement code is kept as a record.
async fn delete_if_expired(
    state: &AppState,
    collection: &'static str,
    ids: Vec<String>,
    keep: fn(&Expiring) -> bool,
) -> Result<(), ApiError> {
    for chunk in ids.chunks(BATCH_WRITES) {
        store::transact(&state.db, |db, tx| {
            let chunk = chunk.to_vec();
            Box::pin(async move {
                let now = store::now();
                for id in chunk {
                    let doc: Option<Expiring> = store::get(&db, collection, &id).await?;
                    if doc.is_some_and(|d| d.expires_at <= now && !keep(&d)) {
                        tx_delete(&db, tx, collection, &id)?;
                    }
                }
                Ok(())
            })
        })
        .await?;
    }
    Ok(())
}

fn expired_ids(listed: Vec<Expiring>, now: FirestoreInstant) -> Vec<String> {
    listed
        .into_iter()
        .filter(|d| d.expires_at <= now)
        .filter_map(|d| d.id)
        .collect()
}

async fn delete_expired(state: &AppState) -> Result<(), ApiError> {
    let now = store::now();
    // Captures: the Firestore TTL also removes them, some time after `expires_at`.
    let captures: Vec<Expiring> = state
        .db
        .fluent()
        .select()
        .from("captures")
        .filter(|q| q.for_all([q.field("expires_at").less_than(FirestoreTimestamp(now))]))
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    delete_if_expired(state, "captures", expired_ids(captures, now), |_| false).await?;

    let invites: Vec<Expiring> = state
        .db
        .fluent()
        .select()
        .from(invites::COLLECTION)
        .filter(|q| q.for_all([q.field("status").eq(InviteStatus::Active)]))
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    delete_if_expired(state, invites::COLLECTION, expired_ids(invites, now), |d| {
        d.status.as_deref() != Some("active")
    })
    .await?;

    let codes: Vec<Expiring> = state
        .db
        .fluent()
        .select()
        .from(endorsements::COLLECTION)
        .filter(|q| q.for_all([q.field("used").eq(false)]))
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    delete_if_expired(
        state,
        endorsements::COLLECTION,
        expired_ids(codes, now),
        |d| d.used,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{CaptureDoc, CaptureStatus, Purpose};
    use crate::endorsements::EndorsementCodeDoc;
    use crate::exits::fixtures::{person, put};
    use crate::invites::{InviteDoc, InviteKind};
    use crate::members::ShelterRole;
    use crate::people::Level;
    use crate::searches::{Family, MissingPerson, SearchVia};
    use crate::test_support::{
        Actor, ORIGIN, TestApp, all_docs, audit_actions, new_id, north_of_origin,
    };
    use axum::http::StatusCode;
    use serde_json::Value;

    struct World {
        app: TestApp,
        admin: Actor,
        shelter: String,
        event: String,
    }

    async fn world() -> World {
        let app = TestApp::new().await;
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

    async fn event_of(app: &TestApp, shelter: &str) -> String {
        let doc: ShelterDoc = store::get(&app.state.db, shelters::COLLECTION, shelter)
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

    fn ago(days: i64, extra_seconds: i64) -> FirestoreInstant {
        let micros = store::now().as_microsecond() - days * DAY_MICROS - extra_seconds * 1_000_000;
        FirestoreInstant::from_microsecond(micros).unwrap()
    }

    async fn idle_for(app: &TestApp, event_id: &str, days: i64) {
        let mut doc = event(app, event_id).await;
        doc.last_activity_at = ago(days, 0);
        store::replace(&app.state.db, events::COLLECTION, event_id, &doc)
            .await
            .unwrap();
    }

    async fn close_shelter(w: &World, shelter: &str) {
        let (status, body) = w
            .app
            .send(
                "POST",
                &format!("/v1/shelters/{shelter}/close"),
                &w.admin.secret,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    async fn search(app: &TestApp, event_id: &str, owner: &str) -> String {
        let id = new_id();
        let doc = SearchDoc {
            id: None,
            created_by: owner.into(),
            via: SearchVia::Volunteer,
            origin_shelter_id: None,
            event_id: Some(event_id.into()),
            person_ids: vec![],
            photos_sent: 1,
            usable_photos: 1,
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
        store::replace(&app.state.db, searches::COLLECTION, &id, &doc)
            .await
            .unwrap();
        id
    }

    async fn match_of(app: &TestApp, search_id: &str, status: MatchStatus) {
        let doc = MatchDoc {
            id: None,
            search_id: search_id.into(),
            person_id: new_id(),
            is_child: false,
            mediator_kind: matches::MediatorKind::Central,
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

    async fn expiring_alerts(app: &TestApp) -> Vec<Value> {
        all_docs(&app.state.db, alerts::COLLECTION)
            .await
            .into_iter()
            .filter(|a| a["kind"] == "search_expiring")
            .collect()
    }

    async fn close_count(app: &TestApp) -> usize {
        audit_actions(&app.state.db)
            .await
            .iter()
            .filter(|a| *a == "event.close")
            .count()
    }

    async fn thumbnail_person(app: &TestApp, events: &[&str]) -> String {
        let id = new_id();
        let mut doc = person(Level::NoBiometrics, None);
        doc.thumbnail_encrypted = Some(vec![1, 2, 3]);
        doc.event_ids = events.iter().map(|e| e.to_string()).collect();
        put(app, &id, &doc).await;
        id
    }

    async fn has_thumbnail(app: &TestApp, id: &str) -> bool {
        let doc: PersonDoc = store::get(&app.state.db, people::COLLECTION, id)
            .await
            .unwrap()
            .unwrap();
        doc.thumbnail_encrypted.is_some()
    }

    #[tokio::test]
    async fn an_event_without_writes_for_30_days_closes_with_its_purge_date() {
        let w = world().await;
        let quiet_shelter = w
            .app
            .create_shelter(&w.admin, "Far", (-23.55, -46.63))
            .await;
        let quiet = event_of(&w.app, &quiet_shelter).await;
        assert_ne!(quiet, w.event);
        idle_for(&w.app, &quiet, 31).await;
        idle_for(&w.app, &w.event, 29).await;

        run_once(&w.app.state).await.unwrap();

        let closed = event(&w.app, &quiet).await;
        let closed_at = closed.closed_at.unwrap();
        assert_eq!(
            (closed.status, closed.closure_reason),
            (EventStatus::Closed, Some(ClosureReason::Inactivity))
        );
        assert_eq!(
            closed.purge_at,
            Some(plus_days(closed_at, w.app.state.config.purge_after_days))
        );
        assert_eq!(
            closed.delivery_purge_at,
            Some(plus_days(
                closed_at,
                w.app.state.config.delivery_retention_days
            ))
        );
        assert!((store::now().as_second() - closed_at.as_second()).abs() < 60);
        let open = event(&w.app, &w.event).await;
        assert_eq!(
            (open.status, open.closed_at, open.purge_at),
            (EventStatus::Active, None, None)
        );
        assert_eq!(close_count(&w.app).await, 1);

        // The closed event takes no more writes; the open one does.
        let checkin = |shelter: &str| {
            let body = json!({ "id": new_id(), "legal_basis": "consent", "client_created_at": store::now().to_string(), "data": {}, "no_biometrics": true });
            (format!("/v1/shelters/{shelter}/checkins"), body)
        };
        let (path, body) = checkin(&quiet_shelter);
        let (status, answer) = w.app.send("POST", &path, &w.admin.secret, Some(body)).await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::CONFLICT, Some("event_closed"))
        );
        let (path, body) = checkin(&w.shelter);
        let (status, _) = w.app.send("POST", &path, &w.admin.secret, Some(body)).await;
        assert_eq!(status, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn an_event_closes_when_all_its_shelters_are_closed_and_not_before() {
        let w = world().await;
        let second = w
            .app
            .create_shelter(&w.admin, "B", north_of_origin(5_000.0))
            .await;
        assert_eq!(event_of(&w.app, &second).await, w.event);

        close_shelter(&w, &w.shelter).await;
        run_once(&w.app.state).await.unwrap();
        assert_eq!(event(&w.app, &w.event).await.status, EventStatus::Active);

        close_shelter(&w, &second).await;
        run_once(&w.app.state).await.unwrap();
        let closed = event(&w.app, &w.event).await;
        assert_eq!(
            (closed.status, closed.closure_reason),
            (EventStatus::Closed, Some(ClosureReason::AllSheltersClosed))
        );
        assert!(closed.closed_at.is_some() && closed.purge_at.is_some());
    }

    #[tokio::test]
    async fn a_second_run_changes_nothing_and_raises_no_second_alert() {
        let w = world().await;
        let owner = w.app.actor("Owner").await;
        let first = search(&w.app, &w.event, &owner.id).await;
        let second = search(&w.app, &w.event, &owner.id).await;
        idle_for(&w.app, &w.event, 31).await;

        run_once(&w.app.state).await.unwrap();
        let closed = event(&w.app, &w.event).await;
        assert_eq!(expiring_alerts(&w.app).await.len(), 2);

        run_once(&w.app.state).await.unwrap();
        let again = event(&w.app, &w.event).await;
        assert_eq!(
            (again.closed_at, again.purge_at),
            (closed.closed_at, closed.purge_at)
        );
        assert_eq!(expiring_alerts(&w.app).await.len(), 2);
        for id in [first, second] {
            let stored: SearchDoc = store::get(&w.app.state.db, searches::COLLECTION, &id)
                .await
                .unwrap()
                .unwrap();
            assert!(stored.expiry_notified_at.is_some());
            assert_eq!(stored.status, SearchStatus::Active);
        }
        assert_eq!(close_count(&w.app).await, 1);
    }

    #[tokio::test]
    async fn two_runs_at_once_close_once_and_alert_once() {
        let w = world().await;
        let owner = w.app.actor("Owner").await;
        for _ in 0..5 {
            search(&w.app, &w.event, &owner.id).await;
        }
        idle_for(&w.app, &w.event, 31).await;
        let (a, b) = tokio::join!(run_once(&w.app.state), run_once(&w.app.state));
        a.unwrap();
        b.unwrap();
        assert_eq!(expiring_alerts(&w.app).await.len(), 5);
        assert_eq!(close_count(&w.app).await, 1);
    }

    #[tokio::test]
    async fn only_searches_without_an_accepted_match_get_an_alert_for_whoever_registered_them() {
        let w = world().await;
        let owner = w.app.actor("Owner").await;
        let code = w.app.invite(&w.admin, &w.shelter, "qr", "volunteer").await;
        let other_owner = w.app.join(&code, "Other").await;
        let plain = search(&w.app, &w.event, &owner.id).await;
        let declined = search(&w.app, &w.event, &other_owner.id).await;
        let accepted = search(&w.app, &w.event, &owner.id).await;
        match_of(&w.app, &declined, MatchStatus::Declined).await;
        match_of(&w.app, &accepted, MatchStatus::Open).await;
        match_of(&w.app, &accepted, MatchStatus::Accepted).await;
        let closed_search = search(&w.app, &w.event, &owner.id).await;
        let mut doc: SearchDoc = store::get(&w.app.state.db, searches::COLLECTION, &closed_search)
            .await
            .unwrap()
            .unwrap();
        doc.status = SearchStatus::Closed;
        store::replace(&w.app.state.db, searches::COLLECTION, &closed_search, &doc)
            .await
            .unwrap();
        // A search of another event is not touched.
        let far_shelter = w
            .app
            .create_shelter(&w.admin, "Far", (-23.55, -46.63))
            .await;
        let far_event = event_of(&w.app, &far_shelter).await;
        search(&w.app, &far_event, &owner.id).await;
        idle_for(&w.app, &w.event, 31).await;

        run_once(&w.app.state).await.unwrap();

        let purge_at = event(&w.app, &w.event).await.purge_at.unwrap();
        let mut got: Vec<(String, String)> = expiring_alerts(&w.app)
            .await
            .iter()
            .map(|a| {
                assert_eq!(a["detail"], json!({ "purge_at": purge_at.to_string() }));
                assert_eq!(
                    (
                        a["informational"].as_bool(),
                        a["for_civil_defense"].as_bool()
                    ),
                    (Some(true), Some(false))
                );
                assert_eq!(a["event_id"], json!(w.event));
                assert_eq!(a["target_kind"], "search");
                (
                    a["target_id"].as_str().unwrap().to_string(),
                    a["volunteer_ids"][0].as_str().unwrap().to_string(),
                )
            })
            .collect();
        got.sort();
        let mut expected = vec![
            (plain, owner.id.clone()),
            (declined, other_owner.id.clone()),
        ];
        expected.sort();
        assert_eq!(got, expected);

        // The volunteer reads it in the alert box; the shelter admin does not.
        let (status, mine) = w
            .app
            .send("GET", "/v1/alerts", &other_owner.secret, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{mine}");
        assert_eq!(mine["alerts"].as_array().unwrap().len(), 1);
        assert_eq!(mine["alerts"][0]["kind"], "search_expiring");
        let (_, admins) = w.app.send("GET", "/v1/alerts", &w.admin.secret, None).await;
        assert!(
            admins["alerts"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| a["kind"] != "search_expiring")
        );
    }

    #[tokio::test]
    async fn a_closed_event_left_half_done_is_finished_by_the_next_run() {
        let w = world().await;
        let owner = w.app.actor("Owner").await;
        let pending = search(&w.app, &w.event, &owner.id).await;
        // The event was closed by an earlier run that stopped before the alerts.
        let mut doc = event(&w.app, &w.event).await;
        doc.status = EventStatus::Closed;
        doc.closed_at = Some(store::now());
        doc.closure_reason = Some(ClosureReason::Inactivity);
        doc.purge_at = Some(plus_days(store::now(), 30));
        store::replace(&w.app.state.db, events::COLLECTION, &w.event, &doc)
            .await
            .unwrap();
        run_once(&w.app.state).await.unwrap();
        let alerts = expiring_alerts(&w.app).await;
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0]["target_id"], json!(pending));
    }

    #[tokio::test]
    async fn thumbnails_go_when_every_event_of_the_person_is_closed() {
        let w = world().await;
        let far_shelter = w
            .app
            .create_shelter(&w.admin, "Far", (-23.55, -46.63))
            .await;
        let far_event = event_of(&w.app, &far_shelter).await;
        idle_for(&w.app, &w.event, 31).await;

        let all_closed = thumbnail_person(&w.app, &[&w.event]).await;
        let mixed = thumbnail_person(&w.app, &[&w.event, &far_event]).await;
        let still_open = thumbnail_person(&w.app, &[&far_event]).await;
        let no_event = thumbnail_person(&w.app, &[]).await;

        run_once(&w.app.state).await.unwrap();

        assert!(!has_thumbnail(&w.app, &all_closed).await);
        for kept in [&mixed, &still_open, &no_event] {
            assert!(has_thumbnail(&w.app, kept).await, "{kept}");
        }
        assert!(
            audit_actions(&w.app.state.db)
                .await
                .contains(&"person.thumbnail_purge".to_string())
        );

        // When the other event closes too, the mixed person loses it as well.
        idle_for(&w.app, &far_event, 31).await;
        run_once(&w.app.state).await.unwrap();
        assert!(!has_thumbnail(&w.app, &mixed).await);
        assert!(!has_thumbnail(&w.app, &still_open).await);
        assert!(has_thumbnail(&w.app, &no_event).await);
    }

    #[tokio::test]
    async fn a_person_with_no_event_and_no_shelter_loses_the_thumbnail_after_the_purge_period() {
        let w = world().await;
        let aged = |days: i64, shelter: Option<&str>| {
            let mut doc = person(Level::NoBiometrics, shelter);
            doc.thumbnail_encrypted = Some(vec![1, 2, 3]);
            doc.updated_at = ago(days, 0);
            doc
        };
        let (old, recent, sheltered) = (new_id(), new_id(), new_id());
        put(&w.app, &old, &aged(31, None)).await;
        put(&w.app, &recent, &aged(29, None)).await;
        put(&w.app, &sheltered, &aged(31, Some(&w.shelter))).await;

        run_once(&w.app.state).await.unwrap();

        assert!(!has_thumbnail(&w.app, &old).await);
        assert!(has_thumbnail(&w.app, &recent).await);
        assert!(has_thumbnail(&w.app, &sheltered).await);
    }

    fn capture(expires_at: FirestoreInstant) -> CaptureDoc {
        CaptureDoc {
            process_id: "p".into(),
            purpose: Purpose::Login,
            device_id: "d".into(),
            status: CaptureStatus::Created,
            person_id: None,
            volunteer_id: None,
            shelter_id: None,
            expected_person_id: None,
            consumed: false,
            created_at: store::now(),
            expires_at,
        }
    }

    fn invite(shelter: &str, status: InviteStatus, expires_at: FirestoreInstant) -> InviteDoc {
        InviteDoc {
            shelter_id: shelter.into(),
            kind: InviteKind::Qr,
            role: ShelterRole::Volunteer,
            created_by: "x".into(),
            created_at: store::now(),
            expires_at,
            status,
            used_by: None,
            used_at: None,
        }
    }

    fn code(shelter: &str, used: bool, expires_at: FirestoreInstant) -> EndorsementCodeDoc {
        EndorsementCodeDoc {
            shelter_id: shelter.into(),
            created_by: "x".into(),
            lat: 0.0,
            lon: 0.0,
            accuracy_m: 1.0,
            expires_at,
            used,
        }
    }

    async fn exists(app: &TestApp, collection: &str, id: &str) -> bool {
        store::get::<Value>(&app.state.db, collection, id)
            .await
            .unwrap()
            .is_some()
    }

    #[tokio::test]
    async fn expired_captures_invites_and_codes_are_deleted_and_the_rest_kept() {
        let w = world().await;
        let (past, future) = (ago(0, 5), ago(0, -600));
        let db = &w.app.state.db;
        for (id, doc) in [
            ("old-capture", capture(past)),
            ("new-capture", capture(future)),
        ] {
            store::replace(db, "captures", id, &doc).await.unwrap();
        }
        for (id, status, at) in [
            ("old-invite", InviteStatus::Active, past),
            ("new-invite", InviteStatus::Active, future),
            ("used-invite", InviteStatus::Used, past),
            ("canceled-invite", InviteStatus::Canceled, past),
        ] {
            let doc = invite(&w.shelter, status, at);
            store::replace(db, invites::COLLECTION, id, &doc)
                .await
                .unwrap();
        }
        for (id, used, at) in [
            ("old-code", false, past),
            ("new-code", false, future),
            ("used-code", true, past),
        ] {
            let doc = code(&w.shelter, used, at);
            store::replace(db, endorsements::COLLECTION, id, &doc)
                .await
                .unwrap();
        }

        run_once(&w.app.state).await.unwrap();

        for (collection, id, kept) in [
            ("captures", "old-capture", false),
            ("captures", "new-capture", true),
            (invites::COLLECTION, "old-invite", false),
            (invites::COLLECTION, "new-invite", true),
            (invites::COLLECTION, "used-invite", true),
            (invites::COLLECTION, "canceled-invite", true),
            (endorsements::COLLECTION, "old-code", false),
            (endorsements::COLLECTION, "new-code", true),
            (endorsements::COLLECTION, "used-code", true),
        ] {
            assert_eq!(
                exists(&w.app, collection, id).await,
                kept,
                "{collection}/{id}"
            );
        }
        // Running again is harmless.
        run_once(&w.app.state).await.unwrap();
        assert!(exists(&w.app, "captures", "new-capture").await);
    }

    #[tokio::test]
    async fn more_than_one_batch_of_expired_documents_is_cleared() {
        let w = world().await;
        let past = ago(0, 5);
        let before = all_docs(&w.app.state.db, "captures").await.len();
        for n in 0..(BATCH_WRITES + 50) {
            store::replace(
                &w.app.state.db,
                "captures",
                &format!("c{n}"),
                &capture(past),
            )
            .await
            .unwrap();
        }
        store::replace(&w.app.state.db, "captures", "live", &capture(ago(0, -600)))
            .await
            .unwrap();
        run_once(&w.app.state).await.unwrap();
        assert_eq!(
            all_docs(&w.app.state.db, "captures").await.len(),
            before + 1
        );
    }
}
