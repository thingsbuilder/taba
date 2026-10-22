//! The photos a family sent with a search, kept encrypted (`THUMBNAIL_KEY`, as the thumbnail)
//! only while the search has a match that is open, so the mediator can compare faces (spec 3.7,
//! 4.8). `search_photos/{search_id}.{n}`, one document per photo (each is under the 1 MiB limit
//! of a Firestore document). A search that makes no match at registration keeps nothing, and a
//! match made later finds no photos: they are not kept on the chance of one.
//!
//! They go away when the last open match of the search is decided, when the event of the search
//! closes, with the event's discard, or `PURGE_AFTER_DAYS` after the search was made (a match
//! nobody attends, a search with no event), whichever comes first.
//!
//! They are written after the answer of the search is sent, so the time of the answer does not
//! tell whether there was a match.

use std::collections::BTreeMap;

use firestore::{FirestoreDb, FirestoreInstant, FirestoreTransaction, path};
use serde::{Deserialize, Serialize};

use crate::audit::{self, AuditEntry, LegalBasis};
use crate::error::ApiError;
use crate::matches::{self, MatchDoc, MatchStatus};
use crate::searches::{self, SearchDoc, SearchStatus};
use crate::store::{self, fs_error, tx_create, tx_delete, tx_replace};
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

/// Whether the bytes start as a JPEG or a PNG: nothing else is kept.
fn is_image(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF])
        || bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
}

/// Encrypts and writes the photos of a search (the ones that are images) in a transaction;
/// returns how many.
pub fn write(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    key: &[u8],
    search_id: &str,
    photos: &[Vec<u8>],
) -> Result<u32, ApiError> {
    let photos: Vec<&Vec<u8>> = photos.iter().filter(|p| is_image(p)).collect();
    for (n, photo) in photos.iter().enumerate() {
        let doc = PhotoDoc {
            search_id: search_id.to_string(),
            photo_encrypted: Some(thumbnail::encrypt(key, photo)?),
            created_at: store::now(),
        };
        tx_create(db, tx, COLLECTION, &photo_id(search_id, n as u32), &doc)?;
    }
    let n = photos.len() as u32;
    if n > 0 {
        audit_counts(db, tx, "search_photos.retain", search_id, n)?;
    }
    Ok(n)
}

/// Whether the person of the search was found: one of its matches was accepted and none is open.
/// Declined and `not_the_person` matches do not count, the search stays active for other people.
/// `deciding` is a match whose new status is not written yet.
pub async fn found(
    db: &FirestoreDb,
    search_id: &str,
    deciding: Option<(&str, MatchStatus)>,
) -> Result<bool, ApiError> {
    let all: Vec<MatchDoc> = db
        .fluent()
        .select()
        .from(matches::COLLECTION)
        .filter(|q| q.for_all([q.field(path!(MatchDoc::search_id)).eq(search_id)]))
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    let statuses: Vec<MatchStatus> = all
        .iter()
        .map(|m| match deciding {
            Some((id, status)) if m.id.as_deref() == Some(id) => status,
            _ => m.status,
        })
        .collect();
    Ok(statuses.contains(&MatchStatus::Accepted) && !statuses.contains(&MatchStatus::Open))
}

/// Keeps the photos of an active search. Skips (writes nothing) when the search is gone, closed,
/// already has photos or was found while the photos waited to be written.
async fn keep(
    db: &FirestoreDb,
    key: &[u8],
    search_id: &str,
    photos: &[Vec<u8>],
) -> Result<(), ApiError> {
    store::transact(db, |db, tx| {
        let (key, search_id, photos) = (key.to_vec(), search_id.to_string(), photos.to_vec());
        Box::pin(async move {
            let Some(mut search) =
                store::get::<SearchDoc>(&db, searches::COLLECTION, &search_id).await?
            else {
                return Ok(());
            };
            if search.status != SearchStatus::Active
                || search.retained_photos > 0
                || found(&db, &search_id, None).await?
            {
                return Ok(());
            }
            search.retained_photos = write(&db, tx, &key, &search_id, &photos)?;
            tx_replace(&db, tx, searches::COLLECTION, &search_id, &search)
        })
    })
    .await
}

/// Writes the photos after the answer of the search went out (spec 10.3: the time of the answer
/// must not tell whether there was a match). If it fails, the search stays without photos, as one
/// with no match, and the failure is audited.
pub fn retain_later(db: FirestoreDb, key: Vec<u8>, search_id: String, photos: Vec<Vec<u8>>) {
    tokio::spawn(async move {
        #[cfg(test)]
        gate::wait(&search_id).await;
        if let Err(error) = keep(&db, &key, &search_id, &photos).await {
            tracing::error!(code = error.code, "family photos not kept");
            let entry = AuditEntry::new("search_photos.retain_failed", "search", &search_id)
                .basis(LegalBasis::VitalInterest);
            let _ = store::transact(&db, |db, tx| {
                let entry = entry.clone();
                Box::pin(async move { audit::add(&db, tx, &entry) })
            })
            .await;
        }
    });
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

/// Test only: holds the writing of one search until the test lets it go.
#[cfg(test)]
pub mod gate {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    use tokio::sync::Semaphore;

    fn gates() -> &'static Mutex<HashMap<String, Arc<Semaphore>>> {
        static GATES: OnceLock<Mutex<HashMap<String, Arc<Semaphore>>>> = OnceLock::new();
        GATES.get_or_init(Default::default)
    }

    pub fn close(search_id: &str) {
        gates()
            .lock()
            .unwrap()
            .insert(search_id.into(), Arc::new(Semaphore::new(0)));
    }

    pub fn open(search_id: &str) {
        if let Some(gate) = gates().lock().unwrap().remove(search_id) {
            gate.add_permits(1);
        }
    }

    pub async fn wait(search_id: &str) {
        let gate = gates().lock().unwrap().get(search_id).cloned();
        if let Some(gate) = gate {
            let _ = gate.acquire().await;
        }
    }
}
