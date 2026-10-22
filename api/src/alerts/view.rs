//! The JSON an alert is shown as.

use serde_json::{Value, json};

use super::model::AlertDoc;

pub(super) fn view(alert: &AlertDoc) -> Value {
    json!({
        "id": alert.id,
        "kind": alert.kind,
        "informational": alert.informational,
        "target_kind": alert.target_kind,
        "target_id": alert.target_id,
        "event_id": alert.event_id,
        "shelter_ids": alert.shelter_ids,
        "volunteer_ids": alert.volunteer_ids,
        "detail": alert.detail,
        "created_at": alert.created_at.to_string(),
        "resolved": alert.resolved,
    })
}
