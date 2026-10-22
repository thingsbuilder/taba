//! Reads of households and of their members, shared with `cards` and the people routes.

use firestore::FirestoreDb;

use super::model::{COLLECTION, HouseholdDoc, not_found, person_not_found};
use crate::auth::AppState;
use crate::error::ApiError;
use crate::people::{self, PersonDoc};
use crate::store;

/// Records activity on the event of the household's shelter after a write to it (spec 4.3.3).
pub async fn touch_event(state: &AppState, household_id: &str) {
    if let Ok(household) = load_household(&state.db, household_id).await {
        crate::members::touch_shelter_event(state, &household.created_shelter_id).await;
    }
}

pub async fn load_household(db: &FirestoreDb, id: &str) -> Result<HouseholdDoc, ApiError> {
    store::get(db, COLLECTION, id).await?.ok_or_else(not_found)
}

pub(super) async fn load_person(db: &FirestoreDb, id: &str) -> Result<PersonDoc, ApiError> {
    store::get(db, people::COLLECTION, id)
        .await?
        .ok_or_else(|| person_not_found(id))
}

/// The members that still have a document, in `member_ids` order.
pub async fn load_members(
    db: &FirestoreDb,
    household: &HouseholdDoc,
) -> Result<Vec<PersonDoc>, ApiError> {
    let mut members = Vec::new();
    for id in &household.member_ids {
        members.extend(store::get::<PersonDoc>(db, people::COLLECTION, id).await?);
    }
    Ok(members)
}
