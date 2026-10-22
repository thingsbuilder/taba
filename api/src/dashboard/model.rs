//! The answers of the panel and the rows they are built from.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::events::EventStatus;

/// The notice the panel shows at the top (spec 3.10).
pub const NOTICE: &str = "Conta desabrigados (pessoas em abrigo). Desalojados (em casa de parentes ou amigos) não estão aqui.";

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
    pub(super) fn add(&mut self, other: &Counts) {
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
    /// When this answer was built (it is served again for up to `CACHE_TTL`).
    pub generated_at: String,
    /// `false` when a bounded read was cut: the indicators in `partial_indicators` are a subset.
    pub complete: bool,
    /// `shelters`, `exits` (movement and exits by destination) or `pets`.
    pub partial_indicators: Vec<&'static str>,
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

#[derive(Deserialize)]
pub(super) struct EventRow {
    #[serde(alias = "_firestore_id", default)]
    pub(super) id: Option<String>,
    pub(super) name: String,
    pub(super) status: EventStatus,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub(super) created_at: firestore::FirestoreInstant,
}
