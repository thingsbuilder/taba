//! The JSON a household is shown as: names and levels only (spec 10.3, criterion 35).

use serde_json::{Value, json};

use super::model::HouseholdDoc;
use crate::people::PersonDoc;

/// Name and level only: no shelter, no history (spec 10.3).
pub fn members_view(members: &[PersonDoc]) -> Vec<Value> {
    members
        .iter()
        .map(|p| {
            json!({
                "person_id": p.id,
                "name": p.name,
                "level": p.level,
            })
        })
        .collect()
}

pub(super) fn view(household: &HouseholdDoc, members: &[PersonDoc]) -> Value {
    json!({
        "id": household.id,
        "members": members_view(members),
        "head_id": household.head_id,
        "pets": household.pets,
        "card_code": household.card_code,
    })
}
