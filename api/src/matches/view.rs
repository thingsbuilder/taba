//! The JSON a match is shown as (spec 4.11).

use serde_json::{Value, json};

use crate::people::PersonDoc;

/// The stays of a person, for who attends the match (spec 4.11).
pub(super) fn history_view(person: &PersonDoc) -> Value {
    let stays: Vec<Value> = person
        .stays
        .iter()
        .map(|s| {
            json!({
                "shelter_id": s.shelter_id,
                "entered_at": s.entered_at.to_string(),
                "left_at": s.left_at.map(|t| t.to_string()),
                "exit_reason": s.exit_reason,
            })
        })
        .collect();
    json!({ "stays": stays })
}
