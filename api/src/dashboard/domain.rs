//! The cuts of the panel and the small pure helpers behind the counts. Nothing here
//! is `async` or reads the database.

use serde::Serialize;

use crate::error::ApiError;
use crate::people::{ChildStatus, Disability, Level};

pub(super) const AGE_KEYS: [&str; 4] = ["0_11", "12_17", "18_59", "60_plus"];
pub(super) const DISABILITIES: [Disability; 6] = [
    Disability::Mobility,
    Disability::Visual,
    Disability::Hearing,
    Disability::Intellectual,
    Disability::Psychosocial,
    Disability::Other,
];

// ---- counting ----

/// One cut of the people in a shelter.
#[derive(Clone, Copy)]
pub(super) enum Cut {
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

/// The rows to use out of a read that asked for `limit + 1`, and whether it was cut.
pub(super) fn cap<T>(mut rows: Vec<T>, limit: u32) -> (Vec<T>, bool) {
    let cut = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    (rows, cut)
}

/// The `count()` aggregation returns an `i64`; one that does not fit a `u32` is an error that
/// shows in the logs, not a zero on the panel.
pub(crate) fn count_value(n: i64) -> Result<u32, ApiError> {
    u32::try_from(n).map_err(|_| {
        tracing::error!(n, "count() result does not fit in a u32");
        ApiError::internal()
    })
}

pub(super) fn variant<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The cuts that run once a shelter is known to have someone in it, by the key the row reads them by.
pub(super) fn cuts(year: i64) -> Vec<(String, Cut)> {
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
