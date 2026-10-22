//! The stored alert and the vocabulary around it (spec 7, 8.8).

use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::store;

pub const COLLECTION: &str = "alerts";
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    ShelterFlagged,
    EventOverlap,
    VolunteerRemoved,
    EndorserFlagged,
    ChildSeparated,
    RevalidationFailed,
    ExceptionalExit,
    HouseholdConflict,
    UnicoLimit,
    /// Phase 4: an active search without an accepted match when its event closes (spec 3.7 item 9).
    SearchExpiring,
    /// Phase 4: the family sent a search through the volunteer's link (spec 3.7 item 11); informational.
    FamilySearchReceived,
    /// Phase 6, informational (spec 4.10): volume of check-ins or rescues of one volunteer above the limit.
    AnomalyVolume,
    /// Phase 6, informational: the same person registered 50 km apart within an hour.
    AnomalyDistance,
    /// Phase 6, informational: a rescue farther than `EVENT_RADIUS_KM` from every shelter of the volunteer.
    OutOfArea,
}

impl AlertKind {
    /// The snake_case name the kind is stored under; a test keeps it equal to the serde name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ShelterFlagged => "shelter_flagged",
            Self::EventOverlap => "event_overlap",
            Self::VolunteerRemoved => "volunteer_removed",
            Self::EndorserFlagged => "endorser_flagged",
            Self::ChildSeparated => "child_separated",
            Self::RevalidationFailed => "revalidation_failed",
            Self::ExceptionalExit => "exceptional_exit",
            Self::HouseholdConflict => "household_conflict",
            Self::UnicoLimit => "unico_limit",
            Self::SearchExpiring => "search_expiring",
            Self::FamilySearchReceived => "family_search_received",
            Self::AnomalyVolume => "anomaly_volume",
            Self::AnomalyDistance => "anomaly_distance",
            Self::OutOfArea => "out_of_area",
        }
    }

    /// Kept for callers outside this module (audit actions).
    pub fn name(self) -> String {
        self.as_str().to_string()
    }

    /// Kinds that only the volunteers in `volunteer_ids` receive and resolve, not even the Civil
    /// Defense (they concern one volunteer's searches, which nobody else reads).
    pub(super) fn is_personal(self) -> bool {
        matches!(self, Self::SearchExpiring | Self::FamilySearchReceived)
    }

    /// Informational anomalies (spec 4.10): only validators and the Civil Defense read them, never
    /// the volunteer they are about nor the shelter admins.
    pub(super) fn is_anomaly(self) -> bool {
        matches!(
            self,
            Self::AnomalyVolume | Self::AnomalyDistance | Self::OutOfArea
        )
    }
}

/// The kinds a validator who is not Civil Defense receives (`is_recipient`).
pub(super) const VALIDATOR_KINDS: [AlertKind; 4] = [
    AlertKind::EndorserFlagged,
    AlertKind::AnomalyVolume,
    AlertKind::AnomalyDistance,
    AlertKind::OutOfArea,
];
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    pub kind: AlertKind,
    pub informational: bool,
    pub target_kind: String,
    pub target_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default)]
    pub shelter_ids: Vec<String>,
    #[serde(default)]
    pub volunteer_ids: Vec<String>,
    pub for_civil_defense: bool,
    pub for_protection: bool,
    pub detail: Value,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    pub resolved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
}

impl AlertDoc {
    /// An open alert for the Civil Defense about one thing; add shelters to also reach their admins.
    pub fn new(kind: AlertKind, target_kind: &str, target_id: &str, detail: Value) -> Self {
        Self {
            id: None,
            kind,
            informational: false,
            target_kind: target_kind.into(),
            target_id: target_id.into(),
            event_id: None,
            shelter_ids: vec![],
            volunteer_ids: vec![],
            for_civil_defense: true,
            for_protection: false,
            detail,
            created_at: store::now(),
            resolved: false,
            resolved_by: None,
        }
    }

    /// An informational alert for one volunteer alone (`search_expiring`, `family_search_received`).
    pub fn personal(
        kind: AlertKind,
        target_kind: &str,
        target_id: &str,
        volunteer_id: &str,
        detail: Value,
    ) -> Self {
        let mut alert = Self::new(kind, target_kind, target_id, detail)
            .about_volunteers(vec![volunteer_id.into()]);
        alert.informational = true;
        alert.for_civil_defense = false;
        alert
    }

    pub fn for_shelters(mut self, shelter_ids: Vec<String>) -> Self {
        self.shelter_ids = shelter_ids;
        self
    }

    pub fn about_volunteers(mut self, volunteer_ids: Vec<String>) -> Self {
        self.volunteer_ids = volunteer_ids;
        self
    }

    pub fn in_event(mut self, event_id: &str) -> Self {
        self.event_id = Some(event_id.into());
        self
    }
}
