//! The rules of an exit (spec 2.3, 3.8) and the vacancy arithmetic (spec 4.2): plain
//! functions; nothing here is `async` or reads the database.

use super::model::{ExitRequest, Plan, forbidden, invalid};
use crate::error::ApiError;
use crate::people::{ChildStatus, GuardianLink, PersonDoc};

pub(super) const MAX_MUNICIPALITY_CHARS: usize = 100;
pub(super) const MAX_REASON_CHARS: usize = 500;

/// `capacity - occupancy`, never below zero; `None` while the shelter has no capacity set.
pub fn vacancies(capacity: Option<u32>, occupancy: u32) -> Option<u32> {
    capacity.map(|capacity| capacity.saturating_sub(occupancy))
}

/// Who may do what (spec 2.3, 3.8). `member`: active member of the shelter; `privileged`:
/// `protection` or Civil Defense. Decides before anything is asked of Unico.
pub(super) fn plan(
    member: bool,
    privileged: bool,
    request: &ExitRequest,
    person: &PersonDoc,
) -> Result<Plan, ApiError> {
    if let Some(exception) = &request.exception {
        if !person.is_child {
            return Err(invalid("exception_for_child_only"));
        }
        if !privileged {
            return Err(ApiError::protection_role_required());
        }
        let reason = exception.reason.trim();
        if reason.is_empty() || reason.chars().count() > MAX_REASON_CHARS {
            return Err(invalid("exception_reason_required"));
        }
        return Ok(Plan::Exception {
            reason: reason.to_string(),
        });
    }
    if !person.is_child {
        return if member {
            Ok(Plan::Adult)
        } else {
            Err(forbidden())
        };
    }
    let guardian = request
        .guardian_id
        .as_deref()
        .and_then(|id| person.guardians.iter().find(|g| g.person_id == id));
    let volunteer_may = member
        && person.child_status == Some(ChildStatus::WithGuardian)
        && guardian.is_some_and(|g| g.link == GuardianLink::DocumentChecked);
    if !privileged && !volunteer_may {
        return Err(ApiError::protection_role_required());
    }
    if request.guardian_id.is_none() {
        return Err(invalid("guardian_required"));
    }
    let guardian = guardian.ok_or_else(|| invalid("guardian_not_linked"))?;
    let capture_id = request
        .capture_id
        .clone()
        .ok_or_else(|| invalid("capture_required"))?;
    Ok(Plan::Revalidate {
        guardian_id: guardian.person_id.clone(),
        capture_id,
    })
}

pub(super) fn open_stay(person: &PersonDoc, shelter_id: &str) -> Option<usize> {
    person
        .stays
        .iter()
        .position(|s| s.shelter_id == shelter_id && s.left_at.is_none())
}

pub(super) fn municipality(value: &Option<String>) -> Result<Option<String>, ApiError> {
    let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > MAX_MUNICIPALITY_CHARS {
        return Err(invalid("invalid_destination_municipality"));
    }
    Ok(Some(value.to_string()))
}
