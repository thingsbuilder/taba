//! Reads and writes of the matches module: Firestore queries and the audit transaction.
//! The rules live in `domain`; the HTTP contract in `mod.rs`.

use std::collections::HashMap;

use firestore::{FirestoreDb, path};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::model::{COLLECTION, LIST_LIMIT, MatchDoc, MatchStatus, MediatorKind, match_not_found};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::shelters::{self, ShelterDoc};
use crate::store::{self, fs_error};

/// Reads of other documents that run at the same time while listing.
const READ_CONCURRENCY: usize = 16;

pub(super) fn name_of<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

pub(super) async fn load_match(db: &FirestoreDb, id: &str) -> Result<MatchDoc, ApiError> {
    store::get(db, COLLECTION, id)
        .await?
        .ok_or_else(match_not_found)
}

/// The newest matches of one queue in one status: up to `LIST_LIMIT + 1`, the extra one telling
/// the caller that the page is not the whole queue.
pub(super) async fn queue_page(
    db: &FirestoreDb,
    kind: MediatorKind,
    shelter_id: Option<&str>,
    status: MatchStatus,
) -> Result<Vec<MatchDoc>, ApiError> {
    let (kind, status) = (name_of(kind), name_of(status));
    db.fluent()
        .select()
        .from(COLLECTION)
        .filter(|q| {
            let mut conditions = vec![
                q.field(path!(MatchDoc::mediator_kind)).eq(kind.clone()),
                q.field(path!(MatchDoc::status)).eq(status.clone()),
            ];
            if let Some(shelter_id) = shelter_id {
                conditions.push(
                    q.field(path!(MatchDoc::mediator_shelter_id))
                        .eq(shelter_id.to_string()),
                );
            }
            q.for_all(conditions)
        })
        .order_by([(
            path!(MatchDoc::created_at),
            firestore::FirestoreQueryDirection::Descending,
        )])
        .limit(LIST_LIMIT + 1)
        .obj()
        .query()
        .await
        .map_err(fs_error)
}

/// The documents of `collection` with the given ids, read `READ_CONCURRENCY` at a time; a
/// missing one is left out of the map.
pub(super) async fn get_many<T>(
    db: &FirestoreDb,
    collection: &'static str,
    ids: Vec<String>,
) -> Result<HashMap<String, T>, ApiError>
where
    T: DeserializeOwned + Send + 'static,
{
    let mut found = HashMap::new();
    for chunk in ids.chunks(READ_CONCURRENCY) {
        let mut reads = tokio::task::JoinSet::new();
        for id in chunk {
            let (db, id) = (db.clone(), id.clone());
            reads.spawn(async move {
                let doc = store::get::<T>(&db, collection, &id).await?;
                Ok::<_, ApiError>((id, doc))
            });
        }
        while let Some(read) = reads.join_next().await {
            let (id, doc) = read.map_err(|err| {
                tracing::error!(%err, "a read of the listing did not finish");
                ApiError::internal()
            })??;
            found.extend(doc.map(|doc| (id, doc)));
        }
    }
    Ok(found)
}

pub(super) async fn shelter_name(
    db: &FirestoreDb,
    shelter_id: &str,
) -> Result<Option<String>, ApiError> {
    let shelter: Option<ShelterDoc> = store::get(db, shelters::COLLECTION, shelter_id).await?;
    Ok(shelter.map(|s| s.name))
}

pub(super) async fn audit_now(state: &AppState, entry: AuditEntry) -> Result<(), ApiError> {
    store::transact(&state.db, |db, tx| {
        let entry = entry.clone();
        Box::pin(async move { audit::add(&db, tx, &entry) })
    })
    .await
}

pub(super) fn match_audit(
    action: &str,
    volunteer: &Volunteer,
    m: &MatchDoc,
    target: (&str, &str),
) -> AuditEntry {
    let entry = AuditEntry::new(action, target.0, target.1)
        .by(&volunteer.id)
        .basis(LegalBasis::VitalInterest);
    match &m.mediator_shelter_id {
        Some(shelter) => entry.in_shelter(shelter),
        None => entry,
    }
}
