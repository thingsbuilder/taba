//! The JSON an invite and the people who came in by it are shown as (spec 8.4).

use serde_json::{Value, json};

use super::model::InviteDoc;
use super::persist::{Accepted, Listed};
use crate::auth::AppState;
use crate::members::MembershipDoc;
use crate::shelters::{self, ShelterDoc};
use crate::volunteers;

pub(super) fn url(state: &AppState, code: &str) -> String {
    format!("https://{}/invite/{code}", state.domain())
}

pub(super) fn invite_row(state: &AppState, l: &Listed) -> Value {
    json!({
        "code": l.id,
        "url": url(state, &l.id),
        "kind": l.invite.kind,
        "role": l.invite.role,
        "created_at": l.invite.created_at.to_string(),
        "expires_at": l.invite.expires_at.to_string(),
    })
}

pub(super) fn joined_row(member: &MembershipDoc, name: &Option<String>) -> Value {
    json!({
        "volunteer_id": member.volunteer_id,
        "name": name,
        "invite_code": member.invite_code,
        "joined_at": member.joined_at.to_string(),
    })
}

/// What the invited person sees before the selfie. An invite that cannot be used says so
/// and nothing else.
pub(super) fn preview(usable: Option<(InviteDoc, ShelterDoc)>) -> Value {
    match usable {
        Some((invite, shelter)) => json!({
            "valid": true,
            "shelter_name": shelter.name,
            "role": invite.role,
            "kind": invite.kind,
            "confirmed": shelter.confirmation == shelters::Confirmation::Confirmed,
        }),
        None => {
            json!({ "valid": false, "shelter_name": null, "role": null, "kind": null, "confirmed": null })
        }
    }
}

pub(super) fn accepted(secret: &str, person_id: &str, accepted: &Accepted) -> Value {
    json!({
        "session": secret,
        "expires_at": accepted.session.expires_at.to_string(),
        "volunteer": volunteers::view(person_id, &accepted.volunteer),
        "membership": {
            "shelter_id": accepted.shelter_id,
            "name": accepted.shelter.name,
            "role": accepted.role,
            "confirmation": accepted.shelter.confirmation,
        },
    })
}
