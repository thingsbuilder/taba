//! People written straight to the store, for the tests of this and other modules.

use crate::audit::LegalBasis;
use crate::people::{
    ChildStatus, Guardian, GuardianLink, Level, Needs, Origin, PersonDoc, Stay, Vulnerabilities,
};
use crate::store;
use crate::test_support::TestApp;

/// A person with an open stay in `shelter` (none: no stay).
pub fn person(level: Level, shelter: Option<&str>) -> PersonDoc {
    let now = store::now();
    PersonDoc {
        id: None,
        level,
        biometrics_declined: false,
        former_ids: vec![],
        name: Some("Maria".into()),
        social_name: None,
        wristband: None,
        birth_year: Some(1990),
        is_child: false,
        child_status: None,
        guardians: vec![],
        guardian_ids: vec![],
        needs: Needs::default(),
        vulnerabilities: Vulnerabilities::default(),
        household_id: None,
        current_shelter_id: shelter.map(str::to_string),
        stays: shelter
            .map(|s| {
                vec![Stay {
                    shelter_id: s.into(),
                    event_id: None,
                    entered_at: now,
                    left_at: None,
                    exit_reason: None,
                    exit_destination: None,
                    destination_municipality: None,
                    registered_by: "test".into(),
                }]
            })
            .unwrap_or_default(),
        event_ids: vec![],
        rescue_ids: vec![],
        thumbnail_encrypted: None,
        origin: Origin::Checkin,
        legal_basis: LegalBasis::Consent,
        created_at: now,
        updated_at: now,
        client_edited_at: None,
    }
}

pub fn child(
    shelter: &str,
    status: ChildStatus,
    guardians: Vec<(&str, GuardianLink)>,
) -> PersonDoc {
    let mut doc = person(Level::Assisted, Some(shelter));
    doc.is_child = true;
    doc.child_status = Some(status);
    doc.guardians = guardians
        .into_iter()
        .map(|(person_id, link)| Guardian {
            person_id: person_id.into(),
            link,
            checked_by: None,
            checked_at: None,
        })
        .collect();
    doc.sync_guardian_ids();
    doc
}

pub async fn put(app: &TestApp, id: &str, doc: &PersonDoc) {
    store::replace(&app.state.db, crate::people::COLLECTION, id, doc)
        .await
        .unwrap();
}

pub async fn load(app: &TestApp, id: &str) -> PersonDoc {
    store::get(&app.state.db, crate::people::COLLECTION, id)
        .await
        .unwrap()
        .unwrap()
}
