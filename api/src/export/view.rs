//! The text of the export: how a cell is written, the names the CSV prints for the enums
//! and the age bands.

use firestore::FirestoreInstant;

use crate::deliveries::{DeliveryStatus, Verification};
use crate::people::{ChildStatus, Level, PersonDoc};

// ---- CSV ----

/// A cell: a leading `= + - @`, tab or CR would run as a formula in a spreadsheet, so it gets a
/// `'` in front; quotes, commas and line breaks are escaped as RFC 4180 says.
pub(super) fn cell(value: &str) -> String {
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

pub(super) fn row(out: &mut String, cells: &[&str]) {
    let line: Vec<String> = cells.iter().map(|c| cell(c)).collect();
    out.push_str(&line.join(","));
    out.push_str("\r\n");
}

pub(super) fn section(out: &mut String, name: &str, columns: &[&str]) {
    out.push_str(&format!("# {name}\r\n"));
    row(out, columns);
}

pub(super) fn warn_if_cut(out: &mut String, name: &str, cut: bool, max_rows: u32) {
    if cut {
        row(
            out,
            &["# warning", "truncated", name, &max_rows.to_string()],
        );
    }
}

pub(super) fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// `0-11`, `12-17`, `18-59`, `60+`; empty when the birth year is unknown.
pub(super) fn age_band(birth_year: Option<u16>, year: i16) -> &'static str {
    let Some(born) = birth_year else { return "" };
    match i32::from(year) - i32::from(born) {
        ..=11 => "0-11",
        12..=17 => "12-17",
        18..=59 => "18-59",
        _ => "60+",
    }
}

pub(super) fn needs_of(person: &PersonDoc) -> String {
    let n = &person.needs;
    [
        (n.continuous_medication, "continuous_medication"),
        (n.dietary_restriction, "dietary_restriction"),
        (n.reduced_mobility, "reduced_mobility"),
        (n.health_follow_up, "health_follow_up"),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .map(|(_, name)| *name)
    .collect::<Vec<_>>()
    .join("|")
}

// The serde names of the enums the CSV prints. The matches have no wildcard, so a new variant
// does not compile until it has a name here; `tags_match_the_serde_names` keeps them equal to serde's.

pub(super) fn level_tag(level: Level) -> &'static str {
    match level {
        Level::Verified => "verified",
        Level::Assisted => "assisted",
        Level::NoBiometrics => "no_biometrics",
    }
}

pub(super) fn child_status_tag(status: ChildStatus) -> &'static str {
    match status {
        ChildStatus::WithGuardian => "with_guardian",
        ChildStatus::Separated => "separated",
        ChildStatus::Unaccompanied => "unaccompanied",
    }
}

pub(super) fn delivery_status_tag(status: DeliveryStatus) -> &'static str {
    match status {
        DeliveryStatus::Ok => "ok",
        DeliveryStatus::InReview => "in_review",
        DeliveryStatus::ReviewedOk => "reviewed_ok",
        DeliveryStatus::Irregular => "irregular",
    }
}

pub(super) fn verification_tag(verification: Verification) -> &'static str {
    match verification {
        Verification::Card => "card",
        Verification::Liveness => "liveness",
        Verification::AdminApproval => "admin_approval",
    }
}

pub(super) fn display_name(person: &PersonDoc) -> String {
    person.name.clone().unwrap_or_default()
}

pub(super) fn at(instant: FirestoreInstant) -> String {
    instant.to_string()
}

/// The stay of `person` in `shelter_id` that is still open: when they entered.
pub(super) fn entered_at(person: &PersonDoc, shelter_id: &str) -> String {
    person
        .stays
        .iter()
        .rev()
        .find(|s| s.shelter_id == shelter_id && s.left_at.is_none())
        .map(|s| at(s.entered_at))
        .unwrap_or_default()
}
