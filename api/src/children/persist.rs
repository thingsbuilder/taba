//! The alert a child's write owes, added to the transaction of that write.

use firestore::{FirestoreDb, FirestoreTransaction};
use serde_json::json;

use crate::alerts::{self, AlertDoc, AlertKind};
use crate::error::ApiError;
use crate::events::{self, EventDoc, ReferenceContacts};
use crate::people::ChildStatus;
use crate::shelters::ShelterDoc;
use crate::store;

/// Adds the `child_separated` alert to the transaction of the write that made the child
/// `separated` or `unaccompanied`; a no-op for `with_guardian`. The alert reaches the
/// protection queue, the Civil Defense and the admins of the shelter, and carries the
/// reference phones of the event (no push, spec 3.4.1, item 3).
pub async fn raise_child_separated(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    person_id: &str,
    shelter_id: &str,
    shelter: &ShelterDoc,
    status: ChildStatus,
) -> Result<(), ApiError> {
    if status == ChildStatus::WithGuardian {
        return Ok(());
    }
    let contacts = match &shelter.event_id {
        Some(event_id) => store::get::<EventDoc>(db, events::COLLECTION, event_id)
            .await?
            .map(|event| event.reference_contacts)
            .unwrap_or_default(),
        None => ReferenceContacts::default(),
    };
    let mut alert = AlertDoc::new(
        AlertKind::ChildSeparated,
        "person",
        person_id,
        json!({ "child_status": status, "shelter_id": shelter_id, "reference_contacts": contacts }),
    )
    .for_shelters(vec![shelter_id.to_string()]);
    alert.for_protection = true;
    alert.event_id = shelter.event_id.clone();
    alerts::add(db, tx, &alert)
}
