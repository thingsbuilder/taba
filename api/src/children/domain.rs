//! The rules on `is_child` and `child_status` (spec 3.4.1, 4.9): plain functions over
//! `PersonDoc`. Nothing here is `async` or reads the database.

use axum::http::StatusCode;
use serde_json::json;

use super::model::{ChildChange, GuardianRequest};
use crate::auth::Volunteer;
use crate::error::ApiError;
use crate::people::{ChildStatus, Guardian, GuardianLink, PersonDoc};
use crate::store;
use crate::volunteers::GlobalRole;

/// `child_status` changes after the check-in belong to `protection` and the Civil Defense
/// (spec 3.4.1, item 4); the same two also exit separated children (spec 2.3).
pub fn can_change_status(volunteer: &Volunteer) -> bool {
    volunteer.has_global(GlobalRole::Protection) || volunteer.has_global(GlobalRole::CivilDefense)
}

/// The single rule for `is_child` and `child_status` of a person who already exists; every
/// route that writes them goes through here. Anyone may raise the protection (declare a child,
/// or send the status a child already has); only `protection` and the Civil Defense
/// (`may_alter`) lower it, change the status or clear the condition. The two fields move
/// together: a status implies a child, and clearing the condition clears the status.
/// Returns the status the `child_separated` alert is owed for: `separated` or `unaccompanied`,
/// and only when it is new, so a repeated write raises nothing.
pub fn change_child_state(
    person: &mut PersonDoc,
    change: ChildChange,
    may_alter: bool,
) -> Result<Option<ChildStatus>, ApiError> {
    if change.is_child == Some(false) && change.child_status.is_some() {
        let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_person");
        error.detail = json!({ "field": "child_status" });
        return Err(error);
    }
    let clears = change.is_child == Some(false) && person.is_child;
    let alters = change
        .child_status
        .is_some_and(|status| Some(status) != person.child_status);
    if (clears || alters) && !may_alter {
        return Err(ApiError::protection_role_required());
    }
    if clears {
        person.is_child = false;
        person.child_status = None;
        return Ok(None);
    }
    let became_child = !person.is_child && (change.is_child == Some(true) || alters);
    if became_child {
        person.is_child = true;
    }
    if alters {
        person.child_status = change.child_status;
    }
    Ok(person
        .child_status
        .filter(|status| (alters || became_child) && *status != ChildStatus::WithGuardian))
}

fn protection_rank(status: Option<ChildStatus>) -> u8 {
    match status {
        None => 0,
        Some(ChildStatus::WithGuardian) => 1,
        Some(ChildStatus::Separated) => 2,
        Some(ChildStatus::Unaccompanied) => 3,
    }
}

/// Late biometrics folds the record `old` into `new`: the merged person is a child if either
/// was, with the more protective of the two statuses, so a fusion never lowers the protection.
/// Returns the status the alert is owed for when the fusion made `new` more protected.
pub fn merge_child_state(new: &mut PersonDoc, old: &PersonDoc) -> Option<ChildStatus> {
    let before = (new.is_child, new.child_status);
    new.is_child |= old.is_child;
    if protection_rank(old.child_status) > protection_rank(new.child_status) {
        new.child_status = old.child_status;
    }
    // a status belongs to a child
    if new.child_status.is_some() {
        new.is_child = true;
    }
    (before != (new.is_child, new.child_status))
        .then_some(new.child_status)
        .flatten()
        .filter(|status| *status != ChildStatus::WithGuardian)
}

/// The entry for `request` as `volunteer` records it: only `document_checked` says who looked
/// at the document and when.
pub(super) fn guardian_entry(request: &GuardianRequest, volunteer: &Volunteer) -> Guardian {
    let checked = request.link == GuardianLink::DocumentChecked;
    Guardian {
        person_id: request.guardian_id.clone(),
        link: request.link,
        checked_by: checked.then(|| volunteer.id.clone()),
        checked_at: checked.then(store::now),
    }
}
