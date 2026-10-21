//! Degraded mode (spec 5.5, 5.7): `system/status`, the monthly limit, three failures in a row,
//! the `503 unico_degraded` gate and `taba unico resume`. Owner D.
#![allow(dead_code)] // stage 0 scaffold of phase 3: the owner removes this once the module is wired

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use firestore::{FirestoreDb, FirestoreInstant};
use serde::{Deserialize, Serialize};

use crate::auth::AppState;
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

/// To be applied (`route_layer`) to the routes that need Unico: answers `503 unico_degraded`
/// while the mode is on. Stage 0: lets everything through.
pub async fn require_unico(
    State(_state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    next.run(request).await
}

/// `taba unico resume`: leaves the `limit` mode. Stage 0: not built.
pub async fn resume(_db: &FirestoreDb) -> anyhow::Result<()> {
    anyhow::bail!("taba unico resume is not implemented yet")
}
