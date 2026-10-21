//! Mediation of a search (spec 4.8, 7, 8.7): the three queues, the check and the decision.

use axum::Json;
use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::auth::Volunteer;
use crate::error::ApiError;
use crate::people::PersonDoc;
use crate::shelters::{Confirmation, ShelterDoc};

pub const COLLECTION: &str = "matches";

/// Who attends the match (spec 4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediatorKind {
    Shelter,
    Central,
    Protection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchStatus {
    Open,
    Accepted,
    Declined,
    NotThePerson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecidedBy {
    Person,
    Operator,
}

/// `matches/{sha256(search_id|person_id)}`, spec 7.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    pub search_id: String,
    pub person_id: String,
    pub is_child: bool,
    pub mediator_kind: MediatorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mediator_shelter_id: Option<String>,
    pub status: MatchStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<DecidedBy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by_volunteer: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub decided_at: Option<FirestoreInstant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
}

/// `matches/{sha256(search_id|person_id)}` as lowercase hex.
// The matching module (owner B) calls it and `queue_for` when it creates a match.
#[allow(dead_code)]
pub fn match_id(search_id: &str, person_id: &str) -> String {
    Sha256::digest(format!("{search_id}|{person_id}").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The queue of a match for `person`, who is now in `shelter` if anywhere (spec 4.8): a child goes
/// to `protection` whatever the shelter; a confirmed shelter keeps its own match (and its id);
/// an unconfirmed shelter or none goes to `central`.
#[allow(dead_code)]
pub fn queue_for(
    person: &PersonDoc,
    shelter: Option<&ShelterDoc>,
) -> (MediatorKind, Option<String>) {
    if person.is_child {
        return (MediatorKind::Protection, None);
    }
    match shelter {
        Some(s) if s.confirmation == Confirmation::Confirmed && s.id.is_some() => {
            (MediatorKind::Shelter, s.id.clone())
        }
        _ => (MediatorKind::Central, None),
    }
}

/// `GET /v1/matches`: the queues the volunteer attends (spec 4.8).
pub async fn list_matches(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}

/// `GET /v1/matches/{id}`: what the check needs, without the family's contact.
pub async fn get_match(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}

/// `POST /v1/matches/{id}/decision`: `accept | decline | not_the_person`; the first to decide closes it.
pub async fn post_decision(_volunteer: Volunteer) -> Result<Json<Value>, ApiError> {
    Err(ApiError::not_implemented())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::LegalBasis;
    use crate::people::{Level, Needs, Origin, Vulnerabilities};
    use crate::test_support::{ORIGIN, TestApp, north_of_origin};
    use crate::{shelters, store};

    pub fn person(id: &str, shelter: Option<&str>, is_child: bool) -> PersonDoc {
        let now = store::now();
        PersonDoc {
            id: Some(id.into()),
            level: Level::Assisted,
            biometrics_declined: false,
            former_ids: vec![],
            name: Some("Maria Souza".into()),
            social_name: None,
            wristband: None,
            birth_year: None,
            is_child,
            child_status: None,
            guardians: vec![],
            guardian_ids: vec![],
            needs: Needs::default(),
            vulnerabilities: Vulnerabilities::default(),
            household_id: None,
            current_shelter_id: shelter.map(str::to_string),
            stays: vec![],
            event_ids: vec![],
            rescue_ids: vec![],
            thumbnail_encrypted: None,
            origin: Origin::Checkin,
            legal_basis: LegalBasis::VitalInterest,
            created_at: now,
            updated_at: now,
            client_edited_at: None,
        }
    }

    async fn shelter_doc(app: &TestApp, id: &str) -> ShelterDoc {
        store::get(&app.state.db, shelters::COLLECTION, id)
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn the_queue_follows_the_child_flag_and_the_shelter_confirmation() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let confirmed_id = app.confirmed_shelter(&admin, "Confirmed", ORIGIN).await;
        let confirmed = shelter_doc(&app, &confirmed_id).await;
        let other = app.actor("Other").await;
        let unconfirmed_id = app
            .create_shelter(&other, "Unconfirmed", north_of_origin(5_000.0))
            .await;
        let unconfirmed = shelter_doc(&app, &unconfirmed_id).await;

        let adult = person("p", Some(&confirmed_id), false);
        let child = person("c", Some(&confirmed_id), true);
        assert_eq!(
            queue_for(&adult, Some(&confirmed)),
            (MediatorKind::Shelter, Some(confirmed_id.clone()))
        );
        assert_eq!(
            queue_for(&adult, Some(&unconfirmed)),
            (MediatorKind::Central, None)
        );
        assert_eq!(queue_for(&adult, None), (MediatorKind::Central, None));
        // A child goes to protection in every situation.
        for shelter in [Some(&confirmed), Some(&unconfirmed), None] {
            assert_eq!(queue_for(&child, shelter), (MediatorKind::Protection, None));
        }
    }

    #[test]
    fn the_match_id_is_the_hex_sha256_of_search_and_person() {
        assert_eq!(
            match_id("s1", "p1"),
            "c372e68eabd2d42d99bebd3d1912fe75e1bd41fff6f634e74d2d1c9b2ac96774"
        );
        assert_ne!(match_id("s1", "p1"), match_id("s1", "p2"));
    }
}
