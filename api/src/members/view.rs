//! The JSON a member is shown as (spec 8.4).

use serde_json::{Value, json};

use super::model::MembershipDoc;
use crate::volunteers::VolunteerDoc;

pub(super) fn member_row(doc: &MembershipDoc, volunteer: &VolunteerDoc) -> Value {
    let mut row = json!({
        "volunteer_id": doc.volunteer_id,
        "name": volunteer.name,
        "role": doc.role,
        "via": doc.via,
        "joined_at": doc.joined_at.to_string(),
    });
    if volunteer.show_phone {
        row["phone"] = json!(volunteer.phone);
    }
    row
}
