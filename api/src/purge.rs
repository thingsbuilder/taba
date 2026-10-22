//! The differentiated discard (spec 10.1.2, 6.2.6 item 3), called by the hourly task in `jobs.rs`.
//!
//! Layers of safety, outermost first:
//! 1. `PURGE_ENABLED` off (the default): dry run. The pass computes what it would delete and
//!    only logs the counts and writes `purge.dry_run` (once a day per event).
//! 2. A lease document keeps two passes from running at once.
//! 3. Only `closed` events with `purge_at <= now` are discarded; a person is deleted only when
//!    every event of theirs is due (re-checked inside the deleting transaction).
//! 4. Deletes go in transactions of at most `BATCH_WRITES` writes, each carrying the daily
//!    counter and a `purge.event` audit entry, and the pass stops at `DAILY_DELETIONS` deletions
//!    in the day (in `TZ_PILOT`); the next hour resumes.
//!
//! Every step recomputes what is left from the database, so a pass that stopped halfway
//! (crash, daily cap) is finished by the next one. Deliveries are pseudonymized first, with the
//! pseudonym of each household kept in `purge_pseudonyms` until the event is done, so a resumed
//! pass gives the same household the same pseudonym. `audit_log` is never touched here.

use std::collections::{BTreeMap, HashMap, HashSet};

use firestore::jiff::tz::TimeZone;
use firestore::{FirestoreDb, FirestoreInstant, FirestoreTimestamp};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// Writes of one transaction (Firestore takes 500).
const BATCH_WRITES: usize = 400;
/// Deletes per transaction: the batch minus the daily counter and the audit entry.
const DELETES_PER_TX: usize = BATCH_WRITES - 2;
/// A delivery write plus at most one new pseudonym, and the audit entry.
const PSEUDONYMIZE_PER_TX: usize = (BATCH_WRITES - 1) / 2;
/// A household takes its cards with it, so fewer per transaction.
const HOUSEHOLDS_PER_TX: usize = 50;
/// The free tier allows 20 000 deletions a day (spec 6.3.5).
const DAILY_DELETIONS: u32 = 15_000;
const VOLUNTEER_IDLE_DAYS: i64 = 365;
const DAY_MICROS: i64 = 86_400_000_000;
/// A pass that dies leaves the lease behind; the next one may take it after this.
const LEASE_MICROS: i64 = 20 * 60 * 1_000_000;

// A transaction stays within `BATCH_WRITES` writes.
const _: () = assert!(DELETES_PER_TX + 2 <= BATCH_WRITES);
const _: () = assert!(PSEUDONYMIZE_PER_TX * 2 < BATCH_WRITES);
const _: () = assert!(HOUSEHOLDS_PER_TX * 3 + 2 <= BATCH_WRITES);

const STATE: &str = "purge_state";
const LOCK_DOC: &str = "lock";
const DAILY_DOC: &str = "daily";
const PSEUDONYMS: &str = "purge_pseudonyms";

/// What one pass did (or, with `PURGE_ENABLED` off, would have done).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PurgeReport {
    /// `true` when `PURGE_ENABLED` is off: the counts below are what it would delete or change.
    pub dry_run: bool,
    pub deleted: u32,
    pub pseudonymized: u32,
}

/// One pass of the discard: events past `purge_at` (people, households, cards, rescues, searches,
/// matches, alerts, memberships and shelters of the event are deleted, deliveries are pseudonymized,
/// the event keeps its name, dates and `stats`), pseudonymized deliveries past `delivery_purge_at`,
/// and volunteer profiles 365 days after `last_seen_at` (banned ones stay). Batches of 400 writes,
/// stopping past 15 000 deletions in the day. With `config.purge_enabled` off it only counts.
/// Owner C. Called by `jobs::run_once`.
pub async fn run(state: &AppState) -> Result<PurgeReport, ApiError> {
    run_with(state, DAILY_DELETIONS).await
}

async fn run_with(state: &AppState, daily_cap: u32) -> Result<PurgeReport, ApiError> {
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

// ---- lease and daily counter ----

#[derive(Serialize, Deserialize)]
struct LockDoc {
    #[serde(with = "firestore::serialize_as_timestamp")]
    locked_until: FirestoreInstant,
}

fn micros_ahead(at: FirestoreInstant, micros: i64) -> FirestoreInstant {
    FirestoreInstant::from_microsecond(at.as_microsecond() + micros).expect("a date within range")
}

async fn acquire_lock(db: &FirestoreDb) -> Result<bool, ApiError> {
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

/// Deletions made on `day` (a date in `TZ_PILOT`), the events whose dry run was already audited
/// that day and whether the data without an event was counted.
#[derive(Serialize, Deserialize, Default)]
struct DailyDoc {
    day: String,
    deleted: u32,
    #[serde(default)]
    dry_run_logged: Vec<String>,
    /// The data without an event was already counted today (it reads every old document).
    #[serde(default)]
    without_event_counted: bool,
}

async fn daily_doc(db: &FirestoreDb, day: &str) -> Result<DailyDoc, ApiError> {
    let doc: Option<DailyDoc> = store::get(db, STATE, DAILY_DOC).await?;
    Ok(doc.filter(|d| d.day == day).unwrap_or_else(|| DailyDoc {
        day: day.into(),
        ..DailyDoc::default()
    }))
}

fn day_of(at: FirestoreInstant, tz: &str) -> String {
    let zone = TimeZone::get(tz).unwrap_or(TimeZone::UTC);
    at.to_zoned(zone).date().to_string()
}

// ---- audit ----

type Counts = BTreeMap<String, u32>;

fn bump(counts: &mut Counts, label: &str, n: u32) {
    *counts.entry(label.into()).or_default() += n;
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

/// The target id of the `purge.dry_run` entry for data no event owns.
const NO_EVENT: &str = "no_event";

/// The event fields of a search or rescue (`event_id`) or a person (`event_ids`).
#[derive(Deserialize)]
struct WithoutEvent {
    #[serde(default)]
    event_id: Option<String>,
    #[serde(default)]
    event_ids: Vec<String>,
}

impl WithoutEvent {
    fn has_no_event(&self) -> bool {
        self.event_id.is_none() && self.event_ids.is_empty()
    }
}

// ---- queries (ids only, by the field the contract indexes) ----

#[derive(Deserialize)]
struct Id {
    #[serde(alias = "_firestore_id")]
    id: String,
}

#[derive(Deserialize)]
struct Row<T> {
    #[serde(alias = "_firestore_id")]
    id: String,
    #[serde(flatten)]
    doc: T,
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

fn due_for_purge(event: &EventDoc, now: FirestoreInstant) -> bool {
    event.status == EventStatus::Closed && event.purge_at.is_some_and(|at| at <= now)
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

// ---- documents the guards read ----

#[derive(Deserialize)]
struct PersonRow {
    #[serde(default)]
    event_ids: Vec<String>,
    #[serde(default)]
    household_id: Option<String>,
}

#[derive(Deserialize)]
struct HouseholdRow {
    #[serde(default)]
    member_ids: Vec<String>,
}

#[derive(Deserialize)]
struct CardRow {
    household_id: String,
}

#[derive(Deserialize)]
struct VolunteerRow {
    #[serde(default)]
    banned: bool,
    #[serde(default)]
    global_roles: Vec<GlobalRole>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    last_seen_at: FirestoreInstant,
}

#[derive(Serialize, Deserialize)]
struct PseudonymDoc {
    event_id: String,
    pseudonym: String,
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

// ---- the pass ----

/// One thing to delete. `Person`, `Household`, `Card` and `Volunteer` are re-checked inside the
/// transaction that deletes them.
#[derive(Clone)]
enum Op {
    Plain(&'static str, String),
    Person(String),
    Household(String),
    Card(String),
    Volunteer(String),
}

impl Op {
    fn label(&self) -> &'static str {
        match self {
            Op::Plain(collection, _) => collection,
            Op::Person(_) => people::COLLECTION,
            Op::Household(_) => households::COLLECTION,
            Op::Card(_) => cards::COLLECTION,
            Op::Volunteer(_) => volunteers::COLLECTION,
        }
    }

    fn per_tx(&self) -> usize {
        match self {
            Op::Household(_) => HOUSEHOLDS_PER_TX,
            Op::Volunteer(_) => 1,
            _ => DELETES_PER_TX,
        }
    }
}

struct Target<'a> {
    action: &'a str,
    kind: &'a str,
    id: &'a str,
}

struct Pass<'a> {
    state: &'a AppState,
    dry: bool,
    cap: u32,
    now: FirestoreInstant,
    day: String,
    /// Deletions the day still allows (`u32::MAX` in a dry run).
    remaining: u32,
    report: PurgeReport,
    /// What this pass changed or would change, by collection.
    totals: Counts,
    /// Dry run only: what the earlier steps would have deleted (`person:`, `household:`, `card:`).
    planned: HashSet<String>,
    /// Dry run only: the counts of each event, for `purge.dry_run`.
    dry_by_event: BTreeMap<String, Counts>,
    cache: HashMap<String, bool>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventStats;

    const AUDIT_LOG: &str = "audit_log";
    use crate::alerts::{AlertDoc, AlertKind};
    use crate::cards::{CardDoc, CardStatus};
    use crate::deliveries::{DeliveryStatus, ItemScope, Verification};
    use crate::exits::fixtures::person;
    use crate::households::HouseholdDoc;
    use crate::matches::{MatchDoc, MatchStatus, MediatorKind};
    use crate::people::{Level, PersonDoc};
    use crate::rescues::RescueDoc;
    use crate::searches::{Family, MissingPerson, SearchDoc, SearchStatus, SearchVia};
    use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, new_id};
    use crate::volunteers::VolunteerDoc;
    use serde_json::json;
    use std::sync::Arc;

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
        let [gone, recent, banned, validator, helper] =
            <[Actor; 5]>::try_from(joined).ok().unwrap();
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
}
