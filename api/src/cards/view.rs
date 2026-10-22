//! The JSON a card is shown as: its household's members by name and level only.

use serde_json::{Value, json};

use super::model::CardDoc;
use crate::households::members_view;
use crate::people::PersonDoc;

pub(super) fn view(card: &CardDoc, members: &[PersonDoc]) -> Value {
    json!({
        "code": card.code,
        "status": card.status,
        "household_id": card.household_id,
        "members": members_view(members),
    })
}
