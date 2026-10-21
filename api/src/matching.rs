//! Comparison of a person with the active searches, and creation of the match (spec 3.6 item 8,
//! 3.7 item 8, 4.8). Never reveals anything to the one who registered the person or the search.

use firestore::{FirestoreDb, FirestoreTransaction, path};
use sha2::{Digest, Sha256};

use crate::audit::{self, AuditEntry, LegalBasis};
use crate::error::ApiError;
use crate::matches::{self, MatchDoc, MatchStatus, MediatorKind};
use crate::people::{self, Level, PersonDoc};
use crate::searches::{self, SearchDoc};
use crate::shelters::{self, Confirmation, ShelterDoc};
use crate::store::{self, fs_error, tx_create};

/// `matches/{sha256(search_id|person_id)}` as lowercase hex (spec 7).
pub fn match_id(search_id: &str, person_id: &str) -> String {
    Sha256::digest(format!("{search_id}|{person_id}").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The queue of a match by the situation of the person found (spec 4.8): a child goes to
/// `protection` in any situation, a person in a `confirmed` shelter to that shelter, anyone
/// else (`unconfirmed` shelter, or no shelter) to `central`.
// ponytail: owner C (`matches.rs`) owns this rule; swap this call for `matches::queue_for` if C
// publishes it.
pub fn queue_for(
    is_child: bool,
    shelter: Option<&ShelterDoc>,
    fallback_shelter_id: Option<&str>,
) -> (MediatorKind, Option<String>) {
    if is_child {
        return (MediatorKind::Protection, None);
    }
    match shelter {
        Some(s) if s.confirmation == Confirmation::Confirmed => (
            MediatorKind::Shelter,
            s.id.clone()
                .or_else(|| fallback_shelter_id.map(str::to_string)),
        ),
        _ => (MediatorKind::Central, None),
    }
}

/// Creates the match of a search and a person if there is none yet. A match that exists in any
/// state (`open`, `declined`, `not_the_person`...) is never recreated, which is also what keeps
/// a closed pair closed. Reads inside the transaction, so two writers of the same pair end with
/// one match. Returns whether it created one.
pub async fn open_match(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    search_id: &str,
    person: &PersonDoc,
    person_id: &str,
    shelter: Option<&ShelterDoc>,
) -> Result<bool, ApiError> {
    let id = match_id(search_id, person_id);
    if store::get::<MatchDoc>(db, matches::COLLECTION, &id)
        .await?
        .is_some()
    {
        return Ok(false);
    }
    let (mediator_kind, mediator_shelter_id) = queue_for(
        person.is_child,
        shelter,
        person.current_shelter_id.as_deref(),
    );
    let doc = MatchDoc {
        id: None,
        search_id: search_id.to_string(),
        person_id: person_id.to_string(),
        is_child: person.is_child,
        mediator_kind,
        mediator_shelter_id,
        status: MatchStatus::Open,
        decided_by: None,
        decided_by_volunteer: None,
        decided_at: None,
        note: None,
        created_at: store::now(),
    };
    tx_create(db, tx, matches::COLLECTION, &id, &doc)?;
    audit::add(
        db,
        tx,
        &AuditEntry::new("match.create", "match", &id).basis(LegalBasis::VitalInterest),
    )?;
    Ok(true)
}

/// Called inside the transaction that registers a person under a token, to look for active
/// searches (`searches.person_ids` array-contains `person_id`, `status = active`) and create the
/// matches (`matches/{sha256(search_id|person_id)}`, idempotent; the queue by spec 4.8). Never
/// reveals anything to the one who registered the person.
///
/// Called by: `people::checkin_in` (every check-in), `people::migrate_in` (late biometrics),
/// `rescues::post_rescue` (a rescue). `shelter` is where the person is now, if anywhere.
/// A `no_biometrics` person has no token id and matches nothing.
pub async fn on_person_registered(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    person: &PersonDoc,
    person_id: &str,
    shelter: Option<&ShelterDoc>,
) -> Result<(), ApiError> {
    if person.level == Level::NoBiometrics {
        return Ok(());
    }
    let found: Vec<SearchDoc> = db
        .fluent()
        .select()
        .from(searches::COLLECTION)
        .filter(|q| {
            q.for_all([
                q.field(path!(SearchDoc::person_ids))
                    .array_contains(person_id),
                q.field(path!(SearchDoc::status)).eq("active"),
            ])
        })
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    for search in found {
        let Some(search_id) = search.id.as_deref() else {
            continue;
        };
        open_match(db, tx, search_id, person, person_id, shelter).await?;
    }
    Ok(())
}

/// The other direction: a search just registered is compared with the people already
/// registered under each of its `person_ids` (spec 3.7 item 5). It always reads every person,
/// found or not, so a search with no match does the same work as one with a match.
pub async fn on_search_registered(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    search_id: &str,
    person_ids: &[String],
) -> Result<(), ApiError> {
    for person_id in person_ids {
        let person: Option<PersonDoc> = store::get(db, people::COLLECTION, person_id).await?;
        let shelter: Option<ShelterDoc> = match person
            .as_ref()
            .and_then(|p| p.current_shelter_id.as_deref())
        {
            Some(id) => store::get(db, shelters::COLLECTION, id).await?,
            None => None,
        };
        if let Some(person) = person.filter(|p| p.level != Level::NoBiometrics) {
            open_match(db, tx, search_id, &person, person_id, shelter.as_ref()).await?;
        }
    }
    Ok(())
}
