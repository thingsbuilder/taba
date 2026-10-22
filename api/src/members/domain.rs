//! The rules on roles and memberships (spec 2.1, 2.3, 3.2.7): plain functions and methods over
//! the documents. Nothing here is `async` or reads the database.

use firestore::FirestoreInstant;

use super::model::{Change, MembershipDoc, MembershipStatus, ShelterRole, Via};
use crate::volunteers::{GlobalRole, RoleGrant, VolunteerDoc};

/// Who the operator's grants are recorded as in `role_grants.by`.
pub(super) const OPERATOR: &str = "cli";

impl GlobalRole {
    pub fn name(self) -> &'static str {
        match self {
            Self::Validator => "validator",
            Self::CivilDefense => "civil_defense",
            Self::Mediator => "mediator",
            Self::Protection => "protection",
        }
    }
}

/// The roles the operator may change from the command line.
pub fn parse_cli_role(name: &str) -> Option<GlobalRole> {
    match name {
        "validator" => Some(GlobalRole::Validator),
        "civil_defense" => Some(GlobalRole::CivilDefense),
        _ => None,
    }
}

/// Applies the change to the profile; returns whether anything changed.
pub fn apply(
    doc: &mut VolunteerDoc,
    role: GlobalRole,
    change: Change,
    now: FirestoreInstant,
) -> bool {
    let has = doc.global_roles.contains(&role);
    match (change, has) {
        (Change::Grant, false) => {
            doc.global_roles.push(role);
            doc.role_grants.push(RoleGrant {
                role,
                by: OPERATOR.into(),
                at: now,
            });
            true
        }
        (Change::Revoke, true) => {
            doc.global_roles.retain(|r| *r != role);
            doc.role_grants.retain(|g| g.role != role);
            true
        }
        _ => false,
    }
}

impl MembershipDoc {
    /// The creator of a shelter is its first admin (spec 3.2.7).
    pub fn creator(shelter_id: &str, volunteer_id: &str, now: FirestoreInstant) -> Self {
        Self {
            shelter_id: shelter_id.into(),
            volunteer_id: volunteer_id.into(),
            role: ShelterRole::Admin,
            status: MembershipStatus::Active,
            via: Via::Creator,
            invite_code: None,
            invited_by: None,
            joined_at: now,
            removed_by: None,
            removed_at: None,
        }
    }

    pub(super) fn remove(&mut self, by: &str, now: FirestoreInstant) {
        self.status = MembershipStatus::Removed;
        self.removed_by = Some(by.into());
        self.removed_at = Some(now);
    }
}
