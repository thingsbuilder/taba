//! Who receives an alert and which sources a volunteer reads from (spec 8.8). Everything here is
//! pure, so each rule is tested without Firestore.

use super::model::{AlertDoc, AlertKind};
use crate::auth::Volunteer;
use crate::volunteers::GlobalRole;

/// Who receives an alert (spec 8.8): the Civil Defense gets all of them; admins get the
/// non-informational ones of their shelters; `protection` gets the protection ones.
/// `endorser_flagged` goes to the Civil Defense and the validators, never to the shelter admins,
/// and never to the endorser it is about. `search_expiring` and `family_search_received` go to
/// the volunteers in `volunteer_ids` only.
pub fn is_recipient(volunteer: &Volunteer, alert: &AlertDoc) -> bool {
    if alert.kind.is_personal() {
        return alert.volunteer_ids.contains(&volunteer.id);
    }
    if alert.kind.is_anomaly() {
        return volunteer.is_validator_or_civil_defense()
            && !alert.volunteer_ids.contains(&volunteer.id);
    }
    if alert.kind == AlertKind::EndorserFlagged {
        return volunteer.is_validator_or_civil_defense()
            && !alert.volunteer_ids.contains(&volunteer.id);
    }
    volunteer.has_global(GlobalRole::CivilDefense)
        || (alert.for_protection && volunteer.has_global(GlobalRole::Protection))
        || (!alert.informational
            && alert
                .shelter_ids
                .iter()
                .any(|shelter| volunteer.is_admin_of(shelter)))
}
/// Anyone who can register a search may get an alert about it: shelter members and mediators too.
pub(super) fn can_receive_any(volunteer: &Volunteer) -> bool {
    volunteer.is_validator_or_civil_defense()
        || volunteer.has_global(GlobalRole::Protection)
        || volunteer.has_global(GlobalRole::Mediator)
        || !volunteer.memberships.is_empty()
}
/// One way into the alerts a volunteer may receive. Each is a query by recipient, run before the
/// limit, so alerts of other recipients cannot crowd a recipient's own out of the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Source {
    /// Everything raised for the Civil Defense.
    CivilDefense,
    /// The kinds only validators and the Civil Defense read.
    ValidatorKinds,
    Protection,
    /// The non-informational alerts of a shelter the volunteer administers.
    Shelter(String),
    /// The alerts that name the volunteer in `volunteer_ids`.
    Personal,
}

impl Source {
    /// Whether the source's query returns `alert` for `volunteer_id`; mirrors `open_from`. Kept
    /// for the reachability test, which cannot run the queries over every combination.
    #[cfg(test)]
    pub(super) fn admits(&self, alert: &AlertDoc, volunteer_id: &str) -> bool {
        match self {
            Self::CivilDefense => alert.for_civil_defense,
            Self::ValidatorKinds => super::model::VALIDATOR_KINDS.contains(&alert.kind),
            Self::Protection => alert.for_protection,
            Self::Shelter(id) => !alert.informational && alert.shelter_ids.contains(id),
            Self::Personal => alert.volunteer_ids.iter().any(|v| v == volunteer_id),
        }
    }
}
pub(super) fn sources_of(volunteer: &Volunteer) -> Vec<Source> {
    let mut sources = vec![Source::Personal];
    if volunteer.has_global(GlobalRole::CivilDefense) {
        sources.push(Source::CivilDefense);
    } else if volunteer.has_global(GlobalRole::Validator) {
        sources.push(Source::ValidatorKinds);
    }
    if volunteer.has_global(GlobalRole::Protection) {
        sources.push(Source::Protection);
    }
    sources.extend(
        volunteer
            .memberships
            .iter()
            .filter(|m| volunteer.is_admin_of(&m.shelter_id))
            .map(|m| Source::Shelter(m.shelter_id.clone())),
    );
    sources
}
/// The open alerts `volunteer` receives: the union of their sources, without repeats, newest first
/// (ties by id), then the final `is_recipient` check.
pub(super) fn arrange(mut alerts: Vec<AlertDoc>, volunteer: &Volunteer) -> Vec<AlertDoc> {
    alerts.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    alerts.dedup_by(|a, b| a.id == b.id);
    alerts.retain(|alert| is_recipient(volunteer, alert));
    alerts
}
