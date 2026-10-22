//! Reads and writes of the alerts module: Firestore queries, one per source of recipients.

use firestore::{FirestoreDb, FirestoreTransaction, path};

use super::domain::Source;
use super::model::{AlertDoc, AlertKind, COLLECTION, VALIDATOR_KINDS};
use crate::error::ApiError;
use crate::store::{fs_error, tx_create};

/// The most alerts one source returns (see `Source`): its newest `LIST_LIMIT` open ones.
// ponytail: each source is queried for its own recipients, so other recipients' alerts never push
// one out; a single source holding more than 200 open alerts leaves its oldest out of the listing.
// Add a cursor if one shelter, or the Civil Defense, ever keeps more than that open.
pub(super) const LIST_LIMIT: u32 = 200;
/// Adds the alert to a transaction, so it commits together with the change that raised it.
pub fn add(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    alert: &AlertDoc,
) -> Result<(), ApiError> {
    tx_create(db, tx, COLLECTION, &uuid::Uuid::new_v4().to_string(), alert)
}
/// The newest `LIST_LIMIT` open alerts of one source.
pub(super) async fn open_from(
    db: &FirestoreDb,
    source: &Source,
    volunteer_id: &str,
) -> Result<Vec<AlertDoc>, ApiError> {
    db.fluent()
        .select()
        .from(COLLECTION)
        .filter(|q| {
            let open = q.field(path!(AlertDoc::resolved)).eq(false);
            match source {
                Source::CivilDefense => {
                    q.for_all([open, q.field(path!(AlertDoc::for_civil_defense)).eq(true)])
                }
                Source::ValidatorKinds => q.for_all([
                    open,
                    q.field(path!(AlertDoc::kind))
                        .is_in(VALIDATOR_KINDS.map(AlertKind::as_str).to_vec()),
                ]),
                Source::Protection => {
                    q.for_all([open, q.field(path!(AlertDoc::for_protection)).eq(true)])
                }
                Source::Shelter(id) => q.for_all([
                    open,
                    q.field(path!(AlertDoc::informational)).eq(false),
                    q.field(path!(AlertDoc::shelter_ids)).array_contains(id),
                ]),
                Source::Personal => q.for_all([
                    open,
                    q.field(path!(AlertDoc::volunteer_ids))
                        .array_contains(volunteer_id),
                ]),
            }
        })
        .order_by([(
            path!(AlertDoc::created_at),
            firestore::FirestoreQueryDirection::Descending,
        )])
        .limit(LIST_LIMIT)
        .obj()
        .query()
        .await
        .map_err(fs_error)
}
