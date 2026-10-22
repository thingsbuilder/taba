//! The Civil Defense dashboard (spec 3.10, 8.8): `GET /v1/dashboard/events`,
//! `GET /v1/dashboard/events/{id}` and `GET /v1/dashboard/events/{id}/daily.csv`, for a validator
//! (read) and the Civil Defense.
//!
//! Every cut counts the people in a shelter now (`current_shelter_id`), which is what the fixed
//! notice says the panel counts; whoever already left shows in the exits by destination. The
//! event's totals are the sum of its shelters. Counts use the Firestore `count()` aggregation; the
//! two things an aggregation cannot do (the exits, which live inside the `stays` array, and the
//! animals, which live inside the households) are bounded scans.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use firestore::select_filter_builder::FirestoreQueryFilterBuilder;
use firestore::{FirestoreDb, FirestoreQueryFilter, path};
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::alerts::AlertDoc;
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::events::{self, EventStatus};
use crate::exits;
use crate::households::{self, Pet};
use crate::matches::{self, MatchDoc};
use crate::people::{self, ChildStatus, Disability, ExitReason, Level, PersonDoc, Stay};
use crate::rescues::{self, RescueDoc};
use crate::shelters::{self, Confirmation, ShelterDoc, ShelterStatus};
use crate::store::{self, fs_error};
use crate::volunteers::{self, VolunteerDoc};

/// The notice the panel shows at the top (spec 3.10).
pub const NOTICE: &str = "Conta desabrigados (pessoas em abrigo). Desalojados (em casa de parentes ou amigos) não estão aqui.";

/// How long a built panel is served again before Firestore is asked (spec 6.3.5: reads are scarce).
// ponytail: per-process cache on a single instance, no lock against two builds at once; swap for
// a shared cache if the API ever runs on more than one instance.
const CACHE_TTL: Duration = Duration::from_secs(30);
/// The exits cost one read per person of the event, so they are rebuilt less often than the counts.
const MOVES_TTL: Duration = Duration::from_secs(600);
/// People read to build the exits of one event.
// ponytail: a bounded scan; past this many people the exits undercount. Keep the counters in the
// event document (written by the exit and check-in routes) when an event grows past it.
const MOVES_SCAN_LIMIT: u32 = 10_000;
/// Households read per shelter to add up the animals.
// ponytail: a bounded scan; same upgrade as above if a shelter has more households.
const PETS_SCAN_LIMIT: u32 = 2_000;
const SHELTER_LIMIT: u32 = 500;
const EVENT_LIMIT: u32 = 100;
/// Aggregation queries in flight at once.
const COUNT_CONCURRENCY: usize = 16;
/// The captures that spend a Unico process on a person being served.
const CAPTURE_PURPOSES: [&str; 3] = ["checkin", "revalidation", "biometrics"];
const CAPTURES: &str = "captures";
const AGE_KEYS: [&str; 4] = ["0_11", "12_17", "18_59", "60_plus"];
const DISABILITIES: [Disability; 6] = [
    Disability::Mobility,
    Disability::Visual,
    Disability::Hearing,
    Disability::Intellectual,
    Disability::Psychosocial,
    Disability::Other,
];

/// A shelter that waits for a confirmation, for the list of whoever may confirm it.
#[derive(Debug, Clone, Serialize)]
pub struct ShelterToConfirm {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub flagged: bool,
    pub city: Option<String>,
    pub state: Option<String>,
    pub lat: f64,
    pub lon: f64,
    /// The volunteer who registered it.
    pub created_by_name: Option<String>,
}

/// One event of `GET /v1/dashboard/events`; `unconfirmed_shelters` is how a validator or the Civil
/// Defense finds the shelters to confirm without knowing an id.
#[derive(Debug, Clone, Serialize)]
pub struct EventSummary {
    pub id: String,
    pub name: String,
    pub status: String,
    pub created_at: String,
    pub shelters: u32,
    pub unique_people: u32,
    pub occupancy: u32,
    pub unconfirmed_shelters: Vec<ShelterToConfirm>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ByLevel {
    pub verified: u32,
    pub assisted: u32,
    pub no_biometrics: u32,
}

/// The counts of the cuts of spec 3.10, for an event or for one shelter.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Counts {
    pub unique_people: u32,
    pub by_level: ByLevel,
    /// `0_11`, `12_17`, `18_59`, `60_plus` and `unknown` (no `birth_year`).
    pub age_bands: BTreeMap<String, u32>,
    pub pregnant: u32,
    pub lactating: u32,
    /// By kind (`mobility`, `visual`, ...).
    pub disability: BTreeMap<String, u32>,
    pub children_separated: u32,
    pub children_unaccompanied: u32,
    pub indigenous: u32,
    pub migrant: u32,
    pub households: u32,
    pub pets: u32,
}

impl Counts {
    fn add(&mut self, other: &Counts) {
        self.unique_people += other.unique_people;
        self.by_level.verified += other.by_level.verified;
        self.by_level.assisted += other.by_level.assisted;
        self.by_level.no_biometrics += other.by_level.no_biometrics;
        for (key, n) in &other.age_bands {
            *self.age_bands.entry(key.clone()).or_default() += n;
        }
        for (key, n) in &other.disability {
            *self.disability.entry(key.clone()).or_default() += n;
        }
        self.pregnant += other.pregnant;
        self.lactating += other.lactating;
        self.children_separated += other.children_separated;
        self.children_unaccompanied += other.children_unaccompanied;
        self.indigenous += other.indigenous;
        self.migrant += other.migrant;
        self.households += other.households;
        self.pets += other.pets;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ShelterRow {
    pub id: String,
    pub name: String,
    pub confirmation: String,
    pub flagged: bool,
    pub lat: f64,
    pub lon: f64,
    pub capacity: Option<u32>,
    pub occupancy: u32,
    pub vacancies: Option<u32>,
    pub counts: Counts,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Movement {
    pub last_24h: u32,
    pub last_7d: u32,
}

/// Matches waiting in the queues that need a person's attention (4.8).
#[derive(Debug, Clone, Default, Serialize)]
pub struct OpenMatches {
    pub central: u32,
    pub protection: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PilotIndicators {
    /// Share of people who refused the selfie, 0 to 1; absent with nobody registered.
    pub selfie_refusal_rate: Option<f64>,
    pub not_the_person: u32,
    pub other_site_reasons: u32,
    pub unico_processes_per_person: Option<f64>,
}

/// `GET /v1/dashboard/events/{id}` (spec 3.10).
#[derive(Debug, Clone, Serialize)]
pub struct EventDashboard {
    pub id: String,
    pub name: String,
    pub status: String,
    /// The fixed notice of spec 3.10.
    pub notice: &'static str,
    pub totals: Counts,
    pub shelters: Vec<ShelterRow>,
    pub movement: Movement,
    /// By `exit_destination` (`own_home`, `relatives_or_friends`, ...).
    pub exits_by_destination: BTreeMap<String, u32>,
    pub rescues: u32,
    pub open_alerts: u32,
    pub open_matches: OpenMatches,
    pub pilot: PilotIndicators,
    pub unconfirmed_shelters: Vec<ShelterToConfirm>,
}

// ---- cache ----

struct TtlCache<T>(Mutex<BTreeMap<String, (Instant, T)>>);

impl<T: Clone> TtlCache<T> {
    const fn new() -> Self {
        Self(Mutex::new(BTreeMap::new()))
    }

    /// The value built less than `ttl` ago, or a new one.
    async fn get_or<F, Fut>(&self, key: &str, ttl: Duration, build: F) -> Result<T, ApiError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, ApiError>>,
    {
        if let Some((at, value)) = self.0.lock().unwrap().get(key) {
            if at.elapsed() < ttl {
                return Ok(value.clone());
            }
        }
        let value = build().await?;
        self.0
            .lock()
            .unwrap()
            .insert(key.to_string(), (Instant::now(), value.clone()));
        Ok(value)
    }

    #[cfg(test)]
    fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

#[derive(Clone, Default)]
struct Moves {
    movement: Movement,
    exits: BTreeMap<String, u32>,
}

static LIST: TtlCache<Value> = TtlCache::new();
static DETAIL: TtlCache<EventDashboard> = TtlCache::new();
static MOVES: TtlCache<Moves> = TtlCache::new();

// ---- counting ----

/// One cut of the people in a shelter.
#[derive(Clone, Copy)]
enum Cut {
    Present,
    Level(Level),
    /// Inclusive bounds on `birth_year`.
    Born(Option<i64>, Option<i64>),
    Pregnant,
    Lactating,
    Disability(Disability),
    Child(ChildStatus),
    Indigenous,
    Migrant,
    Declined,
}

fn variant<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
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
async fn count_with<F>(
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
    Ok(rows.first().map_or(0, |c| u32::try_from(c.n).unwrap_or(0)))
}

/// The cuts that run once a shelter is known to have someone in it, by the key the row reads them by.
fn cuts(year: i64) -> Vec<(String, Cut)> {
    let mut all = vec![
        ("verified".to_string(), Cut::Level(Level::Verified)),
        ("assisted".to_string(), Cut::Level(Level::Assisted)),
        ("no_biometrics".to_string(), Cut::Level(Level::NoBiometrics)),
        ("0_11".to_string(), Cut::Born(Some(year - 11), None)),
        (
            "12_17".to_string(),
            Cut::Born(Some(year - 17), Some(year - 12)),
        ),
        (
            "18_59".to_string(),
            Cut::Born(Some(year - 59), Some(year - 18)),
        ),
        ("60_plus".to_string(), Cut::Born(None, Some(year - 60))),
        ("pregnant".to_string(), Cut::Pregnant),
        ("lactating".to_string(), Cut::Lactating),
        ("separated".to_string(), Cut::Child(ChildStatus::Separated)),
        (
            "unaccompanied".to_string(),
            Cut::Child(ChildStatus::Unaccompanied),
        ),
        ("indigenous".to_string(), Cut::Indigenous),
        ("migrant".to_string(), Cut::Migrant),
        ("declined".to_string(), Cut::Declined),
    ];
    all.extend(DISABILITIES.iter().map(|kind| {
        (
            format!("disability_{}", variant(kind)),
            Cut::Disability(*kind),
        )
    }));
    all
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

async fn pets_of(db: &FirestoreDb, shelter_id: &str) -> Result<u32, ApiError> {
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
        .limit(PETS_SCAN_LIMIT)
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    Ok(households
        .iter()
        .flat_map(|h| &h.pets)
        .map(|pet| pet.count)
        .sum())
}

struct Built {
    row: ShelterRow,
    declined: u32,
    captures: u32,
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
    let pets = if households > 0 {
        pets_of(&db, &id).await?
    } else {
        0
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
    })
}

// ---- loading ----

#[derive(Deserialize)]
struct EventRow {
    #[serde(alias = "_firestore_id", default)]
    id: Option<String>,
    name: String,
    status: EventStatus,
    #[serde(with = "firestore::serialize_as_timestamp")]
    created_at: firestore::FirestoreInstant,
}

fn event_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "event_not_found")
}

async fn shelters_of(db: &FirestoreDb, event_id: &str) -> Result<Vec<ShelterDoc>, ApiError> {
    db.fluent()
        .select()
        .from(shelters::COLLECTION)
        .filter(|q| q.for_all([q.field(path!(ShelterDoc::event_id)).eq(event_id)]))
        .limit(SHELTER_LIMIT)
        .obj()
        .query()
        .await
        .map_err(fs_error)
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

fn current_year() -> i64 {
    i64::from(store::now().to_zoned(TimeZone::UTC).year())
}

/// The exits of the event: stays closed by a check-in elsewhere in the last 24 h and 7 days, and
/// the other closed stays by `exit_destination`.
async fn scan_moves(db: &FirestoreDb, event_id: &str) -> Result<Moves, ApiError> {
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
        .limit(MOVES_SCAN_LIMIT)
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    const HOUR_US: i64 = 3_600 * 1_000_000;
    let now = store::now().as_microsecond();
    let mut moves = Moves::default();
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

async fn build_event(
    db: &FirestoreDb,
    limiter: &Arc<Semaphore>,
    event_id: &str,
) -> Result<EventDashboard, ApiError> {
    let event: EventRow = store::get(db, events::COLLECTION, event_id)
        .await?
        .ok_or_else(event_not_found)?;
    let shelters = shelters_of(db, event_id).await?;
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
    for b in &built {
        totals.add(&b.row.counts);
        declined += b.declined;
        captures += b.captures;
    }
    let moves_key = format!("{}/{event_id}", db.get_database_path());
    let moves = MOVES
        .get_or(&moves_key, MOVES_TTL, || scan_moves(db, event_id))
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
    Ok(EventDashboard {
        id: event_id.to_string(),
        name: event.name,
        status: variant(&event.status),
        notice: NOTICE,
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

async fn build_list(db: &FirestoreDb) -> Result<Value, ApiError> {
    let limiter = Arc::new(Semaphore::new(COUNT_CONCURRENCY));
    let events: Vec<EventRow> = db
        .fluent()
        .select()
        .from(events::COLLECTION)
        .order_by([("created_at", firestore::FirestoreQueryDirection::Descending)])
        .limit(EVENT_LIMIT)
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    let mut list = Vec::with_capacity(events.len());
    for event in events {
        let id = event.id.clone().unwrap_or_default();
        let shelters = shelters_of(db, &id).await?;
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
    Ok(json!({ "events": list }))
}

// ---- CSV ----

/// A field of the CSV that a spreadsheet will not run as a formula (a text that starts with `=`,
/// `+`, `-`, `@`, a tab or a carriage return gets a leading `'`) and quotes when it needs to.
fn csv_field(value: &str) -> String {
    let guarded = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_string()
    };
    if guarded.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", guarded.replace('"', "\"\""))
    } else {
        guarded
    }
}

/// One line per shelter. The shelter's name is the only text; no person's name and no `person_id`.
fn daily_csv_body(dashboard: &EventDashboard, date: &str) -> String {
    let mut header = vec![
        "date",
        "event_id",
        "shelter_id",
        "shelter_name",
        "confirmation",
        "flagged",
        "lat",
        "lon",
        "capacity",
        "occupancy",
        "vacancies",
        "unique_people",
        "verified",
        "assisted",
        "no_biometrics",
    ]
    .into_iter()
    .map(String::from)
    .collect::<Vec<_>>();
    header.extend(
        AGE_KEYS
            .iter()
            .chain(&["unknown"])
            .map(|k| format!("age_{k}")),
    );
    header.extend(["pregnant".to_string(), "lactating".to_string()]);
    header.extend(
        DISABILITIES
            .iter()
            .map(|k| format!("disability_{}", variant(k))),
    );
    header.extend(
        [
            "children_separated",
            "children_unaccompanied",
            "indigenous",
            "migrant",
            "households",
            "pets",
        ]
        .map(String::from),
    );
    let mut out = header.join(",");
    out.push_str("\r\n");
    let opt = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
    for shelter in &dashboard.shelters {
        let c = &shelter.counts;
        let mut fields = vec![
            csv_field(date),
            csv_field(&dashboard.id),
            csv_field(&shelter.id),
            csv_field(&shelter.name),
            csv_field(&shelter.confirmation),
            shelter.flagged.to_string(),
            shelter.lat.to_string(),
            shelter.lon.to_string(),
            opt(shelter.capacity),
            shelter.occupancy.to_string(),
            opt(shelter.vacancies),
            c.unique_people.to_string(),
            c.by_level.verified.to_string(),
            c.by_level.assisted.to_string(),
            c.by_level.no_biometrics.to_string(),
        ];
        fields.extend(
            AGE_KEYS
                .iter()
                .chain(&["unknown"])
                .map(|k| c.age_bands.get(*k).copied().unwrap_or(0).to_string()),
        );
        fields.extend([c.pregnant.to_string(), c.lactating.to_string()]);
        fields.extend(DISABILITIES.iter().map(|k| {
            c.disability
                .get(&variant(k))
                .copied()
                .unwrap_or(0)
                .to_string()
        }));
        fields.extend(
            [
                c.children_separated,
                c.children_unaccompanied,
                c.indigenous,
                c.migrant,
                c.households,
                c.pets,
            ]
            .map(|n| n.to_string()),
        );
        out.push_str(&fields.join(","));
        out.push_str("\r\n");
    }
    out
}

// ---- routes ----

/// Only a validator (read) and the Civil Defense see the panel (spec 2.3).
fn require_reader(volunteer: &Volunteer) -> Result<(), ApiError> {
    if volunteer.is_validator_or_civil_defense() {
        Ok(())
    } else {
        Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden"))
    }
}

async fn dashboard_of(state: &AppState, event_id: &str) -> Result<EventDashboard, ApiError> {
    let key = format!("{}/{event_id}", state.db.get_database_path());
    DETAIL
        .get_or(&key, CACHE_TTL, || async {
            let limiter = Arc::new(Semaphore::new(COUNT_CONCURRENCY));
            build_event(&state.db, &limiter, event_id).await
        })
        .await
}

/// `GET /v1/dashboard/events`: `{events: [EventSummary]}`.
pub async fn list_events(
    State(state): State<AppState>,
    volunteer: Volunteer,
) -> Result<Json<Value>, ApiError> {
    require_reader(&volunteer)?;
    let list = LIST
        .get_or(state.db.get_database_path(), CACHE_TTL, || {
            build_list(&state.db)
        })
        .await?;
    Ok(Json(list))
}

/// `GET /v1/dashboard/events/{id}`: an `EventDashboard`.
pub async fn get_event(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(event_id): Path<String>,
) -> Result<Json<EventDashboard>, ApiError> {
    require_reader(&volunteer)?;
    Ok(Json(dashboard_of(&state, &event_id).await?))
}

/// `GET /v1/dashboard/events/{id}/daily.csv`: one line per shelter, no name of a person, no `person_id`.
pub async fn daily_csv(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(event_id): Path<String>,
) -> Result<Response, ApiError> {
    require_reader(&volunteer)?;
    let dashboard = dashboard_of(&state, &event_id).await?;
    let date = store::now().to_zoned(TimeZone::UTC).date().to_string();
    let body = daily_csv_body(&dashboard, &date);
    Ok((
        StatusCode::OK,
        [
            (CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (
                CONTENT_DISPOSITION,
                format!("attachment; filename=\"taba-{event_id}-{date}.csv\""),
            ),
            (CACHE_CONTROL, "no-store".to_string()),
        ],
        body,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::AlertKind;
    use crate::audit::LegalBasis;
    use crate::auth::{CaptureDoc, CaptureStatus, Purpose};
    use crate::matches::{MatchStatus, MediatorKind};
    use crate::people::{ExitDestination as Dest, Needs, Origin, Vulnerabilities};
    use crate::test_support::{Actor, ORIGIN, TestApp, new_id, north_of_origin};
    use crate::volunteers::GlobalRole;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const HOUR_US: i64 = 3_600 * 1_000_000;

    fn ago(hours: i64) -> firestore::FirestoreInstant {
        firestore::FirestoreInstant::from_microsecond(
            store::now().as_microsecond() - hours * HOUR_US,
        )
        .unwrap()
    }

    fn born(years_old: i64) -> Option<u16> {
        Some(u16::try_from(current_year() - years_old).unwrap())
    }

    /// An adult in `shelter` (of `event`) with nothing else set.
    fn person(level: Level, shelter: Option<&str>, event: &str) -> PersonDoc {
        let now = store::now();
        PersonDoc {
            id: None,
            level,
            biometrics_declined: false,
            former_ids: vec![],
            name: Some("Maria Secreta".into()),
            social_name: None,
            wristband: None,
            birth_year: born(30),
            is_child: false,
            child_status: None,
            guardians: vec![],
            guardian_ids: vec![],
            needs: Needs::default(),
            vulnerabilities: Vulnerabilities::default(),
            household_id: None,
            current_shelter_id: shelter.map(str::to_string),
            stays: vec![],
            event_ids: vec![event.to_string()],
            rescue_ids: vec![],
            thumbnail_encrypted: None,
            origin: Origin::Checkin,
            legal_basis: LegalBasis::Consent,
            created_at: now,
            updated_at: now,
            client_edited_at: None,
        }
    }

    async fn put_person(app: &TestApp, doc: &PersonDoc) -> String {
        let id = new_id();
        store::replace(&app.state.db, people::COLLECTION, &id, doc)
            .await
            .unwrap();
        id
    }

    fn closed_stay(
        shelter: &str,
        event: &str,
        left_hours_ago: i64,
        reason: ExitReason,
        destination: Option<Dest>,
    ) -> Stay {
        Stay {
            shelter_id: shelter.into(),
            event_id: Some(event.into()),
            entered_at: ago(left_hours_ago + 1),
            left_at: Some(ago(left_hours_ago)),
            exit_reason: Some(reason),
            exit_destination: destination,
            destination_municipality: None,
            registered_by: "test".into(),
        }
    }

    async fn shelter_doc(app: &TestApp, id: &str) -> ShelterDoc {
        store::get(&app.state.db, shelters::COLLECTION, id)
            .await
            .unwrap()
            .unwrap()
    }

    async fn update_shelter(app: &TestApp, id: &str, edit: impl FnOnce(&mut ShelterDoc)) {
        let mut doc = shelter_doc(app, id).await;
        edit(&mut doc);
        store::replace(&app.state.db, shelters::COLLECTION, id, &doc)
            .await
            .unwrap();
    }

    struct World {
        app: TestApp,
        civil_defense: Actor,
        validator: Actor,
        ana: Actor,
        event: String,
        a: String,
        b: String,
        other_event: String,
        people_ids: Vec<String>,
    }

    /// Event E: shelter A (unconfirmed, flagged, capacity 10, 5 people) and shelter B (confirmed, no
    /// capacity, 6 people on the edges of the age bands); event O: one shelter with one person.
    async fn world() -> World {
        let app = TestApp::new().await;
        let ana = app.actor("Ana").await;
        let civil_defense = app.actor("Defesa").await;
        app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let validator = app.actor("Validador").await;
        app.grant(&validator, GlobalRole::Validator).await;

        let a = app.create_shelter(&ana, "Abrigo A", ORIGIN).await;
        let b = app
            .confirmed_shelter(&ana, "Abrigo B", north_of_origin(5_000.0))
            .await;
        let far = app.actor("Longe").await;
        let o = app
            .create_shelter(&far, "Abrigo O", north_of_origin(1_000_000.0))
            .await;
        let event = shelter_doc(&app, &a).await.event_id.unwrap();
        let other_event = shelter_doc(&app, &o).await.event_id.unwrap();
        assert_ne!(event, other_event);
        assert_eq!(shelter_doc(&app, &b).await.event_id.unwrap(), event);
        update_shelter(&app, &a, |s| {
            s.capacity = Some(10);
            s.flagged = true;
        })
        .await;
        update_shelter(&app, &b, |s| s.other_site_reason = Some("a reason".into())).await;

        let mut ids = vec![];
        // Shelter A.
        let mut p = person(Level::Verified, Some(&a), &event);
        p.birth_year = born(5);
        p.is_child = true;
        p.child_status = Some(ChildStatus::Separated);
        ids.push(put_person(&app, &p).await);
        let mut p = person(Level::Assisted, Some(&a), &event);
        p.birth_year = born(15);
        p.is_child = true;
        p.child_status = Some(ChildStatus::Unaccompanied);
        ids.push(put_person(&app, &p).await);
        let mut p = person(Level::NoBiometrics, Some(&a), &event);
        p.vulnerabilities.pregnant = true;
        p.vulnerabilities.disability = vec![Disability::Mobility, Disability::Visual];
        p.biometrics_declined = true;
        ids.push(put_person(&app, &p).await);
        let mut p = person(Level::Verified, Some(&a), &event);
        p.birth_year = born(70);
        p.vulnerabilities.lactating = true;
        p.vulnerabilities.indigenous_people = Some("guarani".into());
        ids.push(put_person(&app, &p).await);
        let mut p = person(Level::Assisted, Some(&a), &event);
        p.birth_year = None;
        p.vulnerabilities.migrant_language = Some("es".into());
        p.vulnerabilities.disability = vec![Disability::Mobility];
        ids.push(put_person(&app, &p).await);
        // Shelter B: the edges of the age bands.
        for years_old in [18, 17, 12, 11, 60, 59] {
            let mut p = person(Level::Verified, Some(&b), &event);
            p.birth_year = born(years_old);
            ids.push(put_person(&app, &p).await);
        }
        // The other event.
        ids.push(put_person(&app, &person(Level::Verified, Some(&o), &other_event)).await);
        // Left the shelters: the exits and movements of E.
        let mut gone = person(Level::Verified, None, &event);
        gone.stays = vec![
            closed_stay(&a, &event, 30, ExitReason::Exit, Some(Dest::OwnHome)),
            // Another event's stay on the same person: not an exit of E.
            closed_stay(&o, &other_event, 1, ExitReason::Exit, Some(Dest::RentalAid)),
        ];
        ids.push(put_person(&app, &gone).await);
        let mut gone = person(Level::Verified, None, &event);
        gone.stays = vec![closed_stay(
            &a,
            &event,
            100,
            ExitReason::ExceptionalExit,
            Some(Dest::RelativesOrFriends),
        )];
        ids.push(put_person(&app, &gone).await);
        for hours in [2, 72, 240] {
            let mut gone = person(Level::Verified, None, &event);
            gone.stays = vec![closed_stay(
                &a,
                &event,
                hours,
                ExitReason::CheckinElsewhere,
                None,
            )];
            ids.push(put_person(&app, &gone).await);
        }

        // Households and animals.
        for (shelter, pets) in [
            (&a, vec![("dog", 2), ("cat", 1)]),
            (&a, vec![("dog", 1)]),
            (&b, vec![]),
        ] {
            let now = store::now();
            let household = households::HouseholdDoc {
                id: None,
                member_ids: vec![],
                head_id: None,
                pets: pets
                    .into_iter()
                    .map(|(species, count)| Pet {
                        species: species.into(),
                        count,
                    })
                    .collect(),
                card_code: None,
                created_by: "test".into(),
                created_shelter_id: shelter.clone(),
                client_created_at: now,
                created_at: now,
                updated_at: now,
            };
            store::replace(&app.state.db, households::COLLECTION, &new_id(), &household)
                .await
                .unwrap();
        }

        // Rescues: two of E, one of O.
        for ev in [&event, &event, &other_event] {
            let now = store::now();
            let rescue = RescueDoc {
                person_id: "p".into(),
                volunteer_id: "v".into(),
                device_id: None,
                lat: 0.0,
                lon: 0.0,
                accuracy_m: 1.0,
                landing_point: None,
                captured_at: now,
                received_at: now,
                event_id: Some(ev.clone()),
                destination_shelter_id: None,
                note: None,
            };
            store::replace(&app.state.db, rescues::COLLECTION, &new_id(), &rescue)
                .await
                .unwrap();
        }

        // Alerts: two open (one informational) and one resolved of E, one open of O.
        for (ev, informational, resolved) in [
            (&event, false, false),
            (&event, true, false),
            (&event, false, true),
            (&other_event, false, false),
        ] {
            let mut alert = AlertDoc::new(AlertKind::ShelterFlagged, "shelter", &a, json!({}));
            alert.event_id = Some(ev.clone());
            alert.informational = informational;
            alert.resolved = resolved;
            store::replace(&app.state.db, crate::alerts::COLLECTION, &new_id(), &alert)
                .await
                .unwrap();
        }

        // Matches: 2 central open, 1 protection open, 1 central accepted, 1 not_the_person.
        for (kind, status) in [
            (MediatorKind::Central, MatchStatus::Open),
            (MediatorKind::Central, MatchStatus::Open),
            (MediatorKind::Protection, MatchStatus::Open),
            (MediatorKind::Central, MatchStatus::Accepted),
            (MediatorKind::Shelter, MatchStatus::NotThePerson),
        ] {
            let doc = MatchDoc {
                id: None,
                search_id: "s".into(),
                person_id: "p".into(),
                is_child: false,
                mediator_kind: kind,
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

        // Captures: 3 that count in A, a login that does not, one in the other event's shelter.
        for (shelter, purpose) in [
            (&a, Purpose::Checkin),
            (&a, Purpose::Checkin),
            (&a, Purpose::Biometrics),
            (&a, Purpose::Login),
            (&o, Purpose::Checkin),
        ] {
            let doc = CaptureDoc {
                process_id: "proc".into(),
                purpose,
                device_id: "d".into(),
                status: CaptureStatus::Ok,
                person_id: None,
                volunteer_id: None,
                shelter_id: Some(shelter.clone()),
                expected_person_id: None,
                consumed: false,
                created_at: store::now(),
                expires_at: store::now(),
            };
            store::replace(&app.state.db, CAPTURES, &new_id(), &doc)
                .await
                .unwrap();
        }
        World {
            app,
            civil_defense,
            validator,
            ana,
            event,
            a,
            b,
            other_event,
            people_ids: ids,
        }
    }

    async fn get_json(w: &World, who: &Actor, path: &str) -> (StatusCode, Value) {
        w.app.get_authed(path, &who.secret).await
    }

    fn row<'a>(body: &'a Value, shelter_id: &str) -> &'a Value {
        body["shelters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == shelter_id)
            .unwrap()
    }

    #[tokio::test]
    async fn the_event_totals_count_every_cut_of_the_people_in_shelters() {
        let w = world().await;
        let path = format!("/v1/dashboard/events/{}", w.event);
        let (status, body) = get_json(&w, &w.civil_defense, &path).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["notice"], NOTICE);
        assert_eq!(body["id"], w.event.as_str());
        let t = &body["totals"];
        assert_eq!(t["unique_people"], 11);
        assert_eq!(
            t["by_level"],
            json!({ "verified": 8, "assisted": 2, "no_biometrics": 1 })
        );
        assert_eq!(
            t["age_bands"],
            json!({ "0_11": 2, "12_17": 3, "18_59": 3, "60_plus": 2, "unknown": 1 })
        );
        assert_eq!(t["pregnant"], 1);
        assert_eq!(t["lactating"], 1);
        assert_eq!(
            t["disability"],
            json!({ "mobility": 2, "visual": 1, "hearing": 0, "intellectual": 0, "psychosocial": 0, "other": 0 })
        );
        assert_eq!(t["children_separated"], 1);
        assert_eq!(t["children_unaccompanied"], 1);
        assert_eq!(t["indigenous"], 1);
        assert_eq!(t["migrant"], 1);
        assert_eq!(t["households"], 3);
        assert_eq!(t["pets"], 4);
    }

    #[tokio::test]
    async fn each_shelter_row_has_its_own_counts_capacity_and_vacancies() {
        let w = world().await;
        let path = format!("/v1/dashboard/events/{}", w.event);
        let (_, body) = get_json(&w, &w.validator, &path).await;
        assert_eq!(body["shelters"].as_array().unwrap().len(), 2);
        let a = row(&body, &w.a);
        assert_eq!(a["name"], "Abrigo A");
        assert_eq!(a["confirmation"], "unconfirmed");
        assert_eq!(a["flagged"], true);
        assert_eq!(a["lat"], ORIGIN.0);
        assert_eq!(a["lon"], ORIGIN.1);
        assert_eq!(a["capacity"], 10);
        assert_eq!(a["occupancy"], 5);
        assert_eq!(a["vacancies"], 5);
        let c = &a["counts"];
        assert_eq!(c["unique_people"], 5);
        assert_eq!(
            c["by_level"],
            json!({ "verified": 2, "assisted": 2, "no_biometrics": 1 })
        );
        // The person without `birth_year` is only in `unknown`.
        assert_eq!(
            c["age_bands"],
            json!({ "0_11": 1, "12_17": 1, "18_59": 1, "60_plus": 1, "unknown": 1 })
        );
        assert_eq!(c["pregnant"], 1);
        assert_eq!(c["lactating"], 1);
        assert_eq!(c["disability"]["mobility"], 2);
        assert_eq!(c["disability"]["visual"], 1);
        assert_eq!(c["children_separated"], 1);
        assert_eq!(c["children_unaccompanied"], 1);
        assert_eq!(c["indigenous"], 1);
        assert_eq!(c["migrant"], 1);
        assert_eq!(c["households"], 2);
        assert_eq!(c["pets"], 4);

        let b = row(&body, &w.b);
        assert_eq!(b["confirmation"], "confirmed");
        assert_eq!(b["flagged"], false);
        assert_eq!(b["capacity"], Value::Null);
        assert_eq!(b["occupancy"], 6);
        assert_eq!(b["vacancies"], Value::Null);
        let c = &b["counts"];
        assert_eq!(c["unique_people"], 6);
        assert_eq!(
            c["by_level"],
            json!({ "verified": 6, "assisted": 0, "no_biometrics": 0 })
        );
        // Ages 18 and 59 are 18_59, 17 and 12 are 12_17, 11 is 0_11, 60 is 60_plus.
        assert_eq!(
            c["age_bands"],
            json!({ "0_11": 1, "12_17": 2, "18_59": 2, "60_plus": 1, "unknown": 0 })
        );
        assert_eq!(c["households"], 1);
        assert_eq!(c["pets"], 0);
        assert_eq!(c["pregnant"], 0);
        assert_eq!(c["indigenous"], 0);
    }

    #[tokio::test]
    async fn movements_exits_rescues_alerts_matches_and_pilot_indicators() {
        let w = world().await;
        let path = format!("/v1/dashboard/events/{}", w.event);
        let (_, body) = get_json(&w, &w.civil_defense, &path).await;
        // A check-in elsewhere 2 h ago is in both windows, 72 h ago only in the 7 days, 240 h in none.
        assert_eq!(body["movement"], json!({ "last_24h": 1, "last_7d": 2 }));
        // The stay of the other event on one of E's people is not an exit of E.
        assert_eq!(
            body["exits_by_destination"],
            json!({ "own_home": 1, "relatives_or_friends": 1 })
        );
        assert_eq!(body["rescues"], 2);
        assert_eq!(body["open_alerts"], 2);
        assert_eq!(
            body["open_matches"],
            json!({ "central": 2, "protection": 1 })
        );
        let pilot = &body["pilot"];
        assert_eq!(pilot["not_the_person"], 1);
        assert_eq!(pilot["other_site_reasons"], 1);
        // One refusal in 11 people; 3 Unico processes in 11 people.
        assert!((pilot["selfie_refusal_rate"].as_f64().unwrap() - 1.0 / 11.0).abs() < 1e-9);
        assert!((pilot["unico_processes_per_person"].as_f64().unwrap() - 3.0 / 11.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn an_event_without_shelters_is_all_zero_and_the_rates_are_absent() {
        let w = world().await;
        let empty = new_id();
        let doc = events::EventDoc {
            name: "Vazio".into(),
            auto_named: false,
            status: EventStatus::Active,
            created_at: store::now(),
            last_activity_at: store::now(),
            closed_at: None,
            closure_reason: None,
            purge_at: None,
            delivery_purge_at: None,
            purged_at: None,
            stats: None,
            reference_contacts: Default::default(),
        };
        store::replace(&w.app.state.db, events::COLLECTION, &empty, &doc)
            .await
            .unwrap();
        let (status, body) = get_json(
            &w,
            &w.civil_defense,
            &format!("/v1/dashboard/events/{empty}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["totals"]["unique_people"], 0);
        assert_eq!(body["shelters"], json!([]));
        assert_eq!(body["movement"], json!({ "last_24h": 0, "last_7d": 0 }));
        assert_eq!(body["exits_by_destination"], json!({}));
        assert_eq!(body["pilot"]["selfie_refusal_rate"], Value::Null);
        assert_eq!(body["pilot"]["unico_processes_per_person"], Value::Null);
    }

    #[tokio::test]
    async fn the_list_has_the_summary_and_the_shelters_to_confirm() {
        let w = world().await;
        let (status, body) = get_json(&w, &w.civil_defense, "/v1/dashboard/events").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let events = body["events"].as_array().unwrap();
        let e = events.iter().find(|e| e["id"] == w.event.as_str()).unwrap();
        assert_eq!(e["status"], "active");
        assert_eq!(e["shelters"], 2);
        assert_eq!(e["occupancy"], 11);
        assert_eq!(e["unique_people"], 11);
        let waiting = e["unconfirmed_shelters"].as_array().unwrap();
        assert_eq!(waiting.len(), 1, "{e}");
        let s = &waiting[0];
        assert_eq!(s["id"], w.a.as_str());
        assert_eq!(s["name"], "Abrigo A");
        assert_eq!(s["flagged"], true);
        assert_eq!(s["lat"], ORIGIN.0);
        assert_eq!(s["lon"], ORIGIN.1);
        assert_eq!(s["created_by_name"], "Ana");
        let o = events
            .iter()
            .find(|e| e["id"] == w.other_event.as_str())
            .unwrap();
        assert_eq!(o["occupancy"], 1);
        assert_eq!(o["unconfirmed_shelters"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_closed_shelter_is_not_waiting_for_confirmation() {
        let w = world().await;
        update_shelter(&w.app, &w.a, |s| s.status = ShelterStatus::Closed).await;
        let (_, body) = get_json(&w, &w.validator, "/v1/dashboard/events").await;
        let e = body["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == w.event.as_str())
            .unwrap();
        assert_eq!(e["unconfirmed_shelters"], json!([]));
        let path = format!("/v1/dashboard/events/{}", w.event);
        let (_, detail) = get_json(&w, &w.validator, &path).await;
        assert_eq!(detail["unconfirmed_shelters"], json!([]));
    }

    #[tokio::test]
    async fn the_detail_lists_the_unconfirmed_shelters_too() {
        let w = world().await;
        let path = format!("/v1/dashboard/events/{}", w.event);
        let (_, body) = get_json(&w, &w.validator, &path).await;
        let waiting = body["unconfirmed_shelters"].as_array().unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0]["id"], w.a.as_str());
        assert_eq!(waiting[0]["created_by_name"], "Ana");
    }

    #[tokio::test]
    async fn only_validator_and_civil_defense_read_the_panel() {
        let w = world().await;
        let paths = [
            "/v1/dashboard/events".to_string(),
            format!("/v1/dashboard/events/{}", w.event),
            format!("/v1/dashboard/events/{}/daily.csv", w.event),
        ];
        let mediator = w.app.actor("Mediadora").await;
        w.app.grant(&mediator, GlobalRole::Mediator).await;
        let protection = w.app.actor("Protecao").await;
        w.app.grant(&protection, GlobalRole::Protection).await;
        let stranger = w.app.actor("Estranho").await;
        for path in &paths {
            let (status, _, body) = w.app.request("GET", path, &[], None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
            assert_eq!(body["error"], "unauthorized");
            // `ana` is the admin of a shelter of the event.
            for who in [&w.ana, &mediator, &protection, &stranger] {
                let (status, body) = get_json(&w, who, path).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
                assert_eq!(body["error"], "forbidden");
            }
            for who in [&w.validator, &w.civil_defense] {
                let auth = format!("Bearer {}", who.secret);
                let (status, _, _) = w
                    .app
                    .request("GET", path, &[("authorization", auth.as_str())], None)
                    .await;
                assert_eq!(status, StatusCode::OK, "{path}");
            }
        }
    }

    #[tokio::test]
    async fn an_unknown_event_is_404_after_the_permission_check() {
        let w = world().await;
        for suffix in ["", "/daily.csv"] {
            let path = format!("/v1/dashboard/events/nope{suffix}");
            let (status, body) = get_json(&w, &w.civil_defense, &path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
            assert_eq!(body["error"], "event_not_found");
            let (status, _) = get_json(&w, &w.ana, &path).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }
    }

    async fn csv(w: &World, who: &Actor) -> (StatusCode, axum::http::HeaderMap, String) {
        let request = Request::builder()
            .method("GET")
            .uri(format!("/v1/dashboard/events/{}/daily.csv", w.event))
            .header("authorization", format!("Bearer {}", who.secret))
            .body(Body::empty())
            .unwrap();
        let response = crate::router(w.app.state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let (status, headers) = (response.status(), response.headers().clone());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn the_daily_csv_has_one_line_per_shelter_and_no_person() {
        let w = world().await;
        let (status, headers, body) = csv(&w, &w.civil_defense).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_TYPE], "text/csv; charset=utf-8");
        assert!(
            headers[CONTENT_DISPOSITION]
                .to_str()
                .unwrap()
                .starts_with("attachment; filename=\"taba-")
        );
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 3, "header and two shelters: {body}");
        assert_eq!(
            lines[0],
            "date,event_id,shelter_id,shelter_name,confirmation,flagged,lat,lon,capacity,occupancy,vacancies,\
unique_people,verified,assisted,no_biometrics,age_0_11,age_12_17,age_18_59,age_60_plus,age_unknown,\
pregnant,lactating,disability_mobility,disability_visual,disability_hearing,disability_intellectual,\
disability_psychosocial,disability_other,children_separated,children_unaccompanied,indigenous,migrant,\
households,pets"
        );
        let width = lines[0].split(',').count();
        let a: Vec<&str> = lines
            .iter()
            .find(|l| l.contains(&w.a))
            .unwrap()
            .split(',')
            .collect();
        assert_eq!(a.len(), width);
        let date = store::now().to_zoned(TimeZone::UTC).date().to_string();
        assert_eq!(a[0], date);
        assert_eq!(a[1], w.event);
        assert_eq!(a[3], "Abrigo A");
        assert_eq!(a[4], "unconfirmed");
        assert_eq!(a[5], "true");
        assert_eq!(&a[8..12], ["10", "5", "5", "5"]);
        assert_eq!(&a[12..15], ["2", "2", "1"]);
        assert_eq!(&a[15..20], ["1", "1", "1", "1", "1"]);
        assert_eq!(&a[20..22], ["1", "1"]);
        assert_eq!(&a[22..28], ["2", "1", "0", "0", "0", "0"]);
        assert_eq!(&a[28..34], ["1", "1", "1", "1", "2", "4"]);
        let b: Vec<&str> = lines
            .iter()
            .find(|l| l.contains(&w.b))
            .unwrap()
            .split(',')
            .collect();
        // No capacity: both `capacity` and `vacancies` are empty.
        assert_eq!((b[8], b[9], b[10]), ("", "6", ""));
        assert!(!body.contains("Maria"));
        assert!(!body.contains("person"));
        for id in &w.people_ids {
            assert!(!body.contains(id.as_str()));
        }
    }

    #[tokio::test]
    async fn the_json_answers_never_carry_a_name_or_a_person_id() {
        let w = world().await;
        for path in [
            "/v1/dashboard/events".to_string(),
            format!("/v1/dashboard/events/{}", w.event),
        ] {
            let (_, body) = get_json(&w, &w.civil_defense, &path).await;
            let text = body.to_string();
            assert!(!text.contains("Maria"), "{path}");
            assert!(!text.contains("person_id"), "{path}");
            for id in &w.people_ids {
                assert!(!text.contains(id.as_str()), "{path}");
            }
        }
    }

    #[tokio::test]
    async fn a_shelter_name_that_looks_like_a_formula_is_neutralized_in_the_csv() {
        let w = world().await;
        update_shelter(&w.app, &w.b, |s| {
            s.name = "=HYPERLINK(\"http://x\",\"a,b\")".into();
        })
        .await;
        let (_, _, body) = csv(&w, &w.civil_defense).await;
        assert!(
            body.contains("\"'=HYPERLINK(\"\"http://x\"\",\"\"a,b\"\")\""),
            "{body}"
        );
    }

    #[test]
    fn csv_fields_are_guarded_against_formulas_and_quoted_when_needed() {
        assert_eq!(csv_field("Abrigo"), "Abrigo");
        for lead in ["=1+1", "+1", "-1", "@SUM(A1)", "\tx"] {
            assert_eq!(csv_field(lead), format!("'{lead}"), "{lead:?}");
        }
        assert_eq!(csv_field("\rx"), "\"'\rx\"");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("a\nb"), "\"a\nb\"");
        assert_eq!(csv_field("São José"), "São José");
    }

    #[tokio::test]
    async fn the_panel_is_served_from_the_cache_until_it_expires() {
        let w = world().await;
        let path = format!("/v1/dashboard/events/{}", w.event);
        let (_, first) = get_json(&w, &w.civil_defense, &path).await;
        assert_eq!(first["totals"]["unique_people"], 11);
        put_person(&w.app, &person(Level::Assisted, Some(&w.b), &w.event)).await;
        let (_, second) = get_json(&w, &w.civil_defense, &path).await;
        assert_eq!(second, first, "served from the cache");
        // The cache does not skip the permission check.
        let (status, _) = get_json(&w, &w.ana, &path).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        DETAIL.clear();
        let (_, third) = get_json(&w, &w.civil_defense, &path).await;
        assert_eq!(third["totals"]["unique_people"], 12);
    }

    #[tokio::test]
    async fn the_cache_builds_once_inside_the_ttl_and_again_after_it() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let cache: TtlCache<u32> = TtlCache::new();
        let built = AtomicU32::new(0);
        let build = || async { Ok(built.fetch_add(1, Ordering::SeqCst) + 1) };
        let ttl = Duration::from_secs(60);
        assert_eq!(cache.get_or("k", ttl, build).await.unwrap(), 1);
        assert_eq!(cache.get_or("k", ttl, build).await.unwrap(), 1);
        // Another key is its own entry.
        assert_eq!(cache.get_or("other", ttl, build).await.unwrap(), 2);
        // A ttl of zero has always expired.
        assert_eq!(cache.get_or("k", Duration::ZERO, build).await.unwrap(), 3);
        // A failed build is not cached.
        let failed = cache
            .get_or("bad", ttl, || async { Err(ApiError::internal()) })
            .await;
        assert!(failed.is_err());
        assert_eq!(cache.get_or("bad", ttl, build).await.unwrap(), 4);
    }
}
