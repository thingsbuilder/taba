//! Small Firestore helpers shared by the handlers: error mapping and document writes.

use std::future::Future;
use std::pin::Pin;

use firestore::errors::{BackoffError, FirestoreError};
use firestore::{FirestoreDb, FirestoreInstant, FirestoreTransaction, FirestoreWritePrecondition};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// Maps a Firestore failure to a 500. An `ApiError` raised inside a transaction body comes
/// back wrapped in `ErrorInTransaction`; it is returned as it was raised.
/// The current time at the precision Firestore stores (microseconds), so a value written and
/// read back compares equal and a response matches the stored document.
pub fn now() -> FirestoreInstant {
    FirestoreInstant::from_microsecond(FirestoreInstant::now().as_microsecond())
        .expect("current time is in range")
}

/// Another transaction held the document for too long (Firestore `Aborted`, after its retries).
pub const CONTENTION: &str = "contention";

pub fn fs_error(err: FirestoreError) -> ApiError {
    if let FirestoreError::DatabaseError(db_err) = &err {
        if db_err.public.code == "Aborted" {
            tracing::warn!(%err, "firestore transaction aborted by contention");
            let mut busy = ApiError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, CONTENTION);
            busy.retry_after = Some(1);
            return busy;
        }
    }
    if let FirestoreError::ErrorInTransaction(inner) = &err {
        if let Some(api) = inner.source.downcast_ref::<ApiError>() {
            return api.clone();
        }
    }
    tracing::error!(%err, "firestore error");
    ApiError::internal()
}

/// Times a transaction that lost to another one is run again before the caller sees `contention`.
const MAX_ATTEMPTS: u32 = 12;

/// Runs `body` in a transaction. Reads must go through the `FirestoreDb` it receives: that
/// copy is bound to the transaction, so a document read there conflicts with a concurrent
/// write. A transaction Firestore aborts under contention runs again from the top, so it reads
/// what the winner wrote; `body` may therefore run more than once and must not keep state.
pub async fn transact<T, F>(db: &FirestoreDb, body: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: for<'b> Fn(
            FirestoreDb,
            &'b mut FirestoreTransaction<'_>,
        ) -> Pin<Box<dyn Future<Output = Result<T, ApiError>> + Send + 'b>>
        + Send
        + Sync,
{
    let mut attempt = 0;
    loop {
        attempt += 1;
        let ran = db
            .run_transaction(|db, tx| {
                let run = body(db, tx);
                Box::pin(async move { run.await.map_err(BackoffError::permanent) })
            })
            .await
            .map_err(fs_error);
        match ran {
            Err(busy) if busy.code == CONTENTION && attempt < MAX_ATTEMPTS => {
                tokio::time::sleep(std::time::Duration::from_millis(5 * u64::from(attempt))).await;
            }
            other => return other,
        }
    }
}

pub async fn get<T: DeserializeOwned + Send>(
    db: &FirestoreDb,
    collection: &str,
    id: &str,
) -> Result<Option<T>, ApiError> {
    db.fluent()
        .select()
        .by_id_in(collection)
        .obj()
        .one(id)
        .await
        .map_err(fs_error)
}

/// Replaces a whole document, creating it if needed.
pub async fn replace<T: Serialize + DeserializeOwned + Sync + Send>(
    db: &FirestoreDb,
    collection: &str,
    id: &str,
    doc: &T,
) -> Result<(), ApiError> {
    db.fluent()
        .update()
        .in_col(collection)
        .document_id(id)
        .object(doc)
        .execute::<()>()
        .await
        .map_err(fs_error)
}

/// Inside a transaction: inserts a document that must not exist yet.
pub fn tx_create<T: Serialize + DeserializeOwned + Sync + Send>(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    collection: &str,
    id: &str,
    doc: &T,
) -> Result<(), ApiError> {
    db.fluent()
        .update()
        .in_col(collection)
        .precondition(FirestoreWritePrecondition::Exists(false))
        .document_id(id)
        .object(doc)
        .add_to_transaction(tx)
        .map_err(fs_error)?;
    Ok(())
}

/// Inside a transaction: replaces a whole document.
pub fn tx_replace<T: Serialize + DeserializeOwned + Sync + Send>(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    collection: &str,
    id: &str,
    doc: &T,
) -> Result<(), ApiError> {
    db.fluent()
        .update()
        .in_col(collection)
        .document_id(id)
        .object(doc)
        .add_to_transaction(tx)
        .map_err(fs_error)?;
    Ok(())
}

pub fn tx_delete(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    collection: &str,
    id: &str,
) -> Result<(), ApiError> {
    db.fluent()
        .delete()
        .from(collection)
        .document_id(id)
        .add_to_transaction(tx)
        .map_err(fs_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn now_has_the_microsecond_precision_firestore_keeps() {
        let t = super::now();
        assert_eq!(t.as_nanosecond() % 1000, 0);
    }
}
