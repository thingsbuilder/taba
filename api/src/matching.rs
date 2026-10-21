//! Comparison of a person with the active searches, and creation of the match (spec 3.6 item 8,
//! 3.7 item 8, 4.8). Stage 0: the hook exists and does nothing.
#![allow(dead_code)] // stage 0 scaffold: the owner of this module removes this line

use firestore::{FirestoreDb, FirestoreTransaction};

use crate::error::ApiError;
use crate::people::PersonDoc;
use crate::shelters::ShelterDoc;

/// Called inside the transaction that registers a person under a token, to look for active
/// searches (`searches.person_ids` array-contains `person_id`, `status = active`) and create the
/// matches (`matches/{sha256(search_id|person_id)}`, idempotent; the queue by spec 4.8). Never
/// reveals anything to the one who registered the person.
///
/// Called by: `people::checkin_in` (every check-in), `people::migrate_in` (late biometrics),
/// `rescues::post_rescue` (a rescue). `shelter` is where the person is now, if anywhere.
/// A `no_biometrics` person has no token id and matches nothing. Owner B writes the body.
pub async fn on_person_registered(
    _db: &FirestoreDb,
    _tx: &mut FirestoreTransaction<'_>,
    _person: &PersonDoc,
    _person_id: &str,
    _shelter: Option<&ShelterDoc>,
) -> Result<(), ApiError> {
    Ok(())
}
