//! Audit log (spec 4.10, 7): every write from phase 1 on records an `audit_log` entry.
//! Entries carry ids only, never a name or a photo. `expires_at` is one year ahead; the TTL
//! policy and the anomaly alerts come in phase 6.

use firestore::{FirestoreDb, FirestoreInstant, FirestoreTransaction, FirestoreWritePrecondition};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::store::fs_error;

const COLLECTION: &str = "audit_log";
const RETENTION_SECONDS: i64 = 365 * 24 * 3600;

/// `audit_log.legal_basis` (spec 10.1.1). Phase 1 only uses `FraudPrevention`.
#[allow(dead_code)]
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
    pub action: String,
    pub target_kind: String,
    pub target_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legal_basis: Option<LegalBasis>,
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
            action: action.into(),
            target_kind: target_kind.into(),
            target_id: target_id.into(),
            legal_basis: None,
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

    pub fn basis(mut self, basis: LegalBasis) -> Self {
        self.legal_basis = Some(basis);
        self
    }
}

/// Adds the entry to a transaction, so it commits together with the write it describes.
pub fn add(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    entry: &AuditEntry,
) -> Result<(), ApiError> {
    db.fluent()
        .update()
        .in_col(COLLECTION)
        .precondition(FirestoreWritePrecondition::Exists(false))
        .document_id(uuid::Uuid::new_v4().to_string())
        .object(entry)
        .add_to_transaction(tx)
        .map_err(fs_error)?;
    Ok(())
}

/// Test helper: a standalone entry.
#[cfg(test)]
pub async fn write(db: &FirestoreDb, entry: &AuditEntry) -> Result<(), ApiError> {
    db.fluent()
        .insert()
        .into(COLLECTION)
        .document_id(uuid::Uuid::new_v4().to_string())
        .object(entry)
        .execute::<()>()
        .await
        .map_err(fs_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_db;

    #[tokio::test]
    async fn entry_is_stored_with_a_one_year_timestamp_ttl() {
        let db = test_db().await;
        let entry = AuditEntry::new("session.create", "session", "abc")
            .by("vol-1")
            .on_device("dev-1")
            .in_shelter("shelter-1")
            .basis(LegalBasis::FraudPrevention);
        write(&db, &entry).await.unwrap();

        let stored: Vec<AuditEntry> = db
            .fluent()
            .select()
            .from(COLLECTION)
            .obj()
            .query()
            .await
            .unwrap();
        assert_eq!(stored.len(), 1);
        let got = &stored[0];
        assert_eq!(got.action, "session.create");
        assert_eq!(got.volunteer_id.as_deref(), Some("vol-1"));
        assert_eq!(got.device_id.as_deref(), Some("dev-1"));
        assert_eq!(got.shelter_id.as_deref(), Some("shelter-1"));
        assert_eq!(got.legal_basis, Some(LegalBasis::FraudPrevention));
        let year = got.expires_at.as_second() - got.at.as_second();
        assert_eq!(year, RETENTION_SECONDS);
    }

    #[tokio::test]
    async fn legal_basis_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&LegalBasis::FraudPrevention).unwrap(),
            "\"fraud_prevention\""
        );
    }
}
