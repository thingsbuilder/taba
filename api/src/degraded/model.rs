//! The `system/status` document (spec 7) and where it lives.

use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};

use crate::status::{Reason, Unico};

/// Collection and document of the status (spec 7: `system/status`).
pub const COLLECTION: &str = "system";
pub const DOCUMENT: &str = "status";

/// The whole `system/status` document; `GET /v1/status` shows only `unico` and `reason`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusDoc {
    pub unico: Unico,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub since: Option<FirestoreInstant>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub until: Option<FirestoreInstant>,
}
