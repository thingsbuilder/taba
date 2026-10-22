//! Children (spec 3.4.1, 4.9): guardians, who may change `child_status`, and the
//! `child_separated` alert.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use firestore::{FirestoreDb, FirestoreTransaction};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::alerts::{self, AlertDoc, AlertKind};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::events::{self, EventDoc, ReferenceContacts};
use crate::people::{self, ChildStatus, Guardian, GuardianLink, Level, PersonDoc};
use crate::shelters::ShelterDoc;
use crate::store::{self, tx_replace};
use crate::volunteers::GlobalRole;

/// `child_status` changes after the check-in belong to `protection` and the Civil Defense
/// (spec 3.4.1, item 4); the same two also exit separated children (spec 2.3).
pub fn can_change_status(volunteer: &Volunteer) -> bool {
    volunteer.has_global(GlobalRole::Protection) || volunteer.has_global(GlobalRole::CivilDefense)
}

/// What a write asks of `is_child` and `child_status`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChildChange {
    pub is_child: Option<bool>,
    pub child_status: Option<ChildStatus>,
}

/// The single rule for `is_child` and `child_status` of a person who already exists; every
/// route that writes them goes through here. Anyone may raise the protection (declare a child,
/// or send the status a child already has); only `protection` and the Civil Defense
/// (`may_alter`) lower it, change the status or clear the condition. The two fields move
/// together: a status implies a child, and clearing the condition clears the status.
/// Returns the status the `child_separated` alert is owed for: `separated` or `unaccompanied`,
/// and only when it is new, so a repeated write raises nothing.
pub fn change_child_state(
    person: &mut PersonDoc,
    change: ChildChange,
    may_alter: bool,
) -> Result<Option<ChildStatus>, ApiError> {
    if change.is_child == Some(false) && change.child_status.is_some() {
        let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_person");
        error.detail = json!({ "field": "child_status" });
        return Err(error);
    }
    let clears = change.is_child == Some(false) && person.is_child;
    let alters = change
        .child_status
        .is_some_and(|status| Some(status) != person.child_status);
    if (clears || alters) && !may_alter {
        return Err(ApiError::protection_role_required());
    }
    if clears {
        person.is_child = false;
        person.child_status = None;
        return Ok(None);
    }
    let became_child = !person.is_child && (change.is_child == Some(true) || alters);
    if became_child {
        person.is_child = true;
    }
    if alters {
        person.child_status = change.child_status;
    }
    Ok(person
        .child_status
        .filter(|status| (alters || became_child) && *status != ChildStatus::WithGuardian))
}

fn protection_rank(status: Option<ChildStatus>) -> u8 {
    match status {
        None => 0,
        Some(ChildStatus::WithGuardian) => 1,
        Some(ChildStatus::Separated) => 2,
        Some(ChildStatus::Unaccompanied) => 3,
    }
}

/// Late biometrics folds the record `old` into `new`: the merged person is a child if either
/// was, with the more protective of the two statuses, so a fusion never lowers the protection.
/// Returns the status the alert is owed for when the fusion made `new` more protected.
pub fn merge_child_state(new: &mut PersonDoc, old: &PersonDoc) -> Option<ChildStatus> {
    let before = (new.is_child, new.child_status);
    new.is_child |= old.is_child;
    if protection_rank(old.child_status) > protection_rank(new.child_status) {
        new.child_status = old.child_status;
    }
    // a status belongs to a child
    if new.child_status.is_some() {
        new.is_child = true;
    }
    (before != (new.is_child, new.child_status))
        .then_some(new.child_status)
        .flatten()
        .filter(|status| *status != ChildStatus::WithGuardian)
}

/// Adds the `child_separated` alert to the transaction of the write that made the child
/// `separated` or `unaccompanied`; a no-op for `with_guardian`. The alert reaches the
/// protection queue, the Civil Defense and the admins of the shelter, and carries the
/// reference phones of the event (no push, spec 3.4.1, item 3).
pub async fn raise_child_separated(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    person_id: &str,
    shelter_id: &str,
    shelter: &ShelterDoc,
    status: ChildStatus,
) -> Result<(), ApiError> {
    if status == ChildStatus::WithGuardian {
        return Ok(());
    }
    let contacts = match &shelter.event_id {
        Some(event_id) => store::get::<EventDoc>(db, events::COLLECTION, event_id)
            .await?
            .map(|event| event.reference_contacts)
            .unwrap_or_default(),
        None => ReferenceContacts::default(),
    };
    let mut alert = AlertDoc::new(
        AlertKind::ChildSeparated,
        "person",
        person_id,
        json!({ "child_status": status, "shelter_id": shelter_id, "reference_contacts": contacts }),
    )
    .for_shelters(vec![shelter_id.to_string()]);
    alert.for_protection = true;
    alert.event_id = shelter.event_id.clone();
    alerts::add(db, tx, &alert)
}

// ---- POST /v1/people/{person_id}/guardians ----

#[derive(Debug, Clone, Deserialize)]
pub struct GuardianRequest {
    guardian_id: String,
    link: GuardianLink,
}

/// The entry for `request` as `volunteer` records it: only `document_checked` says who looked
/// at the document and when.
fn guardian_entry(request: &GuardianRequest, volunteer: &Volunteer) -> Guardian {
    let checked = request.link == GuardianLink::DocumentChecked;
    Guardian {
        person_id: request.guardian_id.clone(),
        link: request.link,
        checked_by: checked.then(|| volunteer.id.clone()),
        checked_at: checked.then(store::now),
    }
}

pub async fn post_guardian(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(person_id): Path<String>,
    Json(request): Json<GuardianRequest>,
) -> Result<Json<Value>, ApiError> {
    let guardians = store::transact(&state.db, |db, tx| {
        let (person_id, request, volunteer) =
            (person_id.clone(), request.clone(), volunteer.clone());
        Box::pin(async move {
            let mut child: PersonDoc = store::get(&db, people::COLLECTION, &person_id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "person_not_found"))?;
            let in_shelter = child
                .current_shelter_id
                .as_deref()
                .is_some_and(|shelter| volunteer.role_in(shelter).is_some());
            if !in_shelter && !can_change_status(&volunteer) {
                return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden"));
            }
            if let Some(shelter_id) = child.current_shelter_id.as_deref() {
                crate::events::ensure_shelter_open(&db, shelter_id).await?;
            }
            if !child.is_child {
                return Err(ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "not_a_child",
                ));
            }
            if request.guardian_id == person_id {
                return Err(ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_guardian",
                ));
            }
            let guardian: Option<PersonDoc> =
                store::get(&db, people::COLLECTION, &request.guardian_id).await?;
            if guardian.is_none_or(|g| g.level != Level::Verified) {
                return Err(ApiError::guardian_not_verified());
            }
            let entry = guardian_entry(&request, &volunteer);
            match child
                .guardians
                .iter_mut()
                .find(|g| g.person_id == entry.person_id)
            {
                Some(existing) => *existing = entry,
                None => child.guardians.push(entry),
            }
            child.sync_guardian_ids();
            child.updated_at = store::now();
            tx_replace(&db, tx, people::COLLECTION, &person_id, &child)?;
            let mut audit_entry = AuditEntry::new("person.guardian", "person", &person_id)
                .by(&volunteer.id)
                .basis(LegalBasis::ChildProtection);
            if let Some(shelter) = &child.current_shelter_id {
                audit_entry = audit_entry.in_shelter(shelter);
            }
            audit::add(&db, tx, &audit_entry)?;
            Ok(child.guardians)
        })
    })
    .await?;
    let view: Vec<Value> = guardians
        .iter()
        .map(|g| {
            json!({
                "person_id": g.person_id,
                "link": g.link,
                "checked_by": g.checked_by,
                "checked_at": g.checked_at.map(|t| t.to_string()),
            })
        })
        .collect();
    if let Ok(Some(child)) =
        store::get::<PersonDoc>(&state.db, people::COLLECTION, &person_id).await
    {
        if let Some(shelter_id) = child.current_shelter_id.as_deref() {
            crate::members::touch_shelter_event(&state, shelter_id).await;
        }
    }
    Ok(Json(json!({ "person_id": person_id, "guardians": view })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exits::fixtures::{child, load, person, put};
    use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, new_id, north_of_origin};

    struct World {
        app: TestApp,
        unconfirmed: String,
        confirmed: String,
        /// (label, actor); the labels name the matrix columns.
        actors: Vec<(&'static str, Actor)>,
    }

    impl World {
        async fn new() -> Self {
            let app = TestApp::new().await;
            let admin_unconfirmed = app.actor("AdminUnconfirmed").await;
            let unconfirmed = app
                .create_shelter(&admin_unconfirmed, "Nao confirmado", ORIGIN)
                .await;
            let admin_confirmed = app.actor("AdminConfirmed").await;
            let confirmed = app
                .confirmed_shelter(&admin_confirmed, "Confirmado", north_of_origin(200_000.0))
                .await;
            let code = app
                .invite(&admin_unconfirmed, &unconfirmed, "qr", "volunteer")
                .await;
            let volunteer_unconfirmed = app.join(&code, "VolunteerUnconfirmed").await;
            let code = app
                .invite(&admin_confirmed, &confirmed, "qr", "volunteer")
                .await;
            let volunteer_confirmed = app.join(&code, "VolunteerConfirmed").await;
            let mut actors = vec![
                ("stranger", app.actor("Stranger").await),
                ("volunteer_unconfirmed", volunteer_unconfirmed),
                ("admin_unconfirmed", admin_unconfirmed),
                ("volunteer_confirmed", volunteer_confirmed),
                ("admin_confirmed", admin_confirmed),
            ];
            for (label, role) in [
                ("validator", GlobalRole::Validator),
                ("mediator", GlobalRole::Mediator),
                ("civil_defense", GlobalRole::CivilDefense),
                ("protection", GlobalRole::Protection),
            ] {
                let actor = app.actor(label).await;
                app.grant(&actor, role).await;
                actors.push((label, actor));
            }
            Self {
                app,
                unconfirmed,
                confirmed,
                actors,
            }
        }

        fn actor(&self, label: &str) -> &Actor {
            &self.actors.iter().find(|(l, _)| *l == label).unwrap().1
        }

        async fn guardian(
            &self,
            link: &str,
            as_who: Option<&str>,
            child_id: &str,
            guardian_id: &str,
        ) -> (StatusCode, Value) {
            let path = format!("/v1/people/{child_id}/guardians");
            let body = Some(json!({ "guardian_id": guardian_id, "link": link }));
            match as_who {
                Some(label) => {
                    self.app
                        .send("POST", &path, &self.actor(label).secret, body)
                        .await
                }
                None => {
                    let (status, _, json) = self.app.request("POST", &path, &[], body).await;
                    (status, json)
                }
            }
        }

        async fn verified_guardian(&self) -> String {
            let id = new_id();
            put(&self.app, &id, &person(Level::Verified, None)).await;
            id
        }
    }

    fn code_of(body: &Value) -> &str {
        body["error"].as_str().unwrap_or("")
    }

    #[tokio::test]
    async fn a_declared_guardian_is_recorded_without_who_checked() {
        let w = World::new().await;
        let guardian = w.verified_guardian().await;
        let kid = new_id();
        put(
            &w.app,
            &kid,
            &child(&w.unconfirmed, ChildStatus::WithGuardian, vec![]),
        )
        .await;
        let (status, body) = w
            .guardian("declared", Some("volunteer_unconfirmed"), &kid, &guardian)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let stored = load(&w.app, &kid).await;
        assert_eq!(stored.guardians.len(), 1);
        assert_eq!(stored.guardians[0].person_id, guardian);
        assert_eq!(stored.guardians[0].link, GuardianLink::Declared);
        assert_eq!(stored.guardians[0].checked_by, None);
        assert!(stored.guardians[0].checked_at.is_none());
    }

    #[tokio::test]
    async fn a_document_check_records_who_and_when_and_replaces_the_declared_link() {
        let w = World::new().await;
        let guardian = w.verified_guardian().await;
        let kid = new_id();
        put(
            &w.app,
            &kid,
            &child(&w.unconfirmed, ChildStatus::WithGuardian, vec![]),
        )
        .await;
        w.guardian("declared", Some("admin_unconfirmed"), &kid, &guardian)
            .await;
        let (status, body) = w
            .guardian(
                "document_checked",
                Some("volunteer_unconfirmed"),
                &kid,
                &guardian,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let stored = load(&w.app, &kid).await;
        assert_eq!(stored.guardians.len(), 1);
        assert_eq!(stored.guardians[0].link, GuardianLink::DocumentChecked);
        assert_eq!(
            stored.guardians[0].checked_by.as_deref(),
            Some(w.actor("volunteer_unconfirmed").id.as_str())
        );
        assert!(stored.guardians[0].checked_at.is_some());
        assert_eq!(body["guardians"][0]["link"], "document_checked");
        assert!(
            all_docs(&w.app.state.db, "audit_log")
                .await
                .iter()
                .any(|e| e["action"] == "person.guardian" && e["legal_basis"] == "child_protection")
        );
    }

    #[tokio::test]
    async fn a_guardian_must_exist_and_be_verified() {
        let w = World::new().await;
        let kid = new_id();
        put(
            &w.app,
            &kid,
            &child(&w.unconfirmed, ChildStatus::WithGuardian, vec![]),
        )
        .await;
        let assisted = new_id();
        put(&w.app, &assisted, &person(Level::Assisted, None)).await;
        let declined = new_id();
        put(&w.app, &declined, &person(Level::NoBiometrics, None)).await;
        for guardian in [assisted, declined, new_id()] {
            for link in ["declared", "document_checked"] {
                let (status, body) = w
                    .guardian(link, Some("admin_unconfirmed"), &kid, &guardian)
                    .await;
                assert_eq!(
                    (status, code_of(&body)),
                    (StatusCode::UNPROCESSABLE_ENTITY, "guardian_not_verified")
                );
            }
        }
        assert!(load(&w.app, &kid).await.guardians.is_empty());
    }

    #[tokio::test]
    async fn guardians_are_for_children_and_not_for_themselves() {
        let w = World::new().await;
        let guardian = w.verified_guardian().await;
        let adult = new_id();
        put(
            &w.app,
            &adult,
            &person(Level::Verified, Some(&w.unconfirmed)),
        )
        .await;
        let (status, body) = w
            .guardian("declared", Some("admin_unconfirmed"), &adult, &guardian)
            .await;
        assert_eq!(
            (status, code_of(&body)),
            (StatusCode::UNPROCESSABLE_ENTITY, "not_a_child")
        );
        let kid = new_id();
        let mut doc = child(&w.unconfirmed, ChildStatus::WithGuardian, vec![]);
        doc.level = Level::Verified;
        put(&w.app, &kid, &doc).await;
        let (status, body) = w
            .guardian("declared", Some("admin_unconfirmed"), &kid, &kid)
            .await;
        assert_eq!(
            (status, code_of(&body)),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_guardian")
        );
        let (status, _) = w
            .guardian("declared", Some("admin_unconfirmed"), &new_id(), &guardian)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn guardian_matrix() {
        let w = World::new().await;
        let guardian = w.verified_guardian().await;
        for shelter in [&w.unconfirmed, &w.confirmed] {
            let members: &[&str] = if *shelter == w.unconfirmed {
                &["volunteer_unconfirmed", "admin_unconfirmed"]
            } else {
                &["volunteer_confirmed", "admin_confirmed"]
            };
            for who in [
                None,
                Some("stranger"),
                Some("volunteer_unconfirmed"),
                Some("admin_unconfirmed"),
                Some("volunteer_confirmed"),
                Some("admin_confirmed"),
                Some("validator"),
                Some("mediator"),
                Some("civil_defense"),
                Some("protection"),
            ] {
                let kid = new_id();
                put(
                    &w.app,
                    &kid,
                    &child(shelter, ChildStatus::WithGuardian, vec![]),
                )
                .await;
                let (status, body) = w.guardian("document_checked", who, &kid, &guardian).await;
                let expected = match who {
                    None => StatusCode::UNAUTHORIZED,
                    Some(label)
                        if members.contains(&label)
                            || label == "civil_defense"
                            || label == "protection" =>
                    {
                        StatusCode::OK
                    }
                    Some(_) => StatusCode::FORBIDDEN,
                };
                assert_eq!(status, expected, "{who:?} in {shelter}: {body}");
            }
        }
    }

    // ---- child_separated (criterion 42) ----

    async fn raise(w: &World, shelter_id: &str, kid: &str, status: ChildStatus) {
        let shelter: ShelterDoc =
            store::get(&w.app.state.db, crate::shelters::COLLECTION, shelter_id)
                .await
                .unwrap()
                .unwrap();
        store::transact(&w.app.state.db, |db, tx| {
            let (shelter_id, shelter, kid) =
                (shelter_id.to_string(), shelter.clone(), kid.to_string());
            Box::pin(async move {
                raise_child_separated(&db, tx, &kid, &shelter_id, &shelter, status).await
            })
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn an_unaccompanied_child_alerts_with_the_reference_phones() {
        let w = World::new().await;
        let shelter: ShelterDoc =
            store::get(&w.app.state.db, crate::shelters::COLLECTION, &w.unconfirmed)
                .await
                .unwrap()
                .unwrap();
        let event_id = shelter.event_id.clone().unwrap();
        let (status, body) = w
            .app
            .send(
                "PATCH",
                &format!("/v1/events/{event_id}"),
                &w.actor("civil_defense").secret,
                Some(json!({ "reference_contacts": { "civil_defense_phone": "199", "child_protection_phone": "+55 51 3333-0000" } })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let kid = new_id();
        raise(&w, &w.unconfirmed, &kid, ChildStatus::WithGuardian).await;
        assert!(all_docs(&w.app.state.db, "alerts").await.is_empty());
        raise(&w, &w.unconfirmed, &kid, ChildStatus::Unaccompanied).await;
        raise(&w, &w.unconfirmed, &kid, ChildStatus::Separated).await;

        let alerts = all_docs(&w.app.state.db, "alerts").await;
        assert_eq!(alerts.len(), 2);
        let alert = alerts
            .iter()
            .find(|a| a["detail"]["child_status"] == "unaccompanied")
            .unwrap();
        assert_eq!(alert["kind"], "child_separated");
        assert_eq!(alert["target_id"], json!(kid));
        assert_eq!(alert["for_protection"], true);
        assert_eq!(alert["for_civil_defense"], true);
        assert_eq!(alert["informational"], false);
        assert_eq!(alert["shelter_ids"], json!([w.unconfirmed]));
        assert_eq!(
            alert["detail"]["reference_contacts"],
            json!({ "civil_defense_phone": "199", "child_protection_phone": "+55 51 3333-0000" })
        );

        // Who is told: protection, the Civil Defense and the admins of that shelter only.
        for (label, sees) in [
            ("protection", true),
            ("civil_defense", true),
            ("admin_unconfirmed", true),
            ("admin_confirmed", false),
        ] {
            let (status, listed) = w
                .app
                .send("GET", "/v1/alerts", &w.actor(label).secret, None)
                .await;
            if sees {
                assert_eq!(status, StatusCode::OK, "{label}");
                assert_eq!(listed["alerts"].as_array().unwrap().len(), 2, "{label}");
                assert_eq!(
                    listed["alerts"][0]["detail"]["reference_contacts"]["child_protection_phone"],
                    "+55 51 3333-0000"
                );
            } else {
                let empty = status == StatusCode::FORBIDDEN
                    || listed["alerts"].as_array().is_some_and(Vec::is_empty);
                assert!(empty, "{label}: {listed}");
            }
        }
        // A plain volunteer opens the box (for their own search alerts) and finds it empty.
        let (status, listed) = w
            .app
            .send(
                "GET",
                "/v1/alerts",
                &w.actor("volunteer_unconfirmed").secret,
                None,
            )
            .await;
        assert_eq!(
            (status, listed["alerts"].as_array().map(Vec::len)),
            (StatusCode::OK, Some(0))
        );
    }

    #[tokio::test]
    async fn without_reference_phones_the_alert_still_goes_out() {
        let w = World::new().await;
        raise(&w, &w.confirmed, &new_id(), ChildStatus::Separated).await;
        let alerts = all_docs(&w.app.state.db, "alerts").await;
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0]["detail"]["reference_contacts"], json!({}));
    }

    #[tokio::test]
    async fn only_protection_and_the_civil_defense_may_change_the_status() {
        let w = World::new().await;
        let volunteer = |label: &str| Volunteer {
            id: w.actor(label).id.clone(),
            doc: crate::volunteers::VolunteerDoc::from_profile(
                &serde_json::from_value(
                    json!({ "name": "x", "show_phone": false, "language": "pt-BR" }),
                )
                .unwrap(),
            )
            .unwrap(),
            memberships: vec![],
            session_id: String::new(),
        };
        let with_role = |role| {
            let mut v = volunteer("stranger");
            v.doc.global_roles.push(role);
            v
        };
        assert!(can_change_status(&with_role(GlobalRole::Protection)));
        assert!(can_change_status(&with_role(GlobalRole::CivilDefense)));
        assert!(!can_change_status(&with_role(GlobalRole::Validator)));
        assert!(!can_change_status(&with_role(GlobalRole::Mediator)));
        assert!(!can_change_status(&volunteer("stranger")));
    }

    // ---- the child state rule, without a database ----

    fn adult() -> PersonDoc {
        person(Level::Assisted, Some("s"))
    }

    fn kid(status: ChildStatus) -> PersonDoc {
        child("s", status, vec![])
    }

    fn change(is_child: Option<bool>, child_status: Option<ChildStatus>) -> ChildChange {
        ChildChange {
            is_child,
            child_status,
        }
    }

    #[test]
    fn anyone_may_declare_a_child_but_only_protection_lowers_or_changes_it() {
        use ChildStatus::*;
        // declaring raises the protection: allowed to anyone, alert owed for separated
        let mut p = adult();
        let alert = change_child_state(&mut p, change(Some(true), None), false).unwrap();
        assert_eq!((p.is_child, p.child_status, alert), (true, None, None));
        // a status for a person who had none is a change of status: protection only
        let error = change_child_state(&mut p, change(None, Some(Separated)), false).unwrap_err();
        assert_eq!(error.code, "protection_role_required");
        assert_eq!(p.child_status, None);
        let alert = change_child_state(&mut p, change(None, Some(Separated)), true).unwrap();
        assert_eq!(alert, Some(Separated));
        // clearing the condition, or changing the status, is refused to everyone else...
        for ask in [change(Some(false), None), change(None, Some(WithGuardian))] {
            let mut p = kid(Separated);
            let error = change_child_state(&mut p, ask, false).unwrap_err();
            assert_eq!(error.code, "protection_role_required", "{ask:?}");
            assert_eq!((p.is_child, p.child_status), (true, Some(Separated)));
        }
        // ...and allowed to protection, the two fields moving together
        let mut p = kid(Separated);
        assert_eq!(
            change_child_state(&mut p, change(Some(false), None), true).unwrap(),
            None
        );
        assert_eq!((p.is_child, p.child_status), (false, None));
        let mut p = kid(Separated);
        assert_eq!(
            change_child_state(&mut p, change(None, Some(WithGuardian)), true).unwrap(),
            None
        );
        assert_eq!((p.is_child, p.child_status), (true, Some(WithGuardian)));
    }

    #[test]
    fn repeating_a_child_state_changes_nothing_and_owes_no_alert() {
        use ChildStatus::*;
        let mut p = kid(Unaccompanied);
        for may_alter in [false, true] {
            let same = change(Some(true), Some(Unaccompanied));
            assert_eq!(change_child_state(&mut p, same, may_alter).unwrap(), None);
            assert_eq!(
                (p.is_child, p.child_status),
                (true, Some(Unaccompanied)),
                "{may_alter}"
            );
        }
        // clearing what is not set is not a lowering
        let mut p = adult();
        assert_eq!(
            change_child_state(&mut p, change(Some(false), None), false).unwrap(),
            None
        );
        // a status and "not a child" together is a contradiction
        let error =
            change_child_state(&mut p, change(Some(false), Some(Separated)), true).unwrap_err();
        assert_eq!(error.code, "invalid_person");
    }

    #[test]
    fn a_fusion_never_lowers_the_protection() {
        use ChildStatus::*;
        let alert = |new: PersonDoc, old: PersonDoc| {
            let mut new = new;
            let alert = merge_child_state(&mut new, &old);
            (new.is_child, new.child_status, alert)
        };
        assert_eq!(
            alert(adult(), kid(Separated)),
            (true, Some(Separated), Some(Separated))
        );
        assert_eq!(
            alert(kid(WithGuardian), kid(Unaccompanied)),
            (true, Some(Unaccompanied), Some(Unaccompanied))
        );
        // the more protected record wins whichever side it was on, and nothing new is owed
        assert_eq!(
            alert(kid(Separated), kid(WithGuardian)),
            (true, Some(Separated), None)
        );
        assert_eq!(
            alert(kid(Separated), adult()),
            (true, Some(Separated), None)
        );
        assert_eq!(alert(adult(), adult()), (false, None, None));
    }
}
