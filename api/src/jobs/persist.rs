//! The hourly steps: their queries and transactions.

use std::collections::HashSet;

use firestore::{FirestoreDb, FirestoreInstant, FirestoreResult, FirestoreTimestamp, path};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::json;

use super::domain::{
    BATCH_WRITES, Budget, DAY_MICROS, MAX_EVENTS, minus_days, orphan_expired, plus_days,
};
use super::model::{Expiring, Listed, PersonEvents};
use crate::alerts::{self, AlertDoc, AlertKind};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::AppState;
use crate::endorsements;
use crate::error::ApiError;
use crate::events::{self, ClosureReason, EventDoc, EventStatus};
use crate::invites::{self, InviteStatus};
use crate::matches::{self, MatchDoc, MatchStatus};
use crate::people::{self, PersonDoc};
use crate::search_photos;
use crate::searches::{self, SearchDoc, SearchStatus};
use crate::shelters::{self, ShelterDoc, ShelterStatus};
use crate::store::{self, fs_error, tx_delete, tx_replace};

async fn events_with(
    db: &FirestoreDb,
    status: EventStatus,
) -> Result<Vec<Listed<EventDoc>>, ApiError> {
    let listed: Vec<Listed<EventDoc>> = db
        .fluent()
        .select()
        .from(events::COLLECTION)
        .filter(|q| q.for_all([q.field(path!(EventDoc::status)).eq(status)]))
        .limit(MAX_EVENTS as u32)
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    if listed.len() == MAX_EVENTS {
        tracing::warn!(
            limit = MAX_EVENTS,
            ?status,
            "events listing is full; the events past it wait"
        );
    }
    Ok(listed)
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

pub(super) async fn close_events(state: &AppState) -> Result<(), ApiError> {
    let (inactivity_days, purge_days, delivery_days) = (
        state.config.inactivity_days,
        state.config.purge_after_days,
        state.config.delivery_retention_days,
    );
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
                audit::add(
                    &db,
                    tx,
                    &AuditEntry::new("event.close", "event", &id).basis(LegalBasis::PublicPolicy),
                )
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
/// halfway is finished by the next one. The searches of an event are read as a stream; the
/// ones already notified are skipped without a write, and the step stops at its work budget.
pub(super) async fn notify_expiring_searches(state: &AppState) -> Result<(), ApiError> {
    let mut budget = Budget::new("notify_expiring_searches");
    for event in events_with(&state.db, EventStatus::Closed).await? {
        let Some(purge_at) = event.doc.purge_at else {
            continue;
        };
        let mut active = state
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
            .obj::<SearchDoc>()
            .stream_query_with_errors()
            .await
            .map_err(fs_error)?;
        let mut pending: Vec<String> = Vec::new();
        while let Some(search) = active.next().await {
            let search = search.map_err(fs_error)?;
            let Some(search_id) = search.id.clone() else {
                continue;
            };
            // The event is over: the family's photos kept for a match go now, not at the discard.
            if search.retained_photos > 0 {
                discard_family_photos(state, &search_id, "event_closed").await?;
                budget.spend(1);
            }
            if search.expiry_notified_at.is_none() {
                pending.push(search_id);
            }
            // A search and its alert: two writes.
            if pending.len() == BATCH_WRITES / 2 {
                notify_chunk(state, &event.id, purge_at, &pending).await?;
                budget.spend(pending.len());
                pending.clear();
            }
            if budget.spent() {
                return Ok(());
            }
        }
        notify_chunk(state, &event.id, purge_at, &pending).await?;
        budget.spend(pending.len());
    }
    Ok(())
}

async fn notify_chunk(
    state: &AppState,
    event_id: &str,
    purge_at: FirestoreInstant,
    chunk: &[String],
) -> Result<(), ApiError> {
    if chunk.is_empty() {
        return Ok(());
    }
    store::transact(&state.db, |db, tx| {
        let (chunk, event_id) = (chunk.to_vec(), event_id.to_string());
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
    .await
}

/// Discards the family's photos kept for a search, in a transaction that reads it again.
async fn discard_family_photos(
    state: &AppState,
    search_id: &str,
    reason: &'static str,
) -> Result<(), ApiError> {
    store::transact(&state.db, |db, tx| {
        let search_id = search_id.to_string();
        Box::pin(async move {
            if let Some(mut search) =
                store::get::<SearchDoc>(&db, searches::COLLECTION, &search_id).await?
            {
                search_photos::discard(&db, tx, &search_id, &mut search, reason)?;
            }
            Ok(())
        })
    })
    .await
}

/// The family's photos go `PURGE_AFTER_DAYS` after the search was made at the latest, whatever
/// became of the match (one nobody attends, a search with no event). Read as a stream; stops at
/// the work budget.
pub(super) async fn expire_family_photos(state: &AppState) -> Result<(), ApiError> {
    let cutoff = minus_days(store::now(), state.config.purge_after_days);
    let mut budget = Budget::new("expire_family_photos");
    let mut kept = state
        .db
        .fluent()
        .select()
        .from(searches::COLLECTION)
        .filter(|q| q.for_all([q.field(path!(SearchDoc::retained_photos)).greater_than(0)]))
        .obj::<SearchDoc>()
        .stream_query_with_errors()
        .await
        .map_err(fs_error)?;
    while let Some(search) = kept.next().await {
        let search = search.map_err(fs_error)?;
        let Some(search_id) = search.id.filter(|_| search.created_at < cutoff) else {
            continue;
        };
        discard_family_photos(state, &search_id, "max_age").await?;
        budget.spend(1);
        if budget.spent() {
            break;
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

/// Deletes the thumbnail of every person whose events are all closed. The people with a
/// thumbnail are read as a stream (their event ids only); the step stops at its work budget.
pub(super) async fn delete_thumbnails(state: &AppState) -> Result<(), ApiError> {
    let active: HashSet<String> = events_with(&state.db, EventStatus::Active)
        .await?
        .into_iter()
        .map(|e| e.id)
        .collect();
    let mut with_thumbnail = state
        .db
        .fluent()
        .select()
        .fields(["event_ids", "current_shelter_id", "updated_at"])
        .from(people::COLLECTION)
        .filter(|q| q.for_all([q.field(path!(PersonDoc::thumbnail_encrypted)).is_not_null()]))
        .obj::<Listed<PersonEvents>>()
        .stream_query_with_errors()
        .await
        .map_err(fs_error)?;
    let cutoff = minus_days(store::now(), state.config.purge_after_days);
    let mut budget = Budget::new("delete_thumbnails");
    let mut due: Vec<String> = Vec::new();
    // A person and the audit entry: two writes.
    while let Some(person) = with_thumbnail.next().await {
        let person = person.map_err(fs_error)?;
        let is_due = (!person.doc.event_ids.is_empty()
            && person.doc.event_ids.iter().all(|e| !active.contains(e)))
            || orphan_expired(
                &person.doc.event_ids,
                person.doc.current_shelter_id.as_deref(),
                person.doc.updated_at,
                cutoff,
            );
        if !is_due {
            continue;
        }
        due.push(person.id);
        if due.len() == BATCH_WRITES / 2 {
            purge_thumbnails(state, &due, cutoff).await?;
            budget.spend(due.len());
            due.clear();
            if budget.spent() {
                return Ok(());
            }
        }
    }
    purge_thumbnails(state, &due, cutoff).await
}

async fn purge_thumbnails(
    state: &AppState,
    due: &[String],
    cutoff: FirestoreInstant,
) -> Result<(), ApiError> {
    if due.is_empty() {
        return Ok(());
    }
    store::transact(&state.db, |db, tx| {
        let chunk = due.to_vec();
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
                    &AuditEntry::new("person.thumbnail_purge", "person", &person_id)
                        .basis(LegalBasis::PublicPolicy),
                )?;
            }
            Ok(())
        })
    })
    .await
}

// ---- 3. expired captures, invites and endorsement codes (spec 6.2.6 item 4) ----

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

/// Reads the listed documents as they come and deletes the expired ones, `BATCH_WRITES` at a
/// time, until the budget is gone.
async fn sweep(
    state: &AppState,
    budget: &mut Budget,
    collection: &'static str,
    mut listed: BoxStream<'_, FirestoreResult<Expiring>>,
    keep: fn(&Expiring) -> bool,
) -> Result<(), ApiError> {
    let now = store::now();
    let mut ids = Vec::new();
    while let Some(doc) = listed.next().await {
        let doc = doc.map_err(fs_error)?;
        if doc.expires_at > now {
            continue;
        }
        ids.extend(doc.id);
        if ids.len() == BATCH_WRITES {
            delete_if_expired(state, collection, std::mem::take(&mut ids), keep).await?;
            budget.spend(BATCH_WRITES);
            if budget.spent() {
                return Ok(());
            }
        }
    }
    budget.spend(ids.len());
    delete_if_expired(state, collection, ids, keep).await
}

pub(super) async fn delete_expired(state: &AppState) -> Result<(), ApiError> {
    let now = store::now();
    let mut budget = Budget::new("delete_expired");
    // Captures: the Firestore TTL also removes them, some time after `expires_at`.
    let captures = state
        .db
        .fluent()
        .select()
        .from("captures")
        .filter(|q| q.for_all([q.field("expires_at").less_than(FirestoreTimestamp(now))]))
        .obj::<Expiring>()
        .stream_query_with_errors()
        .await
        .map_err(fs_error)?;
    sweep(state, &mut budget, "captures", captures, |_| false).await?;

    let invites = state
        .db
        .fluent()
        .select()
        .from(invites::COLLECTION)
        .filter(|q| q.for_all([q.field("status").eq(InviteStatus::Active)]))
        .obj::<Expiring>()
        .stream_query_with_errors()
        .await
        .map_err(fs_error)?;
    if budget.left > 0 {
        sweep(state, &mut budget, invites::COLLECTION, invites, |d| {
            d.status.as_deref() != Some("active")
        })
        .await?;
    }

    let codes = state
        .db
        .fluent()
        .select()
        .from(endorsements::COLLECTION)
        .filter(|q| q.for_all([q.field("used").eq(false)]))
        .obj::<Expiring>()
        .stream_query_with_errors()
        .await
        .map_err(fs_error)?;
    if budget.left > 0 {
        sweep(state, &mut budget, endorsements::COLLECTION, codes, |d| {
            d.used
        })
        .await?;
    }
    Ok(())
}
