//! Reads and writes of the searches module: the daily quota, the paid answer of Unico per photo and
//! the transaction that writes the search, audits it and compares it with the people registered.

use axum::http::HeaderMap;
use firestore::jiff::SignedDuration;
use firestore::{FirestoreDb, FirestoreTransaction};

use super::domain::{is_unusable_photo, quota_id};
use super::model::{
    COLLECTION, DayCount, RegisterOutcome, SearchDoc, SearchInput, SearchOwner, SearchStatus,
    SearchVia, search_id_taken,
};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::AppState;
use crate::error::ApiError;
use crate::idempotency;
use crate::matching;
use crate::search_photos;
use crate::store::{self, tx_create, tx_replace};

pub(super) const ROUTE: &str = "POST /v1/searches";
/// The quota counters live in the `idempotency` collection, which already has the TTL on `expires_at`.
pub(super) const QUOTA_COLLECTION: &str = idempotency::COLLECTION;
pub(super) const QUOTA_TTL: SignedDuration = SignedDuration::from_hours(48);
/// Takes one of the `limit` daily slots (`SEARCHES_PER_DAY`) of the volunteer inside the caller's
/// transaction: `429 search_limit` when none is left. Searches and family links share it.
pub async fn take_daily_slot(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    volunteer_id: &str,
    limit: u32,
    tz: &str,
) -> Result<(), ApiError> {
    let id = quota_id(volunteer_id, tz);
    let used = store::get::<DayCount>(db, QUOTA_COLLECTION, &id)
        .await?
        .map_or(0, |c| c.count);
    if used >= limit {
        return Err(ApiError::search_limit());
    }
    let next = DayCount {
        count: used + 1,
        expires_at: store::now() + QUOTA_TTL,
    };
    tx_replace(db, tx, QUOTA_COLLECTION, &id, &next)
}
/// Whether the day is already used up: a cheap look before paying Unico for the photos. The
/// slot itself is taken in the transaction that writes the search.
pub(super) async fn day_is_full(
    db: &FirestoreDb,
    volunteer_id: &str,
    limit: u32,
    tz: &str,
) -> Result<bool, ApiError> {
    let used = store::get::<DayCount>(db, QUOTA_COLLECTION, &quota_id(volunteer_id, tz))
        .await?
        .map_or(0, |c| c.count);
    Ok(used >= limit)
}
/// One token per photo; only the derived `person_id`s come out, in the order of the photos.
/// Each paid answer is kept under the owner's key, so a retry does not pay it again.
pub(super) async fn person_ids_of(
    state: &AppState,
    owner: &SearchOwner,
    photos: &[Vec<u8>],
) -> Result<Vec<String>, ApiError> {
    let mut found = Vec::new();
    for (index, photo) in photos.iter().enumerate() {
        let mut headers = HeaderMap::new();
        if let Some(key) = owner.key.as_deref().filter(|k| !k.is_empty()) {
            if let Ok(value) = format!("{key}.{index}").parse() {
                headers.insert("idempotency-key", value);
            }
        }
        let digest = idempotency::digest_of(&[photo]);
        let asked = idempotency::photo_person_id_of(
            state,
            &headers,
            &owner.volunteer_id,
            ROUTE,
            Some(&digest),
            async {
                let token = state.unico.token_from_image(photo).await?;
                Ok(state.person_id_for_token(&token))
            },
        )
        .await;
        match asked {
            Ok(person_id) => found.push(person_id),
            Err(error) if is_unusable_photo(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(found)
}
/// Writes the search, takes the daily slot when `limit` is given, audits and compares it with the
/// people already registered, in one transaction. An id that already exists for the same owner
/// is a repeat: it answers as the first time and changes nothing.
pub(super) async fn commit_search(
    db: &FirestoreDb,
    owner: &SearchOwner,
    input: &SearchInput,
    person_ids: &[String],
    usable_photos: u32,
    limit: Option<u32>,
    tz: &str,
) -> Result<u32, ApiError> {
    store::transact(db, |db, tx| {
        let (owner, input, tz) = (owner.clone(), input.clone(), tz.to_string());
        let person_ids = person_ids.to_vec();
        Box::pin(async move {
            if let Some(existing) = store::get::<SearchDoc>(&db, COLLECTION, &input.id).await? {
                return if existing.created_by == owner.volunteer_id {
                    Ok(existing.usable_photos)
                } else {
                    Err(search_id_taken())
                };
            }
            if let Some(limit) = limit {
                take_daily_slot(&db, tx, &owner.volunteer_id, limit, &tz).await?;
            }
            matching::on_search_registered(&db, tx, &input.id, &person_ids).await?;
            let search = SearchDoc {
                id: None,
                created_by: owner.volunteer_id.clone(),
                via: owner.via,
                origin_shelter_id: owner.origin_shelter_id.clone(),
                event_id: owner.event_id.clone(),
                person_ids: person_ids.clone(),
                photos_sent: input.photos.len() as u32,
                usable_photos,
                retained_photos: 0,
                missing_person: input.missing_person.clone(),
                family: input.family.clone(),
                status: SearchStatus::Active,
                expiry_notified_at: None,
                created_at: store::now(),
            };
            tx_create(&db, tx, COLLECTION, &input.id, &search)?;
            let mut entry = AuditEntry::new("search.create", "search", &input.id)
                .by(&owner.volunteer_id)
                .basis(LegalBasis::VitalInterest);
            if let Some(shelter_id) = &owner.origin_shelter_id {
                entry = entry.in_shelter(shelter_id);
            }
            if let Some(device) = &owner.device_id {
                entry = entry.on_device(device);
            }
            audit::add(&db, tx, &entry)?;
            Ok(usable_photos)
        })
    })
    .await
}
pub(super) async fn register(
    state: &AppState,
    owner: SearchOwner,
    input: SearchInput,
    limit: Option<u32>,
) -> Result<RegisterOutcome, ApiError> {
    // A repeat of a search already written answers from it, without asking Unico again.
    if let Some(existing) = store::get::<SearchDoc>(&state.db, COLLECTION, &input.id).await? {
        if existing.created_by != owner.volunteer_id {
            return Err(search_id_taken());
        }
        return Ok(RegisterOutcome {
            usable_photos: existing.usable_photos,
        });
    }
    if let Some(limit) = limit {
        if day_is_full(
            &state.db,
            &owner.volunteer_id,
            limit,
            &state.config.tz_pilot,
        )
        .await?
        {
            return Err(ApiError::search_limit());
        }
    }
    let tokens = person_ids_of(state, &owner, &input.photos).await?;
    let usable_photos = tokens.len() as u32;
    let mut person_ids: Vec<String> = Vec::new();
    for id in tokens {
        if !person_ids.contains(&id) {
            person_ids.push(id);
        }
    }
    // A family link whose photos all fail makes no search: the family is asked for other photos.
    if usable_photos == 0 && owner.via == SearchVia::FamilyLink {
        return Ok(RegisterOutcome { usable_photos: 0 });
    }
    let usable_photos = commit_search(
        &state.db,
        &owner,
        &input,
        &person_ids,
        usable_photos,
        limit,
        &state.config.tz_pilot,
    )
    .await?;
    // The family's photos are kept (encrypted) while the search is active, match or not, so a
    // match made later can show them. The writing is done after the answer, which therefore takes
    // the same time whether or not there was a match.
    if usable_photos > 0 {
        search_photos::retain_later(
            state.db.clone(),
            state.thumbnail_key().to_vec(),
            input.id.clone(),
            input.photos,
        );
    }
    Ok(RegisterOutcome { usable_photos })
}
/// The pipeline both `POST /v1/searches` and the family link's submit run (spec 3.7 items 5 and
/// 11): one token per photo, keep only the derived `person_id`s, write the search, try the match
/// now (`matching`), and answer the same whether or not there was one. `503 unico_degraded` when
/// Unico cannot be used, without writing anything. The daily limit is not taken here: the route
/// takes it, and the family link takes it when the link is made (`take_daily_slot`).
pub async fn register_photos(
    state: &AppState,
    owner: SearchOwner,
    input: SearchInput,
) -> Result<RegisterOutcome, ApiError> {
    register(state, owner, input, None).await
}
