//! The counts, scans and caches that read Firestore to build the answers.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use firestore::select_filter_builder::FirestoreQueryFilterBuilder;
use firestore::{FirestoreDb, FirestoreQueryFilter, path};
use jiff::tz::TimeZone;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use super::domain::{AGE_KEYS, Cut, DISABILITIES, cap, count_value, cuts, variant};
use super::model::{
    ByLevel, Counts, EventDashboard, EventRow, EventSummary, Movement, NOTICE, OpenMatches,
    PilotIndicators, ShelterRow, ShelterToConfirm,
};
use crate::alerts::AlertDoc;
use crate::error::ApiError;
use crate::events;
use crate::exits;
use crate::households::{self, Pet};
use crate::matches::{self, MatchDoc};
use crate::people::{self, ExitReason, PersonDoc, Stay};
use crate::rescues::{self, RescueDoc};
use crate::shelters::{self, Confirmation, ShelterDoc, ShelterStatus};
use crate::store::{self, fs_error};
use crate::volunteers::{self, VolunteerDoc};

/// How long a built panel is served again before Firestore is asked (spec 6.3.5: reads are scarce).
// The cache is per process: with more than one API instance each one builds its own copy
// (docs/pt_br/dividas-tecnicas.md, DT-01).
pub(super) const CACHE_TTL: Duration = Duration::from_secs(30);
/// The list is the screen the Civil Defense leaves open: a few counts per event, so it is rebuilt
/// less often than the detail.
pub(super) const LIST_TTL: Duration = Duration::from_secs(300);
/// The exits cost one read per person of the event, so they are rebuilt less often than the counts.
const MOVES_TTL: Duration = Duration::from_secs(600);
/// People read to build the exits of one event.
/// Past it the exits and movements are marked partial (`partial_indicators` has `exits`).
const MOVES_SCAN_LIMIT: u32 = 10_000;
/// Households read per shelter to add up the animals.
/// Past it the animals of that shelter are marked partial (`pets`).
const PETS_SCAN_LIMIT: u32 = 2_000;
/// Shelters of one event; past it the event is marked partial (`shelters`).
pub(super) const SHELTER_LIMIT: u32 = 500;
/// Events in the list; past it the list is marked partial.
pub(super) const EVENT_LIMIT: u32 = 100;
/// Aggregation queries in flight at once, for the whole process.
const COUNT_CONCURRENCY: usize = 16;
/// Entries one cache keeps; past it the entry touched longest ago is dropped.
const CACHE_MAX_ENTRIES: usize = 256;
pub(super) static COUNT_LIMITER: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(COUNT_CONCURRENCY)));
/// The captures that spend a Unico process on a person being served.
const CAPTURE_PURPOSES: [&str; 3] = ["checkin", "revalidation", "biometrics"];
pub(super) const CAPTURES: &str = "captures";

// ---- cache ----

type Slot<T> = Arc<tokio::sync::Mutex<Option<(Instant, T)>>>;

/// A cache of built values by key. One build per key at a time: whoever arrives while it runs
/// waits for it and reads its value. At most `max` keys are kept.
pub(super) struct TtlCache<T> {
    pub(super) slots: Mutex<BTreeMap<String, (Instant, Slot<T>)>>,
    pub(super) max: usize,
}

impl<T: Clone> TtlCache<T> {
    pub(super) const fn new(max: usize) -> Self {
        Self {
            slots: Mutex::new(BTreeMap::new()),
            max,
        }
    }

    /// The value built less than `ttl` ago, or a new one. A failed build is not cached; the next
    /// caller in line builds again.
    pub(super) async fn get_or<F, Fut>(
        &self,
        key: &str,
        ttl: Duration,
        build: F,
    ) -> Result<T, ApiError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, ApiError>>,
    {
        let slot = {
            let mut slots = self.slots.lock().unwrap();
            if !slots.contains_key(key) && slots.len() >= self.max {
                let oldest = slots
                    .iter()
                    .min_by_key(|(_, (touched, _))| *touched)
                    .map(|(k, _)| k.clone());
                if let Some(oldest) = oldest {
                    slots.remove(&oldest);
                }
            }
            let entry = slots
                .entry(key.to_string())
                .or_insert_with(|| (Instant::now(), Slot::default()));
            entry.0 = Instant::now();
            entry.1.clone()
        };
        let mut built = slot.lock().await;
        if let Some((at, value)) = built.as_ref() {
            if at.elapsed() < ttl {
                return Ok(value.clone());
            }
        }
        let value = build().await?;
        *built = Some((Instant::now(), value.clone()));
        Ok(value)
    }

    #[cfg(test)]
    pub(super) fn remove(&self, key: &str) {
        self.slots.lock().unwrap().remove(key);
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.slots.lock().unwrap().len()
    }
}

#[derive(Clone, Default)]
pub(super) struct Moves {
    pub(super) movement: Movement,
    pub(super) exits: BTreeMap<String, u32>,
    /// `false` when the scan hit its limit.
    pub(super) complete: bool,
}

pub(super) static LIST: TtlCache<Value> = TtlCache::new(CACHE_MAX_ENTRIES);
pub(super) static DETAIL: TtlCache<EventDashboard> = TtlCache::new(CACHE_MAX_ENTRIES);
static MOVES: TtlCache<Moves> = TtlCache::new(CACHE_MAX_ENTRIES);

// Test only: the scans of people (`pets_of`, `scan_moves`) this thread has started.
#[cfg(test)]
thread_local! {
    pub(super) static SCANS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn cut_filter(
    q: &FirestoreQueryFilterBuilder,
    shelter_id: &str,
    cut: Cut,
) -> Option<FirestoreQueryFilter> {
    let here = q.field(path!(PersonDoc::current_shelter_id)).eq(shelter_id);
    let more = match cut {
        Cut::Present => None,
        Cut::Level(level) => q.field(path!(PersonDoc::level)).eq(variant(&level)),
        Cut::Born(from, to) => q.for_all([
            from.and_then(|y| {
                q.field(path!(PersonDoc::birth_year))
                    .greater_than_or_equal(y)
            }),
            to.and_then(|y| q.field(path!(PersonDoc::birth_year)).less_than_or_equal(y)),
        ]),
        Cut::Pregnant => q.field("vulnerabilities.pregnant").eq(true),
        Cut::Lactating => q.field("vulnerabilities.lactating").eq(true),
        Cut::Disability(kind) => q
            .field("vulnerabilities.disability")
            .array_contains(variant(&kind)),
        Cut::Child(status) => q.field(path!(PersonDoc::child_status)).eq(variant(&status)),
        Cut::Indigenous => q.field("vulnerabilities.indigenous_people").is_not_null(),
        Cut::Migrant => q.field("vulnerabilities.migrant_language").is_not_null(),
        Cut::Declined => q.field(path!(PersonDoc::biometrics_declined)).eq(true),
    };
    q.for_all([here, more])
}

/// `count()` of the documents `filter` selects: one read per thousand index entries, not per document.
pub(super) async fn count_with<F>(
    db: &FirestoreDb,
    limiter: &Semaphore,
    collection: &str,
    filter: F,
) -> Result<u32, ApiError>
where
    F: Fn(&FirestoreQueryFilterBuilder) -> Option<FirestoreQueryFilter>,
{
    #[derive(Deserialize)]
    struct Count {
        n: i64,
    }
    let _permit = limiter.acquire().await.map_err(|_| ApiError::internal())?;
    let rows: Vec<Count> = db
        .fluent()
        .select()
        .from(collection)
        .filter(|q| filter(&q))
        .aggregate(|a| a.fields([a.field("n").count()]))
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    rows.first().map_or(Ok(0), |c| count_value(c.n))
}

async fn run_cuts(
    db: &FirestoreDb,
    limiter: &Arc<Semaphore>,
    shelter_id: &str,
    year: i64,
) -> Result<BTreeMap<String, u32>, ApiError> {
    let mut tasks = JoinSet::new();
    for (key, cut) in cuts(year) {
        let (db, limiter, shelter_id) = (db.clone(), limiter.clone(), shelter_id.to_string());
        tasks.spawn(async move {
            let n = count_with(&db, &limiter, people::COLLECTION, |q| {
                cut_filter(q, &shelter_id, cut)
            })
            .await?;
            Ok::<_, ApiError>((key, n))
        });
    }
    let mut counted = BTreeMap::new();
    while let Some(done) = tasks.join_next().await {
        let (key, n) = done.map_err(|_| ApiError::internal())??;
        counted.insert(key, n);
    }
    Ok(counted)
}

/// The animals in the households created in a shelter, and whether the scan was cut at `limit`.
pub(super) async fn pets_of(
    db: &FirestoreDb,
    shelter_id: &str,
    limit: u32,
) -> Result<(u32, bool), ApiError> {
    #[cfg(test)]
    SCANS.with(|n| n.set(n.get() + 1));
    #[derive(Deserialize)]
    struct Pets {
        #[serde(default)]
        pets: Vec<Pet>,
    }
    let households: Vec<Pets> = db
        .fluent()
        .select()
        .fields(["pets"])
        .from(households::COLLECTION)
        .filter(|q| q.for_all([q.field("created_shelter_id").eq(shelter_id)]))
        .limit(limit + 1)
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    let (households, cut) = cap(households, limit);
    let pets = households
        .iter()
        .flat_map(|h| &h.pets)
        .map(|pet| pet.count)
        .sum();
    Ok((pets, cut))
}

struct Built {
    pub(super) row: ShelterRow,
    pub(super) declined: u32,
    pub(super) captures: u32,
    pub(super) pets_partial: bool,
}

async fn build_shelter(
    db: FirestoreDb,
    limiter: Arc<Semaphore>,
    shelter: ShelterDoc,
    year: i64,
) -> Result<Built, ApiError> {
    let id = shelter.id.clone().unwrap_or_default();
    let (present, households, captures) = tokio::try_join!(
        count_with(&db, &limiter, people::COLLECTION, |q| cut_filter(
            q,
            &id,
            Cut::Present
        )),
        count_with(&db, &limiter, households::COLLECTION, |q| q
            .for_all([q.field("created_shelter_id").eq(id.as_str())])),
        count_with(&db, &limiter, CAPTURES, |q| q.for_all([
            q.field("shelter_id").eq(id.as_str()),
            q.field("purpose").is_in(CAPTURE_PURPOSES.to_vec()),
        ])),
    )?;
    // Nobody in the shelter: every cut is zero, so the many queries are skipped.
    let counted = if present > 0 {
        run_cuts(&db, &limiter, &id, year).await?
    } else {
        BTreeMap::new()
    };
    let (pets, pets_partial) = if households > 0 {
        pets_of(&db, &id, PETS_SCAN_LIMIT).await?
    } else {
        (0, false)
    };
    let get = |key: &str| counted.get(key).copied().unwrap_or(0);
    let mut age_bands: BTreeMap<String, u32> =
        AGE_KEYS.iter().map(|k| (k.to_string(), get(k))).collect();
    let in_bands: u32 = age_bands.values().sum();
    age_bands.insert("unknown".into(), present.saturating_sub(in_bands));
    let counts = Counts {
        unique_people: present,
        by_level: ByLevel {
            verified: get("verified"),
            assisted: get("assisted"),
            no_biometrics: get("no_biometrics"),
        },
        age_bands,
        pregnant: get("pregnant"),
        lactating: get("lactating"),
        disability: DISABILITIES
            .iter()
            .map(|kind| {
                let kind = variant(kind);
                let n = get(&format!("disability_{kind}"));
                (kind, n)
            })
            .collect(),
        children_separated: get("separated"),
        children_unaccompanied: get("unaccompanied"),
        indigenous: get("indigenous"),
        migrant: get("migrant"),
        households,
        pets,
    };
    Ok(Built {
        row: ShelterRow {
            id,
            name: shelter.name,
            confirmation: variant(&shelter.confirmation),
            flagged: shelter.flagged,
            lat: shelter.lat,
            lon: shelter.lon,
            capacity: shelter.capacity,
            occupancy: present,
            vacancies: exits::vacancies(shelter.capacity, present),
            counts,
        },
        declined: get("declined"),
        captures,
        pets_partial,
    })
}

// ---- loading ----

/// The bounds of the reads of one build; the tests pass small ones.
#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub(super) shelters: u32,
    pub(super) moves: u32,
}

pub(super) const LIMITS: Limits = Limits {
    shelters: SHELTER_LIMIT,
    moves: MOVES_SCAN_LIMIT,
};

fn event_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "event_not_found")
}

/// The shelters of an event (at most `limit`) and whether there were more.
async fn shelters_of(
    db: &FirestoreDb,
    event_id: &str,
    limit: u32,
) -> Result<(Vec<ShelterDoc>, bool), ApiError> {
    let rows = db
        .fluent()
        .select()
        .from(shelters::COLLECTION)
        .filter(|q| q.for_all([q.field(path!(ShelterDoc::event_id)).eq(event_id)]))
        .limit(limit + 1)
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    Ok(cap(rows, limit))
}

/// The shelters of the event still open and without a confirmation, with who registered each.
async fn to_confirm(
    db: &FirestoreDb,
    shelters: &[ShelterDoc],
) -> Result<Vec<ShelterToConfirm>, ApiError> {
    let mut waiting: Vec<&ShelterDoc> = shelters
        .iter()
        .filter(|s| s.confirmation == Confirmation::Unconfirmed && s.status == ShelterStatus::Open)
        .collect();
    waiting.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    let mut list = Vec::with_capacity(waiting.len());
    for shelter in waiting {
        let creator: Option<VolunteerDoc> =
            store::get(db, volunteers::COLLECTION, &shelter.created_by).await?;
        list.push(ShelterToConfirm {
            id: shelter.id.clone().unwrap_or_default(),
            name: shelter.name.clone(),
            created_at: shelter.created_at.to_string(),
            flagged: shelter.flagged,
            city: shelter.address.city.clone(),
            state: shelter.address.state.clone(),
            lat: shelter.lat,
            lon: shelter.lon,
            created_by_name: creator.map(|c| c.name),
        });
    }
    Ok(list)
}

pub(super) fn current_year() -> i64 {
    i64::from(store::now().to_zoned(TimeZone::UTC).year())
}

/// The exits of the event: stays closed by a check-in elsewhere in the last 24 h and 7 days, and
/// the other closed stays by `exit_destination`.
pub(super) async fn scan_moves(
    db: &FirestoreDb,
    event_id: &str,
    limit: u32,
) -> Result<Moves, ApiError> {
    #[cfg(test)]
    SCANS.with(|n| n.set(n.get() + 1));
    #[derive(Deserialize)]
    struct Stays {
        #[serde(default)]
        stays: Vec<Stay>,
    }
    let people: Vec<Stays> = db
        .fluent()
        .select()
        .fields(["stays"])
        .from(people::COLLECTION)
        .filter(|q| {
            q.for_all([q
                .field(path!(PersonDoc::event_ids))
                .array_contains(event_id)])
        })
        .limit(limit + 1)
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    let (people, cut) = cap(people, limit);
    const HOUR_US: i64 = 3_600 * 1_000_000;
    let now = store::now().as_microsecond();
    let mut moves = Moves {
        complete: !cut,
        ..Moves::default()
    };
    for stay in people
        .iter()
        .flat_map(|p| &p.stays)
        .filter(|s| s.event_id.as_deref() == Some(event_id))
    {
        let Some(left_at) = stay.left_at else {
            continue;
        };
        if stay.exit_reason == Some(ExitReason::CheckinElsewhere) {
            let ago = now - left_at.as_microsecond();
            moves.movement.last_24h += u32::from(ago <= 24 * HOUR_US);
            moves.movement.last_7d += u32::from(ago <= 7 * 24 * HOUR_US);
        }
        if let Some(destination) = stay.exit_destination {
            *moves.exits.entry(variant(&destination)).or_default() += 1;
        }
    }
    Ok(moves)
}

pub(super) async fn build_event(
    db: &FirestoreDb,
    limiter: &Arc<Semaphore>,
    event_id: &str,
    limits: Limits,
) -> Result<EventDashboard, ApiError> {
    let event: EventRow = store::get(db, events::COLLECTION, event_id)
        .await?
        .ok_or_else(event_not_found)?;
    let (shelters, shelters_cut) = shelters_of(db, event_id, limits.shelters).await?;
    let year = current_year();
    let mut tasks = JoinSet::new();
    for shelter in shelters.iter().cloned() {
        tasks.spawn(build_shelter(db.clone(), limiter.clone(), shelter, year));
    }
    let mut built = vec![];
    while let Some(done) = tasks.join_next().await {
        built.push(done.map_err(|_| ApiError::internal())??);
    }
    built.sort_by(|a, b| (&a.row.name, &a.row.id).cmp(&(&b.row.name, &b.row.id)));

    let mut totals = Counts::default();
    let (mut declined, mut captures) = (0, 0);
    let pets_partial = built.iter().any(|b| b.pets_partial);
    for b in &built {
        totals.add(&b.row.counts);
        declined += b.declined;
        captures += b.captures;
    }
    let moves_key = format!("{}/{event_id}", db.get_database_path());
    let moves = MOVES
        .get_or(&moves_key, MOVES_TTL, || {
            scan_moves(db, event_id, limits.moves)
        })
        .await?;
    let (rescues, open_alerts, central, protection, not_the_person) = tokio::try_join!(
        count_with(db, limiter, rescues::COLLECTION, |q| q
            .for_all([q.field(path!(RescueDoc::event_id)).eq(event_id)])),
        count_with(db, limiter, crate::alerts::COLLECTION, |q| q.for_all([
            q.field(path!(AlertDoc::event_id)).eq(event_id),
            q.field(path!(AlertDoc::resolved)).eq(false),
        ])),
        open_matches(db, limiter, "central"),
        open_matches(db, limiter, "protection"),
        count_with(db, limiter, matches::COLLECTION, |q| q
            .for_all([q.field(path!(MatchDoc::status)).eq("not_the_person")])),
    )?;
    let people = f64::from(totals.unique_people);
    let pilot = PilotIndicators {
        selfie_refusal_rate: (people > 0.0).then(|| f64::from(declined) / people),
        not_the_person,
        other_site_reasons: u32::try_from(
            shelters
                .iter()
                .filter(|s| s.other_site_reason.is_some())
                .count(),
        )
        .unwrap_or(u32::MAX),
        unico_processes_per_person: (people > 0.0).then(|| f64::from(captures) / people),
    };
    let partial_indicators: Vec<&'static str> = [
        (shelters_cut, "shelters"),
        (!moves.complete, "exits"),
        (pets_partial, "pets"),
    ]
    .into_iter()
    .filter_map(|(cut, name)| cut.then_some(name))
    .collect();
    Ok(EventDashboard {
        id: event_id.to_string(),
        name: event.name,
        status: variant(&event.status),
        notice: NOTICE,
        generated_at: store::now().to_string(),
        complete: partial_indicators.is_empty(),
        partial_indicators,
        totals,
        shelters: built.into_iter().map(|b| b.row).collect(),
        movement: moves.movement,
        exits_by_destination: moves.exits,
        rescues,
        open_alerts,
        open_matches: OpenMatches {
            central,
            protection,
        },
        pilot,
        unconfirmed_shelters: to_confirm(db, &shelters).await?,
    })
}

/// The queues have no event on the match: the count is the whole network's queue (spec 4.8).
async fn open_matches(
    db: &FirestoreDb,
    limiter: &Semaphore,
    queue: &'static str,
) -> Result<u32, ApiError> {
    count_with(db, limiter, matches::COLLECTION, |q| {
        q.for_all([
            q.field(path!(MatchDoc::mediator_kind)).eq(queue),
            q.field(path!(MatchDoc::status)).eq("open"),
        ])
    })
    .await
}

pub(super) async fn build_list(
    db: &FirestoreDb,
    event_limit: u32,
    shelter_limit: u32,
) -> Result<Value, ApiError> {
    let limiter = COUNT_LIMITER.clone();
    let events: Vec<EventRow> = db
        .fluent()
        .select()
        .from(events::COLLECTION)
        .order_by([("created_at", firestore::FirestoreQueryDirection::Descending)])
        .limit(event_limit + 1)
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    let (events, mut partial) = cap(events, event_limit);
    let mut list = Vec::with_capacity(events.len());
    for event in events {
        let id = event.id.clone().unwrap_or_default();
        let (shelters, shelters_cut) = shelters_of(db, &id, shelter_limit).await?;
        partial |= shelters_cut;
        let mut tasks = JoinSet::new();
        for shelter in &shelters {
            let (db, limiter, shelter_id) = (
                db.clone(),
                limiter.clone(),
                shelter.id.clone().unwrap_or_default(),
            );
            tasks.spawn(async move {
                count_with(&db, &limiter, people::COLLECTION, |q| {
                    cut_filter(q, &shelter_id, Cut::Present)
                })
                .await
            });
        }
        let mut occupancy = 0;
        while let Some(done) = tasks.join_next().await {
            occupancy += done.map_err(|_| ApiError::internal())??;
        }
        list.push(EventSummary {
            id,
            name: event.name,
            status: variant(&event.status),
            created_at: event.created_at.to_string(),
            shelters: u32::try_from(shelters.len()).unwrap_or(u32::MAX),
            unique_people: occupancy,
            occupancy,
            unconfirmed_shelters: to_confirm(db, &shelters).await?,
        });
    }
    Ok(json!({
        "events": list,
        "generated_at": store::now().to_string(),
        "complete": !partial,
    }))
}
