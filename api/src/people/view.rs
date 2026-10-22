//! The JSON a person and their history are shown as (spec 4.11).

use serde_json::{Value, json};

use super::model::{PersonDoc, Stay};
use crate::auth::Volunteer;
use crate::error::ApiError;
use crate::members::ShelterRole;
use crate::shelters::Confirmation;

// ---- history visibility (spec 4.11) ----

/// What a volunteer of `shelter_id` sees of the stays: the shelter's own always; the stays in
/// other shelters only for the admin of a confirmed shelter, who gets shelter and dates;
/// everyone else in a confirmed shelter only learns whether there are any; an unconfirmed
/// shelter sees nothing of the others.
pub fn visible_history(stays: &[Stay], shelter_id: &str, admin: bool, confirmed: bool) -> Value {
    let (own, other): (Vec<&Stay>, Vec<&Stay>) =
        stays.iter().partition(|s| s.shelter_id == shelter_id);
    let mut shown: Vec<Value> = own.iter().map(|s| stay_view(s, true)).collect();
    if confirmed && admin {
        shown.extend(other.iter().map(|s| stay_view(s, false)));
    }
    json!({ "stays": shown, "has_other_stays": confirmed && !other.is_empty() })
}

fn stay_view(stay: &Stay, own: bool) -> Value {
    let mut view = json!({
        "shelter_id": stay.shelter_id,
        "entered_at": stay.entered_at.to_string(),
        "left_at": stay.left_at.map(|t| t.to_string()),
        "exit_reason": stay.exit_reason,
    });
    if own {
        view["exit_destination"] = json!(stay.exit_destination);
        view["destination_municipality"] = json!(stay.destination_municipality);
    }
    view
}

pub(super) fn history_for(person: &PersonDoc, viewer: &Volunteer, shelter_id: &str) -> Value {
    match viewer
        .memberships
        .iter()
        .find(|m| m.shelter_id == shelter_id)
    {
        Some(m) => visible_history(
            &person.stays,
            shelter_id,
            m.role == ShelterRole::Admin,
            m.confirmation == Confirmation::Confirmed,
        ),
        None => json!({ "stays": [], "has_other_stays": false }),
    }
}

/// The shelter through which `viewer` is allowed to see the person: the person's current one,
/// else the latest shelter of a stay the viewer belongs to (a registration of their own).
pub(super) fn viewer_shelter(person: &PersonDoc, viewer: &Volunteer) -> Option<String> {
    let current = person
        .current_shelter_id
        .iter()
        .find(|s| viewer.role_in(s).is_some());
    let past = person
        .stays
        .iter()
        .rev()
        .map(|s| &s.shelter_id)
        .find(|s| viewer.role_in(s).is_some());
    current.or(past).cloned()
}

pub fn person_view(id: &str, person: &PersonDoc, history: Value) -> Value {
    json!({
        "id": id,
        "level": person.level,
        "biometrics_declined": person.biometrics_declined,
        "former_ids": person.former_ids,
        "name": person.name,
        "social_name": person.social_name,
        "wristband": person.wristband,
        "birth_year": person.birth_year,
        "is_child": person.is_child,
        "child_status": person.child_status,
        "guardians": person.guardians.iter().map(|g| json!({
            "person_id": g.person_id,
            "link": g.link,
            "checked_by": g.checked_by,
            "checked_at": g.checked_at.map(|t| t.to_string()),
        })).collect::<Vec<_>>(),
        "needs": person.needs,
        "vulnerabilities": person.vulnerabilities,
        "household_id": person.household_id,
        "current_shelter_id": person.current_shelter_id,
        "history": history,
        "has_thumbnail": person.thumbnail_encrypted.is_some(),
        "origin": person.origin,
        "created_at": person.created_at.to_string(),
        "updated_at": person.updated_at.to_string(),
        "client_edited_at": person.client_edited_at.map(|t| t.to_string()),
    })
}

/// One page of a shelter's people; `has_more` tells the app the list stops at the page limit.
pub(super) fn list_view(
    people: &[PersonDoc],
    volunteer: &Volunteer,
    shelter_id: &str,
    has_more: bool,
) -> Result<Value, ApiError> {
    let list = people
        .iter()
        .map(|p| {
            Ok(person_view(
                p.stored_id()?,
                p,
                history_for(p, volunteer, shelter_id),
            ))
        })
        .collect::<Result<Vec<Value>, ApiError>>()?;
    Ok(json!({ "people": list, "has_more": has_more }))
}
