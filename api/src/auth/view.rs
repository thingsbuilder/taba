//! The JSON a new capture and a new session are shown as (spec 5.2, 8.1).

use serde_json::{Value, json};

use super::model::{CaptureDoc, SessionDoc};
use crate::volunteers::{self, VolunteerDoc};

pub(super) fn capture_created(capture_id: &str, link: &str, doc: &CaptureDoc) -> Value {
    json!({ "capture_id": capture_id, "link": link, "expires_at": doc.expires_at.to_string() })
}

pub(super) fn session_opened(
    secret: &str,
    session: &SessionDoc,
    person_id: &str,
    volunteer: &VolunteerDoc,
) -> Value {
    json!({
        "session": secret,
        "expires_at": session.expires_at.to_string(),
        "volunteer": volunteers::view(person_id, volunteer),
    })
}
