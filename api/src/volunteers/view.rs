//! The JSON a profile and `GET /v1/me` are shown as (spec 8.1).

use serde_json::{Value, json};

use super::model::VolunteerDoc;
use crate::auth::Volunteer;

/// What the app may see of a profile.
pub fn view(id: &str, doc: &VolunteerDoc) -> Value {
    json!({
        "id": id,
        "name": doc.name,
        "phone": doc.phone,
        "show_phone": doc.show_phone,
        "language": doc.language,
    })
}

pub fn me_body(volunteer: &Volunteer) -> Value {
    json!({
        "volunteer": view(&volunteer.id, &volunteer.doc),
        "global_roles": volunteer.doc.global_roles,
        "memberships": volunteer.memberships,
    })
}
