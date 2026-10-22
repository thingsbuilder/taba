//! Reads and transactions on `system/status`: the gate before each Unico call, the two ways
//! in and the way out.

use firestore::FirestoreDb;
use firestore::jiff::SignedDuration;
use serde_json::json;

use super::model::{COLLECTION, DOCUMENT, StatusDoc};
use crate::alerts::{self, AlertDoc, AlertKind};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::error::ApiError;
use crate::status::{Reason, Unico};
use crate::store::{self, tx_replace};

/// How long `unavailable` lasts before the next real call tests Unico again (spec 5.7).
pub(super) const UNAVAILABLE_FOR: SignedDuration = SignedDuration::from_mins(5);

pub async fn read(db: &FirestoreDb) -> Result<Option<StatusDoc>, ApiError> {
    store::get(db, COLLECTION, DOCUMENT).await
}

/// Why Unico is off limits right now, if it is.
pub async fn blocking_reason(db: &FirestoreDb) -> Result<Option<Reason>, ApiError> {
    Ok(read(db)
        .await?
        .and_then(|doc| doc.active_reason(store::now())))
}

/// The monthly limit was reached: `degraded`/`limit` until `taba unico resume`, and one
/// `unico_limit` alert for the Civil Defense, written in the same transaction (spec 5.5).
/// Calls that arrive here while the limit is already recorded change nothing.
pub async fn enter_limit(db: &FirestoreDb) -> Result<(), ApiError> {
    store::transact(db, |db, tx| {
        Box::pin(async move {
            let now = store::now();
            let current = read(&db).await?;
            if current.and_then(|doc| doc.active_reason(now)) == Some(Reason::Limit) {
                return Ok(());
            }
            let doc = StatusDoc::degraded(Reason::Limit, now, None);
            tx_replace(&db, tx, COLLECTION, DOCUMENT, &doc)?;
            let alert = AlertDoc::new(AlertKind::UnicoLimit, "system", "unico", json!({}));
            alerts::add(&db, tx, &alert)
        })
    })
    .await
}

/// Three failures in a row: `degraded`/`unavailable` for five minutes. Never replaces the
/// limit (which needs the operator) nor extends a period already running.
pub async fn enter_unavailable(db: &FirestoreDb) -> Result<(), ApiError> {
    store::transact(db, |db, tx| {
        Box::pin(async move {
            let now = store::now();
            if read(&db)
                .await?
                .and_then(|doc| doc.active_reason(now))
                .is_some()
            {
                return Ok(());
            }
            let doc = StatusDoc::degraded(Reason::Unavailable, now, Some(now + UNAVAILABLE_FOR));
            tx_replace(&db, tx, COLLECTION, DOCUMENT, &doc)
        })
    })
    .await
}

/// `taba unico resume`: leaves the degraded mode, whatever the reason. Returns whether the
/// document said `degraded`. The server makes no test call, because every call is billed.
pub async fn resume(db: &FirestoreDb) -> Result<bool, ApiError> {
    store::transact(db, |db, tx| {
        Box::pin(async move {
            let degraded = read(&db)
                .await?
                .is_some_and(|doc| doc.unico == Unico::Degraded);
            if degraded {
                let doc = StatusDoc {
                    unico: Unico::Ok,
                    reason: None,
                    since: Some(store::now()),
                    until: None,
                };
                tx_replace(&db, tx, COLLECTION, DOCUMENT, &doc)?;
                audit::add(
                    &db,
                    tx,
                    &AuditEntry::new(super::ACTION_RESUME, "system", DOCUMENT)
                        .basis(LegalBasis::PublicPolicy),
                )?;
            }
            Ok(degraded)
        })
    })
    .await
}
