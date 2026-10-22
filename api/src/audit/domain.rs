//! The anomaly rules of spec 4.10 as plain state and functions: what the process remembers
//! between requests and when it decides to raise an alert. Nothing here is `async` or reads
//! the database.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};

use super::model::Activity;
use crate::geo;

/// Counting window of `anomaly_volume`, and how long the shelter points of a volunteer are kept.
pub(super) const WINDOW_SECS: i64 = 600;
pub(super) const DISTANCE_LIMIT_KM: f64 = 50.0;
pub(super) const DISTANCE_WINDOW_SECS: i64 = 3600;
/// Entries each in-memory table keeps before it drops the stale ones.
pub(super) const MAX_TRACKED: usize = 10_000;

/// What the anomaly rules remember between requests.
// ponytail: per process, at most `MAX_TRACKED` entries per table (past it the stale ones are
// dropped). Lost on restart, and not shared between instances: a restart or a second instance
// forgets the open window, so one alert may repeat or be missed. The pilot runs one instance;
// move to a Firestore document with a TTL before running two.
#[derive(Default)]
pub(super) struct Tracker {
    /// `(volunteer, activity)` -> start of the window, count in it, alert already raised.
    pub(super) windows: HashMap<(String, Activity), (i64, u32, bool)>,
    /// `person_id` -> where and when the person was last registered.
    pub(super) last_seen: HashMap<String, (f64, f64, i64)>,
    /// Volunteer -> when their shelter points were read, and the points.
    pub(super) areas: HashMap<String, (i64, Vec<(f64, f64)>)>,
    /// Volunteer -> when `out_of_area` was last raised.
    pub(super) out_of_area: HashMap<String, i64>,
}

static TRACKER: LazyLock<Mutex<Tracker>> = LazyLock::new(Mutex::default);

pub(super) fn tracker() -> MutexGuard<'static, Tracker> {
    TRACKER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Tracker {
    /// Counts one activity; the count that first passes `limit` in a window is returned, once.
    pub(super) fn count(
        &mut self,
        volunteer: &str,
        activity: Activity,
        now: i64,
        limit: u32,
    ) -> Option<u32> {
        if self.windows.len() > MAX_TRACKED {
            self.windows
                .retain(|_, (start, _, _)| now - *start < WINDOW_SECS);
        }
        let entry = self
            .windows
            .entry((volunteer.to_string(), activity))
            .or_insert((now, 0, false));
        if now - entry.0 >= WINDOW_SECS {
            *entry = (now, 0, false);
        }
        entry.1 += 1;
        if entry.1 > limit && !entry.2 {
            entry.2 = true;
            return Some(entry.1);
        }
        None
    }

    /// Remembers where `person` was registered; returns the distance in km and the minutes since
    /// when the last point was farther than the limit and less than an hour old.
    pub(super) fn moved(&mut self, person: &str, at: (f64, f64), now: i64) -> Option<(f64, i64)> {
        if self.last_seen.len() > MAX_TRACKED {
            self.last_seen
                .retain(|_, (_, _, seen)| now - *seen < DISTANCE_WINDOW_SECS);
        }
        let previous = self
            .last_seen
            .insert(person.to_string(), (at.0, at.1, now))?;
        let km = geo::haversine_m(previous.0, previous.1, at.0, at.1) / 1000.0;
        let seconds = now - previous.2;
        (seconds < DISTANCE_WINDOW_SECS && km > DISTANCE_LIMIT_KM).then_some((km, seconds / 60))
    }

    /// True the first time in a window that `volunteer` is out of the area.
    pub(super) fn first_out_of_area(&mut self, volunteer: &str, now: i64) -> bool {
        match self.out_of_area.get(volunteer) {
            Some(last) if now - last < WINDOW_SECS => false,
            _ => {
                self.out_of_area.insert(volunteer.to_string(), now);
                true
            }
        }
    }
}

/// The distance in km to the nearest of `points` when all of them are farther than `radius_km`.
pub(super) fn beyond_all(points: &[(f64, f64)], at: (f64, f64), radius_km: f64) -> Option<f64> {
    let nearest = points
        .iter()
        .map(|p| geo::haversine_m(p.0, p.1, at.0, at.1) / 1000.0)
        .min_by(f64::total_cmp)?;
    (nearest > radius_km).then_some(nearest)
}
