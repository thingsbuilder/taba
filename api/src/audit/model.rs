//! The audit entry (spec 4.10, 7, 10.1.1) and the activities the anomaly rules look at.

use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};

pub(super) const COLLECTION: &str = "audit_log";
pub(super) const RETENTION_SECONDS: i64 = 365 * 24 * 3600;

/// `audit_log.legal_basis` (spec 10.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegalBasis {
    Consent,
    VitalInterest,
    FraudPrevention,
    PublicPolicy,
    ChildProtection,
    FamilyConsent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub at: FirestoreInstant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volunteer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shelter_id: Option<String>,
    /// The volunteer whose queued item an admin took over (spec 9.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken_over_from: Option<String>,
    pub action: String,
    pub target_kind: String,
    pub target_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legal_basis: Option<LegalBasis>,
    /// What an operation that changes many documents changed, by collection (the discard); never ids of people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<std::collections::BTreeMap<String, u32>>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub expires_at: FirestoreInstant,
}

impl AuditEntry {
    pub fn new(action: impl Into<String>, target_kind: &str, target_id: &str) -> Self {
        let at = crate::store::now();
        Self {
            at,
            volunteer_id: None,
            device_id: None,
            shelter_id: None,
            taken_over_from: None,
            action: action.into(),
            target_kind: target_kind.into(),
            target_id: target_id.into(),
            legal_basis: None,
            counts: None,
            expires_at: at + firestore::jiff::SignedDuration::from_secs(RETENTION_SECONDS),
        }
    }

    pub fn by(mut self, volunteer_id: &str) -> Self {
        self.volunteer_id = Some(volunteer_id.into());
        self
    }

    pub fn on_device(mut self, device_id: &str) -> Self {
        self.device_id = Some(device_id.into());
        self
    }

    pub fn in_shelter(mut self, shelter_id: &str) -> Self {
        self.shelter_id = Some(shelter_id.into());
        self
    }

    pub fn with_counts(mut self, counts: std::collections::BTreeMap<String, u32>) -> Self {
        self.counts = Some(counts);
        self
    }

    pub fn taken_over_from(mut self, volunteer_id: Option<&str>) -> Self {
        self.taken_over_from = volunteer_id.map(str::to_string);
        self
    }

    pub fn basis(mut self, basis: LegalBasis) -> Self {
        self.legal_basis = Some(basis);
        self
    }
}

/// What an activity check looks at (spec 4.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Activity {
    Checkin,
    Rescue,
}
