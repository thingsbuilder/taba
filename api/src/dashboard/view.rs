//! The daily CSV (spec 8.8): one line per shelter, no name of a person, no `person_id`.

use super::domain::{AGE_KEYS, DISABILITIES, variant};
use super::model::EventDashboard;
// ---- CSV ----

/// A field of the CSV that a spreadsheet will not run as a formula (a text that starts with `=`,
/// `+`, `-`, `@`, a tab or a carriage return gets a leading `'`) and quotes when it needs to.
pub(super) fn csv_field(value: &str) -> String {
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
pub(super) fn daily_csv_body(dashboard: &EventDashboard, date: &str) -> String {
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
            "households_created_here",
            "pets_in_households_created_here",
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
