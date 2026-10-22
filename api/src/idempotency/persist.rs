//! The claim and the kept photo answers in Firestore: creating or taking over a claim, ending
//! it with an answer or giving it back, and the person a photo was already paid for.

use axum::http::{HeaderMap, header};
use firestore::FirestoreDb;

use super::domain::{key_of, pending_for, usable_key};
use super::model::{
    Asker, COLLECTION, ClaimState, DIGEST_HEADER, IdempotencyDoc, PhotoResult, TTL,
};
use crate::auth::{AppState, SESSIONS, SessionDoc, session_id};
use crate::error::ApiError;
use crate::store;

/// Who the request is signed in as, by the session alone (the handler checks the rest).
pub(super) async fn signed_in_as(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let secret = headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    let session: SessionDoc = store::get(&state.db, SESSIONS, &session_id(secret))
        .await
        .ok()??;
    (store::now() < session.expires_at).then_some(session.volunteer_id)
}

pub(super) enum Claim {
    /// The key is ours; the number is this claim's `attempts`.
    Won(u32),
    /// The key belongs to a record, `pending` with a live lease or `done`.
    Existing(IdempotencyDoc),
}

/// Creates the `pending` record of the key, or takes over one whose lease lapsed (same asker,
/// same payload), or returns what is there. A record past its `expires_at` counts as absent.
pub(super) async fn claim(db: &FirestoreDb, key: &str, asker: &Asker) -> Result<Claim, ApiError> {
    // A copy that waits only reads, without a transaction: its reads must not hold the document
    // against the request that is about to settle it.
    let found = store::get::<IdempotencyDoc>(db, COLLECTION, key).await?;
    let now = store::now();
    let live = found.filter(|d| now < d.expires_at);
    match live {
        Some(doc) if !doc.lapsed(now) => Ok(Claim::Existing(doc)),
        Some(_) => take_over(db, key, asker).await,
        None => {
            let pending = pending_for(asker, 1);
            // `create` fails when another copy got there first; the next round reads its record.
            // An expired record is replaced in a transaction, as a lapsed one is.
            if store::create(db, COLLECTION, key, &pending).await? {
                Ok(Claim::Won(1))
            } else {
                take_over(db, key, asker).await
            }
        }
    }
}

/// Claims the key in a transaction that reads it again: a lapsed claim of the same request, or
/// an expired record, becomes ours; anything else is returned as it is.
async fn take_over(db: &FirestoreDb, key: &str, asker: &Asker) -> Result<Claim, ApiError> {
    store::transact(db, |db, tx| {
        let (key, asker) = (key.to_string(), asker.clone());
        Box::pin(async move {
            let now = store::now();
            let found = store::get::<IdempotencyDoc>(&db, COLLECTION, &key).await?;
            let live = found.as_ref().filter(|d| now < d.expires_at);
            if let Some(doc) = live {
                if !doc.lapsed(now) || doc.refusal(&asker).is_some() {
                    return Ok(Claim::Existing(doc.clone()));
                }
            }
            let attempts = live.map_or(1, |d| d.attempts + 1);
            let pending = pending_for(&asker, attempts);
            if found.is_some() {
                store::tx_replace(&db, tx, COLLECTION, &key, &pending)?;
            } else {
                store::tx_create(&db, tx, COLLECTION, &key, &pending)?;
            }
            Ok(Claim::Won(attempts))
        })
    })
    .await
}

/// Ends claim number `attempt`: stores the answer, or (`None`) deletes the claim. A claim that
/// another request took over meanwhile is not touched.
pub(super) async fn settle(
    db: &FirestoreDb,
    key: &str,
    attempt: u32,
    answer: Option<(u16, String)>,
) -> Result<(), ApiError> {
    store::transact(db, |db, tx| {
        let (key, answer) = (key.to_string(), answer.clone());
        Box::pin(async move {
            let Some(mut doc) = store::get::<IdempotencyDoc>(&db, COLLECTION, &key).await? else {
                return Ok(());
            };
            if doc.state != ClaimState::Pending || doc.attempts != attempt {
                return Ok(());
            }
            match answer {
                Some((http_status, body)) => {
                    doc.state = ClaimState::Done;
                    doc.lease_until = None;
                    doc.http_status = http_status;
                    doc.body = body;
                    store::tx_replace(&db, tx, COLLECTION, &key, &doc)
                }
                None => store::tx_delete(&db, tx, COLLECTION, &key),
            }
        })
    })
    .await
}

/// Gives the key back after a refusal, so the same item can be sent again.
pub(super) async fn release(state: &AppState, key: &str, attempt: u32) {
    if let Err(error) = settle(&state.db, key, attempt, None).await {
        tracing::warn!(
            key,
            code = error.code,
            "could not release the idempotency claim; it holds until its lease lapses"
        );
    }
}

/// The person a photo belongs to, asking Unico (a paid call) once per `Idempotency-Key` and
/// request content. The handlers that send a photo call Unico before their transaction, so a
/// request that fails after it (contention, a refusal) and is sent again with the same key would
/// pay a second time; the answer of the first call is kept under the key for as long as the key
/// lives and the retry reuses it (spec 8, 9.4). Without a usable key, or without the digest the
/// layer sets on the request, it simply asks.
pub async fn photo_person_id<F>(
    state: &AppState,
    headers: &HeaderMap,
    volunteer_id: &str,
    route: &str,
    ask: F,
) -> Result<String, ApiError>
where
    F: std::future::Future<Output = Result<String, ApiError>>,
{
    let digest = headers.get(DIGEST_HEADER).and_then(|v| v.to_str().ok());
    photo_person_id_of(state, headers, volunteer_id, route, digest, ask).await
}

/// `photo_person_id` for a caller that has its own digest of the photo.
pub async fn photo_person_id_of<F>(
    state: &AppState,
    headers: &HeaderMap,
    volunteer_id: &str,
    route: &str,
    digest: Option<&str>,
    ask: F,
) -> Result<String, ApiError>
where
    F: std::future::Future<Output = Result<String, ApiError>>,
{
    let key = key_of(headers).filter(|k| usable_key(k));
    let (Some(key), Some(digest)) = (key, digest.filter(|d| !d.is_empty())) else {
        return ask.await;
    };
    let id = format!("photo~{key}");
    if let Ok(Some(kept)) = store::get::<PhotoResult>(&state.db, COLLECTION, &id).await {
        if store::now() < kept.expires_at
            && kept.volunteer_id == volunteer_id
            && kept.route == route
            && kept.digest == digest
        {
            return Ok(kept.person_id);
        }
    }
    let person_id = ask.await?;
    let kept = PhotoResult {
        volunteer_id: volunteer_id.into(),
        route: route.into(),
        digest: digest.into(),
        person_id: person_id.clone(),
        expires_at: store::now() + TTL,
    };
    if let Err(error) = store::replace(&state.db, COLLECTION, &id, &kept).await {
        tracing::warn!(
            code = error.code,
            "could not keep the answer of Unico for the key"
        );
    }
    Ok(person_id)
}
