//! The photos a family sent with a search, kept encrypted (`THUMBNAIL_KEY`, as the thumbnail)
//! only while the search has a match that is open, so the mediator can compare faces (spec 3.7,
//! 4.8). `search_photos/{search_id}.{n}`, one document per photo (each is under the 1 MiB limit
//! of a Firestore document). A search that makes no match at registration keeps nothing, and a
//! match made later finds no photos: they are not kept on the chance of one.
//!
//! They go away when the last open match of the search is decided, when the event of the search
//! closes, and with the event's discard.

use std::collections::BTreeMap;

use firestore::{FirestoreDb, FirestoreInstant, FirestoreTransaction};
use serde::{Deserialize, Serialize};

use crate::audit::{self, AuditEntry, LegalBasis};
use crate::error::ApiError;
use crate::searches::{self, SearchDoc};
use crate::store::{self, tx_create, tx_delete, tx_replace};
use crate::thumbnail;

pub const COLLECTION: &str = "search_photos";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhotoDoc {
    pub search_id: String,
    /// `thumbnail::encrypt` of the photo as the family sent it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::thumbnail::optional_bytes"
    )]
    pub photo_encrypted: Option<Vec<u8>>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
}

pub fn photo_id(search_id: &str, n: u32) -> String {
    format!("{search_id}.{n}")
}

fn audit_counts(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    action: &str,
    search_id: &str,
    n: u32,
) -> Result<(), ApiError> {
    let counts = BTreeMap::from([("images".to_string(), n)]);
    audit::add(
        db,
        tx,
        &AuditEntry::new(action, "search", search_id)
            .basis(LegalBasis::VitalInterest)
            .with_counts(counts),
    )
}

/// Encrypts and writes the photos of a search that has a match, in the transaction that wrote the
/// search; returns how many.
pub fn retain(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    key: &[u8],
    search_id: &str,
    photos: &[Vec<u8>],
) -> Result<u32, ApiError> {
    if photos.is_empty() {
        return Ok(0);
    }
    for (n, photo) in photos.iter().enumerate() {
        let doc = PhotoDoc {
            search_id: search_id.to_string(),
            photo_encrypted: Some(thumbnail::encrypt(key, photo)?),
            created_at: store::now(),
        };
        tx_create(db, tx, COLLECTION, &photo_id(search_id, n as u32), &doc)?;
    }
    let n = photos.len() as u32;
    audit_counts(db, tx, "search_photos.retain", search_id, n)?;
    Ok(n)
}

/// Deletes the photos kept for the search, if any, and says so on the search. `reason` is the
/// audit action's suffix: `decided`, `event_closed`.
pub fn discard(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    search_id: &str,
    search: &mut SearchDoc,
    reason: &str,
) -> Result<(), ApiError> {
    let n = search.retained_photos;
    if n == 0 {
        return Ok(());
    }
    for i in 0..n {
        tx_delete(db, tx, COLLECTION, &photo_id(search_id, i))?;
    }
    search.retained_photos = 0;
    tx_replace(db, tx, searches::COLLECTION, search_id, search)?;
    audit_counts(
        db,
        tx,
        &format!("search_photos.discard.{reason}"),
        search_id,
        n,
    )
}

/// Test only: every photo document (their bytes are not JSON, so `all_docs` cannot read them).
#[cfg(test)]
pub async fn all(db: &FirestoreDb) -> Vec<PhotoDoc> {
    db.fluent()
        .select()
        .from(COLLECTION)
        .obj()
        .query()
        .await
        .unwrap()
}
