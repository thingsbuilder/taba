//! The Civil Defense dashboard (spec 3.10, 8.8): `GET /v1/dashboard/events`,
//! `GET /v1/dashboard/events/{id}` and `GET /v1/dashboard/events/{id}/daily.csv`, for a validator
//! (read) and the Civil Defense. Stage 0: the shapes and the routes answer `501 not_implemented`.
#![allow(dead_code)] // stage 0 scaffold: the owner of this module removes this line

use std::collections::BTreeMap;

use axum::Json;
use serde::Serialize;
use serde_json::Value;

use crate::auth::Volunteer;
use crate::error::ApiError;

/// A shelter that waits for a confirmation, for the list of whoever may confirm it.
#[derive(Debug, Clone, Serialize)]
pub struct ShelterToConfirm {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub flagged: bool,
    pub city: Option<String>,
    pub state: Option<String>,
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

/// `GET /v1/dashboard/events`: `{events: [EventSummary]}`.
pub async fn list_events(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}

/// `GET /v1/dashboard/events/{id}`: an `EventDashboard`.
pub async fn get_event(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}

/// `GET /v1/dashboard/events/{id}/daily.csv`: one line per shelter, no name of a person, no `person_id`.
pub async fn daily_csv(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}
