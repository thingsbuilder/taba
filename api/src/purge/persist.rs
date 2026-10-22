//! The pass itself: it reads what is due, deletes in guarded transactions and audits.

use std::collections::{BTreeMap, HashMap, HashSet};

use firestore::{FirestoreDb, FirestoreInstant, FirestoreTimestamp};
use futures::StreamExt;
use serde_json::Value;

use super::domain::{
    Counts, DAILY_DELETIONS, DAY_MICROS, LEASE_MICROS, NO_EVENT, Op, PSEUDONYMIZE_PER_TX, Target,
    VOLUNTEER_IDLE_DAYS, bump, day_of, due_for_purge, micros_ahead,
};
use super::model::{
    CardRow, DAILY_DOC, DailyDoc, HouseholdRow, Id, LOCK_DOC, LockDoc, PSEUDONYMS, PersonRow,
    PseudonymDoc, PurgeReport, Row, STATE, VolunteerRow, WithoutEvent,
};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::AppState;
use crate::deliveries::{self, DeliveryDoc};
use crate::error::ApiError;
use crate::events::{self, EventDoc, EventStatus};
use crate::members::{self, MembershipDoc, MembershipStatus, ShelterRole};
use crate::shelters::{self, ShelterStatus};
use crate::store::{self, fs_error, tx_create, tx_delete, tx_replace};
use crate::volunteers::{self, GlobalRole};
use crate::{
    alerts, cards, households, matches, people, rescues, search_links, search_photos, searches,
};

/// One pass of the discard: events past `purge_at` (people, households, cards, rescues, searches,
/// matches, alerts, memberships and shelters of the event are deleted, deliveries are pseudonymized,
/// the event keeps its name, dates and `stats`), pseudonymized deliveries past `delivery_purge_at`,
/// and volunteer profiles 365 days after `last_seen_at` (banned ones stay). Batches of 400 writes,
/// stopping past 15 000 deletions in the day. With `config.purge_enabled` off it only counts.
/// Owner C. Called by `jobs::run_once`.
pub async fn run(state: &AppState) -> Result<PurgeReport, ApiError> {
    run_with(state, DAILY_DELETIONS).await
}

pub(super) async fn run_with(state: &AppState, daily_cap: u32) -> Result<PurgeReport, ApiError> {
    let dry_run = !state.config.purge_enabled;
    if !acquire_lock(&state.db).await? {
        tracing::info!("discard pass skipped: another one holds the lease");
        return Ok(PurgeReport {
            dry_run,
            ..PurgeReport::default()
        });
    }
    let mut pass = Pass::new(state, dry_run, daily_cap);
    let result = pass.execute().await;
    if let Err(err) = release_lock(&state.db).await {
        tracing::error!(
            code = err.code,
            "discard lease not released; it expires by itself"
        );
    }
    result?;
    tracing::info!(
        dry_run,
        deleted = pass.report.deleted,
        pseudonymized = pass.report.pseudonymized,
        counts = ?pass.totals,
        "discard pass done"
    );
    Ok(pass.report)
}

pub(super) async fn acquire_lock(db: &FirestoreDb) -> Result<bool, ApiError> {
    store::transact(db, |db, tx| {
        Box::pin(async move {
            let now = store::now();
            let held: Option<LockDoc> = store::get(&db, STATE, LOCK_DOC).await?;
            if held.is_some_and(|l| l.locked_until > now) {
                return Ok(false);
            }
            let lock = LockDoc {
                locked_until: micros_ahead(now, LEASE_MICROS),
            };
            tx_replace(&db, tx, STATE, LOCK_DOC, &lock)?;
            Ok(true)
        })
    })
    .await
}

async fn release_lock(db: &FirestoreDb) -> Result<(), ApiError> {
    let lock = LockDoc {
        locked_until: store::now(),
    };
    store::replace(db, STATE, LOCK_DOC, &lock).await
}

pub(super) async fn daily_doc(db: &FirestoreDb, day: &str) -> Result<DailyDoc, ApiError> {
    let doc: Option<DailyDoc> = store::get(db, STATE, DAILY_DOC).await?;
    Ok(doc.filter(|d| d.day == day).unwrap_or_else(|| DailyDoc {
        day: day.into(),
        ..DailyDoc::default()
    }))
}

fn audit_purge(
    db: &FirestoreDb,
    tx: &mut firestore::FirestoreTransaction<'_>,
    action: &str,
    target_kind: &str,
    target_id: &str,
    counts: Counts,
) -> Result<(), ApiError> {
    // The entry of the audit module, with the counts of what the transaction removed (ids only, never a person).
    audit::add(
        db,
        tx,
        &AuditEntry::new(action, target_kind, target_id)
            .basis(LegalBasis::PublicPolicy)
            .with_counts(counts),
    )
}

async fn ids_where(
    db: &FirestoreDb,
    collection: &str,
    field: &str,
    value: &str,
    limit: Option<u32>,
) -> Result<Vec<String>, ApiError> {
    let query = db
        .fluent()
        .select()
        .fields([field])
        .from(collection)
        .filter(|q| q.for_all([q.field(field).eq(value)]));
    let rows: Vec<Id> = match limit {
        Some(n) => query.limit(n).obj().query().await,
        None => query.obj().query().await,
    }
    .map_err(fs_error)?;
    Ok(rows.into_iter().map(|r| r.id).collect())
}

/// Deliveries of the event still carrying a person or a household (`pseudonymized == false`).
async fn deliveries_to_pseudonymize(
    db: &FirestoreDb,
    event_id: &str,
    limit: u32,
) -> Result<Vec<String>, ApiError> {
    let rows: Vec<Id> = db
        .fluent()
        .select()
        .fields(["event_id"])
        .from(deliveries::COLLECTION)
        .filter(|q| {
            q.for_all([
                q.field("event_id").eq(event_id),
                q.field("pseudonymized").eq(false),
            ])
        })
        .limit(limit)
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    Ok(rows.into_iter().map(|r| r.id).collect())
}

async fn pseudonymized_deliveries(
    db: &FirestoreDb,
    event_id: &str,
    limit: Option<u32>,
) -> Result<Vec<String>, ApiError> {
    let query = db
        .fluent()
        .select()
        .fields(["event_id"])
        .from(deliveries::COLLECTION)
        .filter(|q| {
            q.for_all([
                q.field("event_id").eq(event_id),
                q.field("pseudonymized").eq(true),
            ])
        });
    let rows: Vec<Id> = match limit {
        Some(n) => query.limit(n).obj().query().await,
        None => query.obj().query().await,
    }
    .map_err(fs_error)?;
    Ok(rows.into_iter().map(|r| r.id).collect())
}

async fn events_with(
    db: &FirestoreDb,
    status: EventStatus,
) -> Result<Vec<Row<EventDoc>>, ApiError> {
    db.fluent()
        .select()
        .from(events::COLLECTION)
        .filter(|q| q.for_all([q.field("status").eq(status)]))
        .obj()
        .query()
        .await
        .map_err(fs_error)
}

/// Whether every event is past its purge (`purged`, or `closed` with `purge_at` reached). An
/// event that does not exist is not due: nothing is deleted on a doubt.
async fn events_due(
    db: &FirestoreDb,
    cache: &mut HashMap<String, bool>,
    event_ids: &[String],
    now: FirestoreInstant,
) -> Result<bool, ApiError> {
    if event_ids.is_empty() {
        return Ok(false);
    }
    for id in event_ids {
        let due = match cache.get(id) {
            Some(due) => *due,
            None => {
                let event: Option<EventDoc> = store::get(db, events::COLLECTION, id).await?;
                let due = event
                    .is_some_and(|e| e.status == EventStatus::Purged || due_for_purge(&e, now));
                cache.insert(id.clone(), due);
                due
            }
        };
        if !due {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Every member is gone from `people`, or is about to be (`planned`, for the dry run).
async fn members_gone(
    db: &FirestoreDb,
    members: &[String],
    planned: &HashSet<String>,
) -> Result<bool, ApiError> {
    for member in members {
        if planned.contains(&format!("person:{member}")) {
            continue;
        }
        if store::get::<Value>(db, people::COLLECTION, member)
            .await?
            .is_some()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The memberships to delete with a volunteer, or `None` when the profile must stay: banned,
/// a validator or civil defense, seen again, or the only active admin of an open shelter.
async fn volunteer_deletable(
    db: &FirestoreDb,
    id: &str,
    cutoff: FirestoreInstant,
) -> Result<Option<Vec<String>>, ApiError> {
    let Some(row) = store::get::<VolunteerRow>(db, volunteers::COLLECTION, id).await? else {
        return Ok(None);
    };
    let protected = row
        .global_roles
        .iter()
        .any(|r| matches!(r, GlobalRole::Validator | GlobalRole::CivilDefense));
    if row.banned || protected || row.last_seen_at >= cutoff {
        return Ok(None);
    }
    let mine: Vec<Row<MembershipDoc>> = db
        .fluent()
        .select()
        .from(members::COLLECTION)
        .filter(|q| q.for_all([q.field("volunteer_id").eq(id)]))
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    for membership in mine
        .iter()
        .filter(|m| m.doc.status == MembershipStatus::Active && m.doc.role == ShelterRole::Admin)
    {
        let shelter: Option<shelters::ShelterDoc> =
            store::get(db, shelters::COLLECTION, &membership.doc.shelter_id).await?;
        if shelter.is_none_or(|s| s.status != ShelterStatus::Open) {
            continue;
        }
        let others: Vec<MembershipDoc> = db
            .fluent()
            .select()
            .from(members::COLLECTION)
            .filter(|q| {
                q.for_all([
                    q.field("shelter_id").eq(membership.doc.shelter_id.as_str()),
                    q.field("status").eq(MembershipStatus::Active),
                ])
            })
            .obj()
            .query()
            .await
            .map_err(fs_error)?;
        if !others
            .iter()
            .any(|m| m.role == ShelterRole::Admin && m.volunteer_id != id)
        {
            return Ok(None);
        }
    }
    Ok(Some(mine.into_iter().map(|m| m.id).collect()))
}

struct Pass<'a> {
    pub(super) state: &'a AppState,
    pub(super) dry: bool,
    pub(super) cap: u32,
    pub(super) now: FirestoreInstant,
    pub(super) day: String,
    /// Deletions the day still allows (`u32::MAX` in a dry run).
    pub(super) remaining: u32,
    pub(super) report: PurgeReport,
    /// What this pass changed or would change, by collection.
    pub(super) totals: Counts,
    /// Dry run only: what the earlier steps would have deleted (`person:`, `household:`, `card:`).
    pub(super) planned: HashSet<String>,
    /// Dry run only: the counts of each event, for `purge.dry_run`.
    pub(super) dry_by_event: BTreeMap<String, Counts>,
    pub(super) cache: HashMap<String, bool>,
}

impl<'a> Pass<'a> {
    fn new(state: &'a AppState, dry: bool, cap: u32) -> Self {
        let now = store::now();
        Self {
            state,
            dry,
            cap,
            now,
            day: day_of(now, &state.config.tz_pilot),
            remaining: u32::MAX,
            report: PurgeReport {
                dry_run: dry,
                ..PurgeReport::default()
            },
            totals: Counts::new(),
            planned: HashSet::new(),
            dry_by_event: BTreeMap::new(),
            cache: HashMap::new(),
        }
    }

    fn db(&self) -> &FirestoreDb {
        &self.state.db
    }

    fn limit(&self) -> Option<u32> {
        (!self.dry).then_some(self.remaining)
    }

    fn exhausted(&self) -> bool {
        self.remaining == 0
    }

    async fn execute(&mut self) -> Result<(), ApiError> {
        if !self.dry {
            let today = daily_doc(self.db(), &self.day).await?;
            self.remaining = self.cap.saturating_sub(today.deleted);
        }
        let mut due: Vec<Row<EventDoc>> = events_with(self.db(), EventStatus::Closed)
            .await?
            .into_iter()
            .filter(|e| due_for_purge(&e.doc, self.now))
            .collect();
        due.sort_by_key(|e| e.doc.purge_at);
        for event in &due {
            if self.exhausted() {
                break;
            }
            self.purge_event(&event.id, &event.doc).await?;
        }
        self.purge_deliveries().await?;
        self.purge_volunteers().await?;
        if self.dry {
            self.count_without_event().await?;
            self.log_dry_run().await?;
        }
        Ok(())
    }

    /// Dry run only: searches, rescues and people with no event, older than `PURGE_AFTER_DAYS`.
    /// No event's end discards them and the pass does not delete them by age (pending decision of
    /// the owner); they are counted under the id `no_event` of `purge.dry_run`, once a day.
    async fn count_without_event(&mut self) -> Result<(), ApiError> {
        if daily_doc(self.db(), &self.day).await?.without_event_counted {
            return Ok(());
        }
        let cutoff = micros_ahead(
            self.now,
            -i64::from(self.state.config.purge_after_days) * DAY_MICROS,
        );
        let mut counts = Counts::new();
        for (collection, age_field, label) in [
            (searches::COLLECTION, "created_at", "searches_without_event"),
            (rescues::COLLECTION, "received_at", "rescues_without_event"),
            (people::COLLECTION, "updated_at", "people_without_event"),
        ] {
            // Streamed, so memory stays flat; it reads each old document (two fields) once a day,
            // and only in a dry run.
            let mut old = self
                .db()
                .fluent()
                .select()
                .fields(["event_id", "event_ids"])
                .from(collection)
                .filter(|q| q.for_all([q.field(age_field).less_than(FirestoreTimestamp(cutoff))]))
                .obj::<WithoutEvent>()
                .stream_query_with_errors()
                .await
                .map_err(fs_error)?;
            let mut n = 0u32;
            while let Some(doc) = old.next().await {
                n += u32::from(doc.map_err(fs_error)?.has_no_event());
            }
            if n > 0 {
                bump(&mut counts, label, n);
            }
        }
        let day = self.day.clone();
        store::transact(self.db(), |db, tx| {
            let day = day.clone();
            Box::pin(async move {
                let mut today = daily_doc(&db, &day).await?;
                today.without_event_counted = true;
                tx_replace(&db, tx, STATE, DAILY_DOC, &today)
            })
        })
        .await?;
        if counts.is_empty() {
            return Ok(());
        }
        tracing::info!(counts = ?counts, "data without an event past the retention, not discarded");
        self.dry_by_event.insert(NO_EVENT.into(), counts);
        Ok(())
    }

    /// Runs the ops in transactions, stopping when the day's deletions run out. `false`: not all
    /// of them were done.
    async fn run_ops(&mut self, target: &Target<'_>, ops: Vec<Op>) -> Result<bool, ApiError> {
        if ops.is_empty() {
            return Ok(true);
        }
        if self.dry {
            return self.count_ops(target, &ops).await.map(|()| true);
        }
        let mut rest = ops.as_slice();
        while !rest.is_empty() {
            if self.exhausted() {
                return Ok(false);
            }
            let size = rest[0].per_tx().min(rest.len());
            let take = size.min(self.remaining as usize);
            let n = self.delete_chunk(target, &rest[..take]).await?;
            self.remaining = self.remaining.saturating_sub(n);
            self.report.deleted += n;
            rest = &rest[take..];
        }
        // The listing behind `ops` was cut at the day's room: with none left, whether anything
        // remains is for the next pass to see.
        Ok(!self.exhausted())
    }

    async fn count_ops(&mut self, target: &Target<'_>, ops: &[Op]) -> Result<(), ApiError> {
        let mut counts = Counts::new();
        for op in ops {
            match op {
                Op::Person(id) => {
                    self.planned.insert(format!("person:{id}"));
                }
                Op::Household(id) => {
                    self.planned.insert(format!("household:{id}"));
                    for card in
                        ids_where(self.db(), cards::COLLECTION, "household_id", id, None).await?
                    {
                        if self.planned.insert(format!("card:{card}")) {
                            bump(&mut counts, cards::COLLECTION, 1);
                        }
                    }
                }
                Op::Card(id) => {
                    if !self.planned.insert(format!("card:{id}")) {
                        continue;
                    }
                }
                Op::Volunteer(id) => {
                    let cutoff = self.volunteer_cutoff();
                    let Some(memberships) = volunteer_deletable(self.db(), id, cutoff).await?
                    else {
                        continue;
                    };
                    bump(&mut counts, members::COLLECTION, memberships.len() as u32);
                }
                Op::Plain(..) => {}
            }
            bump(&mut counts, op.label(), 1);
        }
        let n: u32 = counts.values().sum();
        self.report.deleted += n;
        for (label, n) in &counts {
            bump(&mut self.totals, label, *n);
        }
        let entry = self.dry_by_event.entry(target.id.to_string()).or_default();
        for (label, n) in counts {
            bump(entry, &label, n);
        }
        Ok(())
    }

    fn volunteer_cutoff(&self) -> FirestoreInstant {
        micros_ahead(self.now, -VOLUNTEER_IDLE_DAYS * DAY_MICROS)
    }

    /// One transaction: the guarded deletes, the daily counter and the audit entry.
    async fn delete_chunk(&mut self, target: &Target<'_>, ops: &[Op]) -> Result<u32, ApiError> {
        let cutoff = self.volunteer_cutoff();
        let day = self.day.clone();
        let (action, kind, id) = (
            target.action.to_string(),
            target.kind.to_string(),
            target.id.to_string(),
        );
        let done = store::transact(self.db(), |db, tx| {
            let (ops, day) = (ops.to_vec(), day.clone());
            let (action, kind, id) = (action.clone(), kind.clone(), id.clone());
            Box::pin(async move {
                let now = store::now();
                let mut counts = Counts::new();
                let mut cache = HashMap::new();
                for op in ops {
                    match op {
                        Op::Plain(collection, doc_id) => {
                            tx_delete(&db, tx, collection, &doc_id)?;
                            bump(&mut counts, collection, 1);
                        }
                        Op::Person(doc_id) => {
                            let Some(row) =
                                store::get::<PersonRow>(&db, people::COLLECTION, &doc_id).await?
                            else {
                                continue;
                            };
                            if events_due(&db, &mut cache, &row.event_ids, now).await? {
                                tx_delete(&db, tx, people::COLLECTION, &doc_id)?;
                                bump(&mut counts, people::COLLECTION, 1);
                            }
                        }
                        Op::Household(doc_id) => {
                            let Some(row) =
                                store::get::<HouseholdRow>(&db, households::COLLECTION, &doc_id)
                                    .await?
                            else {
                                continue;
                            };
                            if !members_gone(&db, &row.member_ids, &HashSet::new()).await? {
                                continue;
                            }
                            for card in
                                ids_where(&db, cards::COLLECTION, "household_id", &doc_id, None)
                                    .await?
                            {
                                tx_delete(&db, tx, cards::COLLECTION, &card)?;
                                bump(&mut counts, cards::COLLECTION, 1);
                            }
                            tx_delete(&db, tx, households::COLLECTION, &doc_id)?;
                            bump(&mut counts, households::COLLECTION, 1);
                        }
                        Op::Card(doc_id) => {
                            let Some(row) =
                                store::get::<CardRow>(&db, cards::COLLECTION, &doc_id).await?
                            else {
                                continue;
                            };
                            let household =
                                store::get::<Value>(&db, households::COLLECTION, &row.household_id)
                                    .await?;
                            if household.is_none() {
                                tx_delete(&db, tx, cards::COLLECTION, &doc_id)?;
                                bump(&mut counts, cards::COLLECTION, 1);
                            }
                        }
                        Op::Volunteer(doc_id) => {
                            let Some(memberships) =
                                volunteer_deletable(&db, &doc_id, cutoff).await?
                            else {
                                continue;
                            };
                            for membership in &memberships {
                                tx_delete(&db, tx, members::COLLECTION, membership)?;
                                bump(&mut counts, members::COLLECTION, 1);
                            }
                            tx_delete(&db, tx, volunteers::COLLECTION, &doc_id)?;
                            bump(&mut counts, volunteers::COLLECTION, 1);
                        }
                    }
                }
                let n: u32 = counts.values().sum();
                if n == 0 {
                    return Ok((0, counts));
                }
                let mut today = daily_doc(&db, &day).await?;
                today.deleted += n;
                tx_replace(&db, tx, STATE, DAILY_DOC, &today)?;
                audit_purge(&db, tx, &action, &kind, &id, counts.clone())?;
                Ok((n, counts))
            })
        })
        .await?;
        for (label, n) in &done.1 {
            bump(&mut self.totals, label, *n);
        }
        Ok(done.0)
    }

    /// Discards one event. Returns `false` when the day's deletions ran out first.
    async fn purge_event(&mut self, event_id: &str, event: &EventDoc) -> Result<bool, ApiError> {
        let target = Target {
            action: "purge.event",
            kind: "event",
            id: event_id,
        };
        let shelter_ids =
            ids_where(self.db(), shelters::COLLECTION, "event_id", event_id, None).await?;
        if !self.dry && event.stats.is_none() {
            self.write_stats(event_id).await?;
        }
        self.pseudonymize(event_id).await?;

        let limit = self.limit();
        // Matches go before their search, so a pass that stops between them leaves the search to
        // find the rest of its matches.
        let mut ops = Vec::new();
        for search in
            ids_where(self.db(), searches::COLLECTION, "event_id", event_id, limit).await?
        {
            for found in
                ids_where(self.db(), matches::COLLECTION, "search_id", &search, None).await?
            {
                ops.push(Op::Plain(matches::COLLECTION, found));
            }
            for photo in ids_where(
                self.db(),
                search_photos::COLLECTION,
                "search_id",
                &search,
                None,
            )
            .await?
            {
                ops.push(Op::Plain(search_photos::COLLECTION, photo));
            }
            ops.push(Op::Plain(searches::COLLECTION, search));
        }
        if !self.run_ops(&target, ops).await? {
            return Ok(false);
        }
        for (collection, field) in [
            (search_links::COLLECTION, "event_id"),
            (alerts::COLLECTION, "event_id"),
            (rescues::COLLECTION, "event_id"),
        ] {
            let limit = self.limit();
            let ids = ids_where(self.db(), collection, field, event_id, limit).await?;
            let ops = ids
                .into_iter()
                .map(|id| Op::Plain(collection, id))
                .collect();
            if !self.run_ops(&target, ops).await? {
                return Ok(false);
            }
        }
        for shelter in &shelter_ids {
            let limit = self.limit();
            let ids =
                ids_where(self.db(), members::COLLECTION, "shelter_id", shelter, limit).await?;
            let ops = ids
                .into_iter()
                .map(|id| Op::Plain(members::COLLECTION, id))
                .collect();
            if !self.run_ops(&target, ops).await? {
                return Ok(false);
            }
        }

        let (person_ops, mut household_ids) = self.due_people(event_id).await?;
        if !self.run_ops(&target, person_ops).await? {
            return Ok(false);
        }
        for shelter in &shelter_ids {
            household_ids.extend(
                ids_where(
                    self.db(),
                    households::COLLECTION,
                    "created_shelter_id",
                    shelter,
                    None,
                )
                .await?,
            );
        }
        let mut household_ops = Vec::new();
        for id in household_ids {
            let row: Option<HouseholdRow> =
                store::get(self.db(), households::COLLECTION, &id).await?;
            if let Some(row) = row {
                if members_gone(self.db(), &row.member_ids, &self.planned).await? {
                    household_ops.push(Op::Household(id));
                }
            }
        }
        if !self.run_ops(&target, household_ops).await? {
            return Ok(false);
        }
        let mut card_ops = Vec::new();
        for shelter in &shelter_ids {
            for id in ids_where(self.db(), cards::COLLECTION, "shelter_id", shelter, None).await? {
                card_ops.push(Op::Card(id));
            }
        }
        if !self.run_ops(&target, card_ops).await? {
            return Ok(false);
        }

        let limit = self.limit();
        let ids = ids_where(self.db(), PSEUDONYMS, "event_id", event_id, limit).await?;
        let ops = ids
            .into_iter()
            .map(|id| Op::Plain(PSEUDONYMS, id))
            .collect();
        if !self.run_ops(&target, ops).await? {
            return Ok(false);
        }
        let ops = shelter_ids
            .into_iter()
            .take(self.limit().map_or(usize::MAX, |n| n as usize))
            .map(|id| Op::Plain(shelters::COLLECTION, id))
            .collect();
        if !self.run_ops(&target, ops).await? {
            return Ok(false);
        }
        if !self.dry {
            self.finish_event(event_id).await?;
        }
        Ok(true)
    }

    /// The people of the event whose every event is due, and the households they point to.
    async fn due_people(&mut self, event_id: &str) -> Result<(Vec<Op>, HashSet<String>), ApiError> {
        let rows: Vec<Row<PersonRow>> = self
            .db()
            .fluent()
            .select()
            .fields(["event_ids", "household_id"])
            .from(people::COLLECTION)
            .filter(|q| q.for_all([q.field("event_ids").array_contains(event_id)]))
            .obj()
            .query()
            .await
            .map_err(fs_error)?;
        let (mut ops, mut households) = (Vec::new(), HashSet::new());
        for row in rows {
            let db = &self.state.db;
            if !events_due(db, &mut self.cache, &row.doc.event_ids, self.now).await? {
                continue;
            }
            households.extend(row.doc.household_id);
            ops.push(Op::Person(row.id));
        }
        if let Some(room) = self.limit() {
            ops.truncate(room as usize);
        }
        Ok((ops, households))
    }

    async fn write_stats(&self, event_id: &str) -> Result<(), ApiError> {
        let stats = crate::export::event_stats(self.db(), event_id).await?;
        store::transact(self.db(), |db, tx| {
            let id = event_id.to_string();
            Box::pin(async move {
                let Some(mut event) = store::get::<EventDoc>(&db, events::COLLECTION, &id).await?
                else {
                    return Ok(());
                };
                if event.status != EventStatus::Closed || event.stats.is_some() {
                    return Ok(());
                }
                event.stats = Some(stats);
                tx_replace(&db, tx, events::COLLECTION, &id, &event)
            })
        })
        .await
    }

    /// Strips the person and the card from the deliveries of the event, 199 at a time.
    async fn pseudonymize(&mut self, event_id: &str) -> Result<(), ApiError> {
        if self.dry {
            let n = deliveries_to_pseudonymize(self.db(), event_id, i32::MAX as u32)
                .await?
                .len() as u32;
            self.report.pseudonymized += n;
            bump(&mut self.totals, "deliveries_pseudonymized", n);
            bump(
                self.dry_by_event.entry(event_id.into()).or_default(),
                "deliveries_pseudonymized",
                n,
            );
            return Ok(());
        }
        loop {
            let batch =
                deliveries_to_pseudonymize(self.db(), event_id, PSEUDONYMIZE_PER_TX as u32).await?;
            if batch.is_empty() {
                return Ok(());
            }
            let done = store::transact(self.db(), |db, tx| {
                let (batch, event_id) = (batch.clone(), event_id.to_string());
                Box::pin(async move {
                    let mut pseudonyms: HashMap<String, String> = HashMap::new();
                    let mut done = 0u32;
                    for id in batch {
                        let Some(mut delivery) =
                            store::get::<DeliveryDoc>(&db, deliveries::COLLECTION, &id).await?
                        else {
                            continue;
                        };
                        if delivery.pseudonymized {
                            continue;
                        }
                        if let Some(household) = delivery.household_id.take() {
                            let pseudonym = match pseudonyms.get(&household) {
                                Some(p) => p.clone(),
                                None => {
                                    let stored: Option<PseudonymDoc> =
                                        store::get(&db, PSEUDONYMS, &household).await?;
                                    let p = match stored {
                                        Some(doc) => doc.pseudonym,
                                        None => {
                                            let p = uuid::Uuid::new_v4().to_string();
                                            let doc = PseudonymDoc {
                                                event_id: event_id.clone(),
                                                pseudonym: p.clone(),
                                            };
                                            tx_create(&db, tx, PSEUDONYMS, &household, &doc)?;
                                            p
                                        }
                                    };
                                    pseudonyms.insert(household, p.clone());
                                    p
                                }
                            };
                            delivery.household_pseudonym = Some(pseudonym);
                        }
                        delivery.person_id = None;
                        delivery.card_code = None;
                        delivery.delivered_by = String::new();
                        delivery.approved_by = None;
                        delivery.reviewed_by = None;
                        delivery.review_note = None;
                        delivery.pseudonymized = true;
                        tx_replace(&db, tx, deliveries::COLLECTION, &id, &delivery)?;
                        done += 1;
                    }
                    if done > 0 {
                        let mut counts = Counts::new();
                        bump(&mut counts, "deliveries_pseudonymized", done);
                        audit_purge(&db, tx, "purge.event", "event", &event_id, counts)?;
                    }
                    Ok(done)
                })
            })
            .await?;
            self.report.pseudonymized += done;
            bump(&mut self.totals, "deliveries_pseudonymized", done);
            if done == 0 {
                return Ok(());
            }
        }
    }

    async fn finish_event(&self, event_id: &str) -> Result<(), ApiError> {
        store::transact(self.db(), |db, tx| {
            let id = event_id.to_string();
            Box::pin(async move {
                let Some(mut event) = store::get::<EventDoc>(&db, events::COLLECTION, &id).await?
                else {
                    return Ok(());
                };
                if event.status != EventStatus::Closed {
                    return Ok(());
                }
                event.status = EventStatus::Purged;
                event.purged_at = Some(store::now());
                tx_replace(&db, tx, events::COLLECTION, &id, &event)?;
                let mut counts = Counts::new();
                bump(&mut counts, "events_purged", 1);
                audit_purge(&db, tx, "purge.event", "event", &id, counts)
            })
        })
        .await
    }

    /// Deletes the pseudonymized deliveries of events past `delivery_purge_at`. A delivery that
    /// is not pseudonymized yet is never touched here.
    async fn purge_deliveries(&mut self) -> Result<(), ApiError> {
        let mut candidates = events_with(self.db(), EventStatus::Purged).await?;
        candidates.extend(events_with(self.db(), EventStatus::Closed).await?);
        // ponytail: one empty query (one read) per purged or closed event each hour, for ever:
        // 24 reads a day per event, so about a thousand events use the free daily quota of
        // reads. Past that, mark the event once its deliveries are gone so it drops out.
        for event in candidates {
            if self.exhausted() {
                return Ok(());
            }
            if event.doc.delivery_purge_at.is_none_or(|at| at > self.now) {
                continue;
            }
            let limit = self.limit();
            let ids = pseudonymized_deliveries(self.db(), &event.id, limit).await?;
            let ops = ids
                .into_iter()
                .map(|id| Op::Plain(deliveries::COLLECTION, id))
                .collect();
            let target = Target {
                action: "purge.deliveries",
                kind: "event",
                id: &event.id,
            };
            self.run_ops(&target, ops).await?;
        }
        Ok(())
    }

    async fn purge_volunteers(&mut self) -> Result<(), ApiError> {
        if self.exhausted() {
            return Ok(());
        }
        let cutoff = self.volunteer_cutoff();
        let stale: Vec<Row<VolunteerRow>> = self
            .db()
            .fluent()
            .select()
            .fields(["banned", "global_roles", "last_seen_at"])
            .from(volunteers::COLLECTION)
            .filter(|q| {
                q.for_all([q
                    .field("last_seen_at")
                    .less_than(FirestoreTimestamp(cutoff))])
            })
            .obj()
            .query()
            .await
            .map_err(fs_error)?;
        let mut ops: Vec<Op> = stale
            .into_iter()
            .filter(|v| !v.doc.banned)
            .map(|v| Op::Volunteer(v.id))
            .collect();
        if let Some(room) = self.limit() {
            ops.truncate(room as usize);
        }
        let target = Target {
            action: "purge.volunteers",
            kind: "volunteer",
            id: "expired",
        };
        self.run_ops(&target, ops).await?;
        Ok(())
    }

    /// Dry run: one `purge.dry_run` per event and day, with the counts. Always logged as well.
    async fn log_dry_run(&self) -> Result<(), ApiError> {
        for (id, counts) in &self.dry_by_event {
            tracing::info!(event_id = id, counts = ?counts, "discard dry run");
        }
        let (day, by_event) = (self.day.clone(), self.dry_by_event.clone());
        store::transact(self.db(), |db, tx| {
            let (day, by_event) = (day.clone(), by_event.clone());
            Box::pin(async move {
                let mut today = daily_doc(&db, &day).await?;
                let mut changed = false;
                for (id, counts) in by_event {
                    if today.dry_run_logged.contains(&id) {
                        continue;
                    }
                    audit_purge(&db, tx, "purge.dry_run", "event", &id, counts)?;
                    today.dry_run_logged.push(id);
                    changed = true;
                }
                if changed {
                    tx_replace(&db, tx, STATE, DAILY_DOC, &today)?;
                }
                Ok(())
            })
        })
        .await
    }
}
