//! Reads of cards.

use firestore::FirestoreDb;

use super::model::{COLLECTION, CardDoc, card_not_found};
use crate::error::ApiError;
use crate::store;

pub(super) async fn load_card(db: &FirestoreDb, code: &str) -> Result<CardDoc, ApiError> {
    store::get(db, COLLECTION, code)
        .await?
        .ok_or_else(card_not_found)
}
