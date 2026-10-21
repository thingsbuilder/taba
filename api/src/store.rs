//! Small Firestore helpers shared by the handlers: error mapping and document writes.

use firestore::errors::FirestoreError;
use firestore::{FirestoreDb, FirestoreTransaction, FirestoreWritePrecondition};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// Maps a Firestore failure to a 500. An `ApiError` raised inside a transaction body comes
/// back wrapped in `ErrorInTransaction`; it is returned as it was raised.
pub fn fs_error(err: FirestoreError) -> ApiError {
    if let FirestoreError::ErrorInTransaction(inner) = &err {
        if let Some(api) = inner.source.downcast_ref::<ApiError>() {
            return api.clone();
        }
    }
    tracing::error!(%err, "firestore error");
    ApiError::internal()
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

/// Replaces a whole document.
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
