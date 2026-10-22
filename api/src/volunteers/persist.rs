//! The write behind `PATCH /v1/me`: the profile fields and their audit entry in one transaction.

use firestore::errors::BackoffError;
use firestore::paths;

use super::model::{COLLECTION, VolunteerDoc};
use crate::audit::{self, AuditEntry};
use crate::auth::AppState;
use crate::error::ApiError;
use crate::store::fs_error;

pub(super) async fn save_profile(
    state: &AppState,
    id: &str,
    doc: &VolunteerDoc,
    entry: &AuditEntry,
) -> Result<(), ApiError> {
    state
        .db
        .run_transaction(|db, tx| {
            let (id, doc, entry) = (id.to_string(), doc.clone(), entry.clone());
            Box::pin(async move {
                let mut write = || -> Result<(), ApiError> {
                    // Only the profile fields: a role granted meanwhile must survive.
                    db.fluent()
                        .update()
                        .fields(paths!(VolunteerDoc::{name, phone, show_phone, language}))
                        .in_col(COLLECTION)
                        .document_id(&id)
                        .object(&doc)
                        .add_to_transaction(tx)
                        .map_err(fs_error)?;
                    audit::add(&db, tx, &entry)
                };
                write().map_err(BackoffError::permanent)
            })
        })
        .await
        .map_err(fs_error)
}
