//! Exits and occupancy (spec 3.8, 4.2): `POST /v1/shelters/{id}/exits` (adults, children,
//! exceptions) and "N of capacity" from the open stays. Owner C.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use firestore::{FirestoreDb, FirestoreInstant, path};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::alerts::{self, AlertDoc, AlertKind};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{self, AppState, CaptureDoc, Purpose, Volunteer, resolve_person, usable_capture};
use crate::children::can_change_status;
use crate::error::ApiError;
use crate::members;
use crate::people::{self, ChildStatus, ExitDestination, ExitReason, GuardianLink, PersonDoc};
use crate::shelters::{self, ShelterDoc};
use crate::store::{self, fs_error, tx_replace};

const CAPTURES: &str = "captures";
const MAX_MUNICIPALITY_CHARS: usize = 100;
const MAX_REASON_CHARS: usize = 500;

// ---- occupancy ----

/// People with an open stay in the shelter (spec 4.2). `current_shelter_id` is set exactly
/// while a stay is open.
pub async fn occupancy(db: &FirestoreDb, shelter_id: &str) -> Result<u32, ApiError> {
    #[derive(Deserialize)]
    struct Count {
        open: i64,
    }
    let rows: Vec<Count> = db
        .fluent()
        .select()
        .from(people::COLLECTION)
        .filter(|q| q.for_all([q.field(path!(PersonDoc::current_shelter_id)).eq(shelter_id)]))
        .aggregate(|a| a.fields([a.field("open").count()]))
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    Ok(rows
        .first()
        .map_or(0, |c| u32::try_from(c.open).unwrap_or(0)))
}

/// `capacity - occupancy`, never below zero; `None` while the shelter has no capacity set.
pub fn vacancies(capacity: Option<u32>, occupancy: u32) -> Option<u32> {
    capacity.map(|capacity| capacity.saturating_sub(occupancy))
}

// ---- POST /v1/shelters/{id}/exits ----

#[derive(Debug, Clone, Deserialize)]
pub struct Exception {
    reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExitRequest {
    person_id: String,
    #[serde(default)]
    exit_destination: Option<ExitDestination>,
    #[serde(default)]
    destination_municipality: Option<String>,
    #[serde(default)]
    guardian_id: Option<String>,
    #[serde(default)]
    capture_id: Option<String>,
    #[serde(default)]
    exception: Option<Exception>,
}

/// What an exit needs before the stay closes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Plan {
    Adult,
    Exception {
        reason: String,
    },
    /// The guardian must be revalidated with the capture (spec 3.8).
    Revalidate {
        guardian_id: String,
        capture_id: String,
    },
}

fn invalid(code: &'static str) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code)
}

fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

fn not_in_shelter() -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "not_in_shelter")
}

/// Who may do what (spec 2.3, 3.8). `member`: active member of the shelter; `privileged`:
/// `protection` or Civil Defense. Decides before anything is asked of Unico.
fn plan(
    member: bool,
    privileged: bool,
    request: &ExitRequest,
    person: &PersonDoc,
) -> Result<Plan, ApiError> {
    if let Some(exception) = &request.exception {
        if !person.is_child {
            return Err(invalid("exception_for_child_only"));
        }
        if !privileged {
            return Err(ApiError::protection_role_required());
        }
        let reason = exception.reason.trim();
        if reason.is_empty() || reason.chars().count() > MAX_REASON_CHARS {
            return Err(invalid("exception_reason_required"));
        }
        return Ok(Plan::Exception {
            reason: reason.to_string(),
        });
    }
    if !person.is_child {
        return if member {
            Ok(Plan::Adult)
        } else {
            Err(forbidden())
        };
    }
    let guardian = request
        .guardian_id
        .as_deref()
        .and_then(|id| person.guardians.iter().find(|g| g.person_id == id));
    let volunteer_may = member
        && person.child_status == Some(ChildStatus::WithGuardian)
        && guardian.is_some_and(|g| g.link == GuardianLink::DocumentChecked);
    if !privileged && !volunteer_may {
        return Err(ApiError::protection_role_required());
    }
    if request.guardian_id.is_none() {
        return Err(invalid("guardian_required"));
    }
    let guardian = guardian.ok_or_else(|| invalid("guardian_not_linked"))?;
    let capture_id = request
        .capture_id
        .clone()
        .ok_or_else(|| invalid("capture_required"))?;
    Ok(Plan::Revalidate {
        guardian_id: guardian.person_id.clone(),
        capture_id,
    })
}

fn open_stay(person: &PersonDoc, shelter_id: &str) -> Option<usize> {
    person
        .stays
        .iter()
        .position(|s| s.shelter_id == shelter_id && s.left_at.is_none())
}

fn municipality(value: &Option<String>) -> Result<Option<String>, ApiError> {
    let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > MAX_MUNICIPALITY_CHARS {
        return Err(invalid("invalid_destination_municipality"));
    }
    Ok(Some(value.to_string()))
}

/// Everything the transaction that closes the stay needs.
#[derive(Clone)]
struct Exit {
    shelter_id: String,
    volunteer: Volunteer,
    member: bool,
    privileged: bool,
    request: ExitRequest,
    destination: ExitDestination,
    municipality: Option<String>,
    /// The person the revalidation capture turned out to be, once it was resolved.
    revalidated: Option<String>,
}

/// One transaction: re-reads the person, re-checks the rules on what it read, consumes the
/// revalidation capture and closes the stay. Of two exits of the same person only one gets here.
async fn close_stay(
    db: &FirestoreDb,
    tx: &mut firestore::FirestoreTransaction<'_>,
    a: &Exit,
) -> Result<FirestoreInstant, ApiError> {
    let mut person: PersonDoc = store::get(db, people::COLLECTION, &a.request.person_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "person_not_found"))?;
    let index = open_stay(&person, &a.shelter_id).ok_or_else(not_in_shelter)?;
    let plan = plan(a.member, a.privileged, &a.request, &person)?;

    if let (Plan::Revalidate { capture_id, .. }, Some(resolved)) = (&plan, &a.revalidated) {
        let capture: CaptureDoc = store::get(db, CAPTURES, capture_id)
            .await?
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "capture_not_found"))?;
        if capture.consumed {
            return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
        }
        tx_replace(
            db,
            tx,
            CAPTURES,
            capture_id,
            &auth::consumed(capture, resolved),
        )?;
    }

    let now = store::now();
    let (reason, action) = match plan {
        Plan::Exception { .. } => (ExitReason::ExceptionalExit, "person.exit_exceptional"),
        _ => (ExitReason::Exit, "person.exit"),
    };
    let stay = &mut person.stays[index];
    stay.left_at = Some(now);
    stay.exit_reason = Some(reason);
    stay.exit_destination = Some(a.destination);
    stay.destination_municipality = a.municipality.clone();
    if person.current_shelter_id.as_deref() == Some(a.shelter_id.as_str()) {
        person.current_shelter_id = None;
    }
    person.updated_at = now;
    tx_replace(db, tx, people::COLLECTION, &a.request.person_id, &person)?;

    let basis = if person.is_child {
        LegalBasis::ChildProtection
    } else {
        person.legal_basis
    };
    audit::add(
        db,
        tx,
        &AuditEntry::new(action, "person", &a.request.person_id)
            .by(&a.volunteer.id)
            .in_shelter(&a.shelter_id)
            .basis(basis),
    )?;
    if let Plan::Exception { reason } = plan {
        alerts::add(
            db,
            tx,
            &AlertDoc::new(
                AlertKind::ExceptionalExit,
                "person",
                &a.request.person_id,
                json!({ "shelter_id": a.shelter_id, "by": a.volunteer.id, "reason": reason }),
            ),
        )?;
    }
    Ok(now)
}

/// A revalidation that did not match: the capture is spent and the protection queue is told.
/// The exit is not registered (spec 3.8.6).
async fn record_failed_revalidation(
    state: &AppState,
    a: &Exit,
    capture_id: &str,
    guardian_id: &str,
    resolved: &str,
) -> Result<(), ApiError> {
    store::transact(&state.db, |db, tx| {
        let (a, capture_id, guardian_id, resolved) = (
            a.clone(),
            capture_id.to_string(),
            guardian_id.to_string(),
            resolved.to_string(),
        );
        Box::pin(async move {
            let capture: CaptureDoc = store::get(&db, CAPTURES, &capture_id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "capture_not_found"))?;
            if capture.consumed {
                return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
            }
            tx_replace(
                &db,
                tx,
                CAPTURES,
                &capture_id,
                &auth::consumed(capture, &resolved),
            )?;
            let mut alert = AlertDoc::new(
                AlertKind::RevalidationFailed,
                "person",
                &a.request.person_id,
                json!({
                    "shelter_id": a.shelter_id,
                    "guardian_id": guardian_id,
                    "by": a.volunteer.id,
                }),
            )
            .for_shelters(vec![a.shelter_id.clone()]);
            alert.for_protection = true;
            alerts::add(&db, tx, &alert)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new(
                    "person.exit_revalidation_failed",
                    "person",
                    &a.request.person_id,
                )
                .by(&a.volunteer.id)
                .in_shelter(&a.shelter_id)
                .basis(LegalBasis::ChildProtection),
            )
        })
    })
    .await
}

pub async fn post_exit(
    State(state): State<AppState>,
    headers: HeaderMap,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
    Json(request): Json<ExitRequest>,
) -> Result<Json<Value>, ApiError> {
    let member = volunteer.role_in(&shelter_id).is_some();
    let privileged = can_change_status(&volunteer);
    if !member && !privileged {
        return Err(forbidden());
    }
    store::get::<ShelterDoc>(&state.db, shelters::COLLECTION, &shelter_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found"))?;
    let destination = request
        .exit_destination
        .ok_or_else(|| invalid("exit_destination_required"))?;
    let municipality = municipality(&request.destination_municipality)?;
    let person: PersonDoc = store::get(&state.db, people::COLLECTION, &request.person_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "person_not_found"))?;
    open_stay(&person, &shelter_id).ok_or_else(not_in_shelter)?;
    let decided = plan(member, privileged, &request, &person)?;

    let mut exit = Exit {
        shelter_id: shelter_id.clone(),
        volunteer: volunteer.clone(),
        member,
        privileged,
        request,
        destination,
        municipality,
        revalidated: None,
    };
    if let Plan::Revalidate {
        guardian_id,
        capture_id,
    } = &decided
    {
        let device = auth::device_id(&headers)?;
        let mut capture =
            usable_capture(&state, &device, capture_id, Purpose::Revalidation).await?;
        if capture.volunteer_id.as_deref() != Some(volunteer.id.as_str())
            || capture.shelter_id.as_deref() != Some(shelter_id.as_str())
            || capture.expected_person_id.as_deref() != Some(guardian_id.as_str())
        {
            return Err(invalid("wrong_capture_target"));
        }
        let resolved = resolve_person(&state, capture_id, &mut capture).await?;
        if &resolved != guardian_id {
            record_failed_revalidation(&state, &exit, capture_id, guardian_id, &resolved).await?;
            return Err(ApiError::revalidation_failed());
        }
        exit.revalidated = Some(resolved);
    }

    let left_at = store::transact(&state.db, |db, tx| {
        let exit = exit.clone();
        Box::pin(async move { close_stay(&db, tx, &exit).await })
    })
    .await?;
    members::touch_shelter_event(&state, &shelter_id).await;
    Ok(Json(json!({
        "person_id": exit.request.person_id,
        "left_at": left_at.to_string(),
        "exit_destination": destination,
    })))
}

#[cfg(test)]
pub(crate) mod fixtures {
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
}

#[cfg(test)]
mod tests {
    use super::fixtures::{child, load, person, put};
    use super::*;
    use crate::people::Level;
    use crate::test_support::{
        Actor, ORIGIN, TestApp, all_docs, audit_actions, new_id, north_of_origin,
    };
    use crate::volunteers::GlobalRole;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Who {
        Anonymous,
        Stranger,
        VolunteerUnconfirmed,
        AdminUnconfirmed,
        VolunteerConfirmed,
        AdminConfirmed,
        Validator,
        Mediator,
        CivilDefense,
        Protection,
    }

    const EVERYONE: [Who; 10] = [
        Who::Anonymous,
        Who::Stranger,
        Who::VolunteerUnconfirmed,
        Who::AdminUnconfirmed,
        Who::VolunteerConfirmed,
        Who::AdminConfirmed,
        Who::Validator,
        Who::Mediator,
        Who::CivilDefense,
        Who::Protection,
    ];

    struct World {
        app: TestApp,
        unconfirmed: String,
        confirmed: String,
        actors: Vec<(Who, Actor)>,
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
                (Who::Stranger, app.actor("Stranger").await),
                (Who::VolunteerUnconfirmed, volunteer_unconfirmed),
                (Who::AdminUnconfirmed, admin_unconfirmed),
                (Who::VolunteerConfirmed, volunteer_confirmed),
                (Who::AdminConfirmed, admin_confirmed),
            ];
            for (who, name, role) in [
                (Who::Validator, "Validator", GlobalRole::Validator),
                (Who::Mediator, "Mediator", GlobalRole::Mediator),
                (Who::CivilDefense, "CivilDefense", GlobalRole::CivilDefense),
                (Who::Protection, "Protection", GlobalRole::Protection),
            ] {
                let actor = app.actor(name).await;
                app.grant(&actor, role).await;
                actors.push((who, actor));
            }
            Self {
                app,
                unconfirmed,
                confirmed,
                actors,
            }
        }

        fn actor(&self, who: Who) -> Option<&Actor> {
            self.actors.iter().find(|(w, _)| *w == who).map(|(_, a)| a)
        }

        /// Members of the shelter, whatever their role.
        fn is_member(&self, who: Who, shelter: &str) -> bool {
            if shelter == self.unconfirmed {
                matches!(who, Who::VolunteerUnconfirmed | Who::AdminUnconfirmed)
            } else {
                matches!(who, Who::VolunteerConfirmed | Who::AdminConfirmed)
            }
        }

        async fn exit(
            &self,
            who: Option<&Actor>,
            shelter: &str,
            body: Value,
        ) -> (StatusCode, Value) {
            exit_as(&self.app, who, shelter, body).await
        }
    }

    async fn exit_as(
        app: &TestApp,
        who: Option<&Actor>,
        shelter: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let path = format!("/v1/shelters/{shelter}/exits");
        let auth = who.map(|a| format!("Bearer {}", a.secret));
        let mut headers = vec![];
        if let (Some(actor), Some(auth)) = (who, auth.as_deref()) {
            headers.push(("authorization", auth));
            headers.push(("x-device-id", actor.device.as_str()));
        }
        let (status, _, json) = app.request("POST", &path, &headers, Some(body)).await;
        (status, json)
    }

    fn guardian_token() -> (String, String) {
        let token = format!("tok-guardian-{}", new_id());
        let id = auth::derive_id(&[7u8; 32], &token);
        (token, id)
    }

    /// A revalidation capture of `who` for `guardian_id`, finished by the person behind `token`.
    async fn revalidation_capture(
        app: &TestApp,
        who: &Actor,
        shelter: &str,
        guardian_id: &str,
        token: &str,
    ) -> String {
        let auth = format!("Bearer {}", who.secret);
        let (status, _, body) = app
            .request(
                "POST",
                "/v1/captures",
                &[
                    ("authorization", auth.as_str()),
                    ("x-device-id", who.device.as_str()),
                ],
                Some(json!({
                    "purpose": "revalidation",
                    "shelter_id": shelter,
                    "expected_person_id": guardian_id,
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let capture_id = body["capture_id"].as_str().unwrap().to_string();
        let stored: CaptureDoc = store::get(&app.state.db, CAPTURES, &capture_id)
            .await
            .unwrap()
            .unwrap();
        app.unico.finish_with_token(&stored.process_id, token);
        capture_id
    }

    fn code_of(body: &Value) -> &str {
        body["error"].as_str().unwrap_or("")
    }

    // ---- vacancies ----

    #[test]
    fn vacancies_never_go_below_zero_and_need_a_capacity() {
        assert_eq!(vacancies(None, 5), None);
        assert_eq!(vacancies(Some(10), 4), Some(6));
        assert_eq!(vacancies(Some(3), 5), Some(0));
    }

    // ---- adult exits (criterion 37) ----

    #[tokio::test]
    async fn an_adult_exit_needs_the_destination() {
        let w = World::new().await;
        let admin = w.actor(Who::AdminUnconfirmed).unwrap();
        let id = new_id();
        put(
            &w.app,
            &id,
            &person(Level::NoBiometrics, Some(&w.unconfirmed)),
        )
        .await;
        let (status, body) = w
            .exit(Some(admin), &w.unconfirmed, json!({ "person_id": id }))
            .await;
        assert_eq!(
            (status, code_of(&body)),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "exit_destination_required"
            )
        );
        let (status, body) = w
            .exit(
                Some(admin),
                &w.unconfirmed,
                json!({ "person_id": id, "exit_destination": "moon" }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(load(&w.app, &id).await.current_shelter_id.is_some());
    }

    #[tokio::test]
    async fn every_destination_closes_the_stay_and_records_it() {
        let w = World::new().await;
        let volunteer = w.actor(Who::VolunteerUnconfirmed).unwrap();
        for destination in [
            "own_home",
            "relatives_or_friends",
            "rental_aid",
            "other_shelter",
            "other",
            "unknown",
        ] {
            let id = new_id();
            put(&w.app, &id, &person(Level::Verified, Some(&w.unconfirmed))).await;
            let (status, body) = w
                .exit(
                    Some(volunteer),
                    &w.unconfirmed,
                    json!({ "person_id": id, "exit_destination": destination, "destination_municipality": "  Canoas " }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{destination}: {body}");
            let stored = load(&w.app, &id).await;
            assert_eq!(stored.current_shelter_id, None);
            let stay = &stored.stays[0];
            assert!(stay.left_at.is_some());
            assert_eq!(stay.exit_reason, Some(ExitReason::Exit));
            assert_eq!(stay.destination_municipality.as_deref(), Some("Canoas"));
            assert_eq!(
                serde_json::to_value(stay.exit_destination).unwrap(),
                json!(destination)
            );
        }
        let exits = audit_actions(&w.app.state.db)
            .await
            .iter()
            .filter(|a| *a == "person.exit")
            .count();
        assert_eq!(exits, 6);
    }

    #[tokio::test]
    async fn a_long_municipality_is_refused() {
        let w = World::new().await;
        let admin = w.actor(Who::AdminUnconfirmed).unwrap();
        let id = new_id();
        put(&w.app, &id, &person(Level::Verified, Some(&w.unconfirmed))).await;
        let (status, body) = w
            .exit(
                Some(admin),
                &w.unconfirmed,
                json!({ "person_id": id, "exit_destination": "other", "destination_municipality": "x".repeat(101) }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }

    #[tokio::test]
    async fn the_person_must_be_in_the_shelter() {
        let w = World::new().await;
        let admin = w.actor(Who::AdminUnconfirmed).unwrap();
        let elsewhere = new_id();
        put(
            &w.app,
            &elsewhere,
            &person(Level::Verified, Some(&w.confirmed)),
        )
        .await;
        let gone = new_id();
        put(&w.app, &gone, &person(Level::Verified, None)).await;
        for id in [elsewhere, gone] {
            let (status, body) = w
                .exit(
                    Some(admin),
                    &w.unconfirmed,
                    json!({ "person_id": id, "exit_destination": "unknown" }),
                )
                .await;
            assert_eq!(
                (status, code_of(&body)),
                (StatusCode::CONFLICT, "not_in_shelter")
            );
        }
        let (status, _) = w
            .exit(
                Some(admin),
                &w.unconfirmed,
                json!({ "person_id": new_id(), "exit_destination": "unknown" }),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = w
            .exit(
                Some(admin),
                &new_id(),
                json!({ "person_id": new_id(), "exit_destination": "unknown" }),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    // ---- the matrix of 2.3 ----

    #[tokio::test]
    async fn adult_exit_matrix() {
        let w = World::new().await;
        for shelter in [&w.unconfirmed, &w.confirmed] {
            for who in EVERYONE {
                let id = new_id();
                put(&w.app, &id, &person(Level::Verified, Some(shelter))).await;
                let (status, body) = w
                    .exit(
                        w.actor(who),
                        shelter,
                        json!({ "person_id": id, "exit_destination": "own_home" }),
                    )
                    .await;
                let expected = match who {
                    Who::Anonymous => StatusCode::UNAUTHORIZED,
                    _ if w.is_member(who, shelter) => StatusCode::OK,
                    _ => StatusCode::FORBIDDEN,
                };
                assert_eq!(status, expected, "{who:?} in {shelter}: {body}");
                let left = load(&w.app, &id).await.current_shelter_id.is_none();
                assert_eq!(left, expected == StatusCode::OK, "{who:?}");
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Case {
        WithGuardianChecked,
        WithGuardianDeclared,
        Separated,
        Unaccompanied,
        Exception,
    }

    /// Who may register each kind of child exit (spec 2.3): members only for the first;
    /// protection and the Civil Defense for all.
    fn allowed(w: &World, who: Who, shelter: &str, case: Case) -> bool {
        match who {
            Who::CivilDefense | Who::Protection => true,
            _ => matches!(case, Case::WithGuardianChecked) && w.is_member(who, shelter),
        }
    }

    #[tokio::test]
    async fn child_exit_matrix() {
        let w = World::new().await;
        for shelter in [w.unconfirmed.clone(), w.confirmed.clone()] {
            for case in [
                Case::WithGuardianChecked,
                Case::WithGuardianDeclared,
                Case::Separated,
                Case::Unaccompanied,
                Case::Exception,
            ] {
                for who in EVERYONE {
                    let (token, guardian_id) = guardian_token();
                    let (child_status, link) = match case {
                        Case::WithGuardianChecked => {
                            (ChildStatus::WithGuardian, GuardianLink::DocumentChecked)
                        }
                        Case::WithGuardianDeclared => {
                            (ChildStatus::WithGuardian, GuardianLink::Declared)
                        }
                        Case::Separated => (ChildStatus::Separated, GuardianLink::DocumentChecked),
                        Case::Unaccompanied => {
                            (ChildStatus::Unaccompanied, GuardianLink::DocumentChecked)
                        }
                        Case::Exception => (ChildStatus::Separated, GuardianLink::Declared),
                    };
                    let id = new_id();
                    put(
                        &w.app,
                        &id,
                        &child(&shelter, child_status, vec![(&guardian_id, link)]),
                    )
                    .await;
                    let may_capture = w.is_member(who, &shelter)
                        || matches!(who, Who::CivilDefense | Who::Protection);
                    let body = if matches!(case, Case::Exception) {
                        json!({ "person_id": id, "exit_destination": "unknown", "exception": { "reason": "Mother at the gate, no papers" } })
                    } else {
                        let capture_id = match w.actor(who) {
                            Some(actor) if may_capture => {
                                revalidation_capture(&w.app, actor, &shelter, &guardian_id, &token)
                                    .await
                            }
                            _ => new_id(),
                        };
                        json!({ "person_id": id, "exit_destination": "relatives_or_friends", "guardian_id": guardian_id, "capture_id": capture_id })
                    };
                    let (status, answer) = w.exit(w.actor(who), &shelter, body).await;
                    let label = format!("{who:?} {case:?} in {shelter}: {answer}");
                    let stay_closed = load(&w.app, &id).await.current_shelter_id.is_none();
                    if who == Who::Anonymous {
                        assert_eq!(status, StatusCode::UNAUTHORIZED, "{label}");
                    } else if allowed(&w, who, &shelter, case) {
                        assert_eq!(status, StatusCode::OK, "{label}");
                    } else {
                        assert_eq!(status, StatusCode::FORBIDDEN, "{label}");
                        let expected = if w.is_member(who, &shelter) {
                            "protection_role_required"
                        } else {
                            "forbidden"
                        };
                        assert_eq!(code_of(&answer), expected, "{label}");
                    }
                    assert_eq!(stay_closed, status == StatusCode::OK, "{label}");
                }
            }
        }
    }

    // ---- children: criteria 43 to 45 ----

    struct Scene {
        w: World,
        child_id: String,
        guardian_id: String,
        token: String,
    }

    async fn scene(status: ChildStatus, link: GuardianLink) -> Scene {
        let w = World::new().await;
        let (token, guardian_id) = guardian_token();
        let child_id = new_id();
        put(
            &w.app,
            &child_id,
            &child(&w.unconfirmed, status, vec![(&guardian_id, link)]),
        )
        .await;
        Scene {
            w,
            child_id,
            guardian_id,
            token,
        }
    }

    impl Scene {
        async fn capture(&self, who: Who, token: &str) -> String {
            revalidation_capture(
                &self.w.app,
                self.w.actor(who).unwrap(),
                &self.w.unconfirmed,
                &self.guardian_id,
                token,
            )
            .await
        }

        fn body(&self, capture_id: &str) -> Value {
            json!({ "person_id": self.child_id, "exit_destination": "own_home", "guardian_id": self.guardian_id, "capture_id": capture_id })
        }

        async fn exit_as(&self, who: Who, body: Value) -> (StatusCode, Value) {
            self.w
                .exit(self.w.actor(who), &self.w.unconfirmed, body)
                .await
        }
    }

    #[tokio::test]
    async fn separated_child_exit_by_admin_is_refused_and_by_protection_is_done() {
        let s = scene(ChildStatus::Separated, GuardianLink::DocumentChecked).await;
        let capture = s.capture(Who::AdminUnconfirmed, &s.token).await;
        let (status, body) = s.exit_as(Who::AdminUnconfirmed, s.body(&capture)).await;
        assert_eq!(
            (status, code_of(&body)),
            (StatusCode::FORBIDDEN, "protection_role_required")
        );
        let capture = s.capture(Who::Protection, &s.token).await;
        let (status, body) = s.exit_as(Who::Protection, s.body(&capture)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let stored = load(&s.w.app, &s.child_id).await;
        assert_eq!(stored.stays[0].exit_reason, Some(ExitReason::Exit));
        assert_eq!(stored.current_shelter_id, None);
    }

    #[tokio::test]
    async fn declared_link_is_refused_to_a_volunteer_and_checked_link_is_done() {
        let declared = scene(ChildStatus::WithGuardian, GuardianLink::Declared).await;
        let capture = declared
            .capture(Who::VolunteerUnconfirmed, &declared.token)
            .await;
        let (status, body) = declared
            .exit_as(Who::VolunteerUnconfirmed, declared.body(&capture))
            .await;
        assert_eq!(
            (status, code_of(&body)),
            (StatusCode::FORBIDDEN, "protection_role_required")
        );

        let checked = scene(ChildStatus::WithGuardian, GuardianLink::DocumentChecked).await;
        let capture = checked
            .capture(Who::VolunteerUnconfirmed, &checked.token)
            .await;
        let (status, body) = checked
            .exit_as(Who::VolunteerUnconfirmed, checked.body(&capture))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    #[tokio::test]
    async fn a_failed_revalidation_blocks_the_exit_and_alerts_protection() {
        let s = scene(ChildStatus::WithGuardian, GuardianLink::DocumentChecked).await;
        let capture = s.capture(Who::AdminUnconfirmed, "tok-somebody-else").await;
        let (status, body) = s.exit_as(Who::AdminUnconfirmed, s.body(&capture)).await;
        assert_eq!(
            (status, code_of(&body)),
            (StatusCode::CONFLICT, "revalidation_failed")
        );
        assert!(
            load(&s.w.app, &s.child_id)
                .await
                .current_shelter_id
                .is_some()
        );
        let alerts = all_docs(&s.w.app.state.db, "alerts").await;
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0]["kind"], "revalidation_failed");
        assert_eq!(alerts[0]["for_protection"], true);
        assert_eq!(alerts[0]["target_id"], json!(s.child_id));
        // The capture is spent: the same one cannot be tried again.
        let (status, body) = s.exit_as(Who::AdminUnconfirmed, s.body(&capture)).await;
        assert_eq!(
            (status, code_of(&body)),
            (StatusCode::CONFLICT, "capture_consumed")
        );
        // The protection role sees the alert.
        let (_, listed) =
            s.w.app
                .send(
                    "GET",
                    "/v1/alerts",
                    &s.w.actor(Who::Protection).unwrap().secret,
                    None,
                )
                .await;
        assert_eq!(listed["alerts"][0]["kind"], "revalidation_failed");
    }

    #[tokio::test]
    async fn a_capture_is_used_once_and_only_for_what_it_was_asked() {
        let s = scene(ChildStatus::WithGuardian, GuardianLink::DocumentChecked).await;
        let admin = s.w.actor(Who::AdminUnconfirmed).unwrap();
        // A capture for another guardian.
        let other =
            revalidation_capture(&s.w.app, admin, &s.w.unconfirmed, "someone-else", &s.token).await;
        let (status, body) = s.exit_as(Who::AdminUnconfirmed, s.body(&other)).await;
        assert_eq!(
            (status, code_of(&body)),
            (StatusCode::UNPROCESSABLE_ENTITY, "wrong_capture_target")
        );
        // A capture of another volunteer (another device).
        let theirs = s.capture(Who::VolunteerUnconfirmed, &s.token).await;
        let (status, body) = s.exit_as(Who::AdminUnconfirmed, s.body(&theirs)).await;
        assert_eq!(
            (status, code_of(&body)),
            (StatusCode::FORBIDDEN, "capture_wrong_device")
        );
        // Missing pieces.
        for body in [
            json!({ "person_id": s.child_id, "exit_destination": "own_home", "guardian_id": s.guardian_id }),
            json!({ "person_id": s.child_id, "exit_destination": "own_home", "capture_id": new_id() }),
        ] {
            let (status, body) = s.exit_as(Who::AdminUnconfirmed, body).await;
            assert!(
                status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::FORBIDDEN,
                "{status} {body}"
            );
        }
        let (status, body) = s
            .exit_as(
                Who::Protection,
                json!({ "person_id": s.child_id, "exit_destination": "own_home", "guardian_id": "nobody", "capture_id": new_id() }),
            )
            .await;
        assert_eq!(
            (status, code_of(&body)),
            (StatusCode::UNPROCESSABLE_ENTITY, "guardian_not_linked")
        );
        let (status, body) = s
            .exit_as(
                Who::Protection,
                json!({ "person_id": s.child_id, "exit_destination": "own_home", "guardian_id": s.guardian_id }),
            )
            .await;
        assert_eq!(
            (status, code_of(&body)),
            (StatusCode::UNPROCESSABLE_ENTITY, "capture_required")
        );
        assert!(
            load(&s.w.app, &s.child_id)
                .await
                .current_shelter_id
                .is_some()
        );
        // The good one works once.
        let capture = s.capture(Who::AdminUnconfirmed, &s.token).await;
        let (status, body) = s.exit_as(Who::AdminUnconfirmed, s.body(&capture)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let stored: CaptureDoc = store::get(&s.w.app.state.db, CAPTURES, &capture)
            .await
            .unwrap()
            .unwrap();
        assert!(stored.consumed);
    }

    #[tokio::test]
    async fn exception_exit_is_for_protection_and_the_civil_defense_with_an_alert() {
        let s = scene(ChildStatus::Unaccompanied, GuardianLink::Declared).await;
        let body = json!({ "person_id": s.child_id, "exit_destination": "unknown", "exception": { "reason": "Aunt took the child" } });
        for who in [Who::AdminUnconfirmed, Who::VolunteerUnconfirmed] {
            let (status, answer) = s.exit_as(who, body.clone()).await;
            assert_eq!(
                (status, code_of(&answer)),
                (StatusCode::FORBIDDEN, "protection_role_required")
            );
        }
        assert!(all_docs(&s.w.app.state.db, "alerts").await.is_empty());
        let (status, answer) = s.exit_as(Who::CivilDefense, body).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let stored = load(&s.w.app, &s.child_id).await;
        assert_eq!(
            stored.stays[0].exit_reason,
            Some(ExitReason::ExceptionalExit)
        );
        let alerts = all_docs(&s.w.app.state.db, "alerts").await;
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0]["kind"], "exceptional_exit");
        assert_eq!(alerts[0]["for_civil_defense"], true);
        assert!(
            audit_actions(&s.w.app.state.db)
                .await
                .contains(&"person.exit_exceptional".to_string())
        );
    }

    #[tokio::test]
    async fn an_exception_needs_a_reason_and_a_child() {
        let s = scene(ChildStatus::Separated, GuardianLink::Declared).await;
        for reason in ["", "   ", &"x".repeat(501)] {
            let (status, _) = s
                .exit_as(
                    Who::Protection,
                    json!({ "person_id": s.child_id, "exit_destination": "unknown", "exception": { "reason": reason } }),
                )
                .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        }
        let adult = new_id();
        put(
            &s.w.app,
            &adult,
            &person(Level::Verified, Some(&s.w.unconfirmed)),
        )
        .await;
        let (status, body) = s
            .exit_as(
                Who::Protection,
                json!({ "person_id": adult, "exit_destination": "unknown", "exception": { "reason": "why" } }),
            )
            .await;
        assert_eq!(
            (status, code_of(&body)),
            (StatusCode::UNPROCESSABLE_ENTITY, "exception_for_child_only")
        );
    }

    // ---- concurrency ----

    #[tokio::test]
    async fn two_simultaneous_exits_of_the_same_person_register_one() {
        let w = World::new().await;
        let admin = w.actor(Who::AdminUnconfirmed).unwrap().clone();
        let id = new_id();
        put(&w.app, &id, &person(Level::Verified, Some(&w.unconfirmed))).await;
        let mut handles = vec![];
        for _ in 0..6 {
            let (app, admin, shelter, id) = (
                w.app.clone(),
                admin.clone(),
                w.unconfirmed.clone(),
                id.clone(),
            );
            handles.push(tokio::spawn(async move {
                exit_as(
                    &app,
                    Some(&admin),
                    &shelter,
                    json!({ "person_id": id, "exit_destination": "own_home" }),
                )
                .await
            }));
        }
        let mut ok = 0;
        for handle in handles {
            let (status, body) = handle.await.unwrap();
            match status {
                StatusCode::OK => ok += 1,
                StatusCode::CONFLICT => assert_eq!(code_of(&body), "not_in_shelter"),
                other => panic!("{other} {body}"),
            }
        }
        assert_eq!(ok, 1);
        let exits = audit_actions(&w.app.state.db)
            .await
            .iter()
            .filter(|a| *a == "person.exit")
            .count();
        assert_eq!(exits, 1);
    }

    #[tokio::test]
    async fn one_capture_registers_one_child_exit_even_when_sent_twice_at_once() {
        let s = scene(ChildStatus::WithGuardian, GuardianLink::DocumentChecked).await;
        let admin = s.w.actor(Who::AdminUnconfirmed).unwrap().clone();
        let capture = s.capture(Who::AdminUnconfirmed, &s.token).await;
        let mut handles = vec![];
        for _ in 0..4 {
            let (app, admin, shelter, body) = (
                s.w.app.clone(),
                admin.clone(),
                s.w.unconfirmed.clone(),
                s.body(&capture),
            );
            handles.push(tokio::spawn(async move {
                exit_as(&app, Some(&admin), &shelter, body).await
            }));
        }
        let mut ok = 0;
        for handle in handles {
            if handle.await.unwrap().0 == StatusCode::OK {
                ok += 1;
            }
        }
        assert_eq!(ok, 1);
    }

    // ---- occupancy ----

    #[tokio::test]
    async fn occupancy_counts_the_open_stays_and_drops_with_an_exit() {
        let w = World::new().await;
        let admin = w.actor(Who::AdminUnconfirmed).unwrap();
        let path = format!("/v1/shelters/{}", w.unconfirmed);
        let (status, _) = w
            .app
            .send(
                "PATCH",
                &path,
                &admin.secret,
                Some(json!({ "capacity": 5 })),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let (first, second) = (new_id(), new_id());
        put(
            &w.app,
            &first,
            &person(Level::Verified, Some(&w.unconfirmed)),
        )
        .await;
        put(
            &w.app,
            &second,
            &person(Level::Assisted, Some(&w.unconfirmed)),
        )
        .await;
        put(
            &w.app,
            &new_id(),
            &person(Level::Assisted, Some(&w.confirmed)),
        )
        .await;
        put(&w.app, &new_id(), &person(Level::Assisted, None)).await;

        let stranger = w.actor(Who::Stranger).unwrap();
        for viewer in [admin, stranger, w.actor(Who::CivilDefense).unwrap()] {
            let (_, body) = w.app.send("GET", &path, &viewer.secret, None).await;
            assert_eq!(body["shelter"]["occupancy"], 2, "{body}");
            assert_eq!(body["shelter"]["vacancies"], 3, "{body}");
        }
        let (status, _) = w
            .exit(
                Some(admin),
                &w.unconfirmed,
                json!({ "person_id": first, "exit_destination": "own_home" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let (_, body) = w.app.send("GET", &path, &admin.secret, None).await;
        assert_eq!(body["shelter"]["occupancy"], 1);
        assert_eq!(body["shelter"]["vacancies"], 4);
        let (_, patched) = w
            .app
            .send(
                "PATCH",
                &path,
                &admin.secret,
                Some(json!({ "capacity": 1 })),
            )
            .await;
        assert_eq!(patched["shelter"]["vacancies"], 0);
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;
    use crate::test_support::{ORIGIN, TestApp, new_id};

    #[tokio::test]
    async fn a_revalidation_capture_needs_a_session_a_target_and_a_member() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.create_shelter(&admin, "Ginasio", ORIGIN).await;
        let stranger = app.actor("Stranger").await;
        let body =
            json!({ "purpose": "revalidation", "shelter_id": shelter, "expected_person_id": "g1" });
        let call = |who: Option<&crate::test_support::Actor>, body: Value| {
            let app = app.clone();
            let who = who.cloned();
            async move {
                let auth = who.as_ref().map(|a| format!("Bearer {}", a.secret));
                let device = who
                    .as_ref()
                    .map_or("dev-anon".to_string(), |a| a.device.clone());
                let mut headers = vec![("x-device-id", device.as_str())];
                if let Some(auth) = auth.as_deref() {
                    headers.push(("authorization", auth));
                }
                let (status, _, json) = app
                    .request("POST", "/v1/captures", &headers, Some(body))
                    .await;
                (status, json)
            }
        };
        assert_eq!(call(None, body.clone()).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(
            call(Some(&stranger), body.clone()).await.0,
            StatusCode::FORBIDDEN
        );
        let (status, answer) = call(
            Some(&admin),
            json!({ "purpose": "revalidation", "shelter_id": shelter }),
        )
        .await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("capture_target_required")
            )
        );
        let (status, answer) = call(Some(&admin), body).await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        let stored: CaptureDoc = store::get(
            &app.state.db,
            CAPTURES,
            answer["capture_id"].as_str().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(stored.purpose, Purpose::Revalidation);
        assert_eq!(stored.volunteer_id.as_deref(), Some(admin.id.as_str()));
        assert_eq!(stored.shelter_id.as_deref(), Some(shelter.as_str()));
        assert_eq!(stored.expected_person_id.as_deref(), Some("g1"));
        // Purposes of other owners stay closed here.
        let (status, _) = call(
            Some(&admin),
            json!({ "purpose": "bogus_", "shelter_id": new_id() }),
        )
        .await;
        assert!(status.is_client_error());
    }
}
