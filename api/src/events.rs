//! Automatic events (spec 4.3): a shelter joins the active event within `EVENT_RADIUS_KM`, or
//! starts one. `PATCH /v1/events/{id}` renames it and sets the reference contacts (spec 8.2).

use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use firestore::jiff::tz::TimeZone;
use firestore::{FirestoreDb, FirestoreInstant};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::{self, AuditEntry};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::geo;
use crate::shelters::{self, Address, ShelterDoc};
use crate::store::{self, tx_replace};
use crate::volunteers::GlobalRole;

pub const COLLECTION: &str = "events";
const MAX_NAME_CHARS: usize = 100;
const MAX_PHONE_CHARS: usize = 30;
/// `last_activity_at` is written at most this often per event (spec 4.3.3).
const ACTIVITY_INTERVAL: Duration = Duration::from_secs(3600);

#[allow(dead_code)] // `purged` is written by the discard of phase 6
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    Active,
    Closed,
    Purged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosureReason {
    AllSheltersClosed,
    Inactivity,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceContacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub civil_defense_phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_protection_phone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDoc {
    pub name: String,
    pub auto_named: bool,
    pub status: EventStatus,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub last_activity_at: FirestoreInstant,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub closed_at: Option<FirestoreInstant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closure_reason: Option<ClosureReason>,
    /// When the event's data is discarded: `closed_at` + `PURGE_AFTER_DAYS` (spec 4.3, phase 6 does the discard).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub purge_at: Option<FirestoreInstant>,
    #[serde(default)]
    pub reference_contacts: ReferenceContacts,
}

/// "City/ST – MM/YYYY"; without an address yet, "Evento – MM/YYYY" (spec 4.3.1).
pub fn auto_name(city: Option<&str>, state: Option<&str>, at: FirestoreInstant) -> String {
    let zoned = at.to_zoned(TimeZone::UTC);
    let when = format!("{:02}/{}", zoned.month(), zoned.year());
    match (city, state) {
        (Some(city), Some(state)) => format!("{city}/{state} – {when}"),
        (Some(city), None) => format!("{city} – {when}"),
        _ => format!("Evento – {when}"),
    }
}

impl EventDoc {
    fn new(address: &Address, now: FirestoreInstant) -> Self {
        Self {
            name: auto_name(address.city.as_deref(), address.state.as_deref(), now),
            auto_named: true,
            status: EventStatus::Active,
            created_at: now,
            last_activity_at: now,
            closed_at: None,
            closure_reason: None,
            purge_at: None,
            reference_contacts: ReferenceContacts::default(),
        }
    }

    /// The name the event got before any shelter had an address, which a later address replaces.
    pub fn has_placeholder_name(&self) -> bool {
        self.auto_named && self.name == auto_name(None, None, self.created_at)
    }

    /// The name once `address` is known, when it should be renamed (spec 4.3.1).
    pub fn renamed_for(&self, address: &Address) -> Option<String> {
        let city = address.city.as_deref()?;
        self.has_placeholder_name()
            .then(|| auto_name(Some(city), address.state.as_deref(), self.created_at))
    }

    pub fn view(&self, id: &str) -> Value {
        json!({
            "id": id,
            "name": self.name,
            "auto_named": self.auto_named,
            "status": self.status,
            "reference_contacts": self.reference_contacts,
        })
    }
}

/// Where a new shelter goes (spec 4.3.1).
pub struct Assignment {
    pub event_id: String,
    /// Set when no active event was in range and a new one has to be written.
    pub new_event: Option<EventDoc>,
    /// Every active event in range, nearest shelter's first; more than one means an overlap.
    pub in_range: Vec<String>,
}

/// Looks for open shelters of active events within `radius_km` of the point (reads through
/// `db`, so inside a transaction they belong to it). Zero events: a new one. One: join it.
/// Two or more: join the event of the nearest shelter and report the overlap, without merging.
pub async fn assign(
    db: &FirestoreDb,
    lat: f64,
    lon: f64,
    address: &Address,
    radius_km: u32,
    now: FirestoreInstant,
) -> Result<Assignment, ApiError> {
    let mut in_range: Vec<String> = Vec::new();
    for near in geo::open_shelters_within(db, lat, lon, f64::from(radius_km) * 1000.0).await? {
        let Some(event_id) = near.shelter.event_id else {
            continue;
        };
        if in_range.contains(&event_id) {
            continue;
        }
        let event: Option<EventDoc> = store::get(db, COLLECTION, &event_id).await?;
        if event.is_some_and(|e| e.status == EventStatus::Active) {
            in_range.push(event_id);
        }
    }
    Ok(match in_range.first() {
        Some(nearest) => Assignment {
            event_id: nearest.clone(),
            new_event: None,
            in_range,
        },
        None => Assignment {
            event_id: uuid::Uuid::new_v4().to_string(),
            new_event: Some(EventDoc::new(address, now)),
            in_range,
        },
    })
}

/// Gate for the writes of a shelter (spec 4.3 item 5): an event that is closed takes no writes, so
/// the shelters of a closed event answer `409 event_closed`. Every write route that names a
/// shelter calls it first; inside a transaction, pass the transaction's `db` so the read conflicts
/// with the task that closes the event. An event that is missing does not block.
pub async fn ensure_open(db: &FirestoreDb, event_id: Option<&str>) -> Result<(), ApiError> {
    let Some(event_id) = event_id else {
        return Ok(());
    };
    let event: Option<EventDoc> = store::get(db, COLLECTION, event_id).await?;
    match event {
        Some(event) if event.status != EventStatus::Active => Err(ApiError::event_closed()),
        _ => Ok(()),
    }
}

/// `ensure_open` for the event of a shelter; a shelter that is missing does not block.
pub async fn ensure_shelter_open(db: &FirestoreDb, shelter_id: &str) -> Result<(), ApiError> {
    if shelter_id.is_empty() {
        return Ok(());
    }
    let shelter: Option<ShelterDoc> = store::get(db, shelters::COLLECTION, shelter_id).await?;
    ensure_open(db, shelter.and_then(|s| s.event_id).as_deref()).await
}

/// Whether the event's activity should be written now, and if so, remembers it.
// ponytail: the "written within the hour" memory is per process (single e2-micro instance), and is
// lost on restart, which costs one extra write per event. With more than one instance, each one
// would write once an hour; read `last_activity_at` from the event document to share the limit.
fn activity_due(state: &AppState, event_id: &str, now: Instant) -> bool {
    let mut seen = state
        .event_activity
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match seen.get(event_id) {
        Some(last) if now.duration_since(*last) < ACTIVITY_INTERVAL => false,
        _ => {
            seen.insert(event_id.into(), now);
            true
        }
    }
}

/// Records activity on the event after a write to one of its shelters, at most once an hour
/// per event (spec 4.3.3). A failure is logged: the write it follows already succeeded.
pub async fn touch(state: &AppState, event_id: Option<&str>) {
    let Some(event_id) = event_id else { return };
    if !activity_due(state, event_id, Instant::now()) {
        return;
    }
    #[derive(Serialize, Deserialize)]
    struct Activity {
        #[serde(with = "firestore::serialize_as_timestamp")]
        last_activity_at: FirestoreInstant,
    }
    let written = state
        .db
        .fluent()
        .update()
        .fields(["last_activity_at"])
        .in_col(COLLECTION)
        .precondition(firestore::FirestoreWritePrecondition::Exists(true))
        .document_id(event_id)
        .object(&Activity {
            last_activity_at: store::now(),
        })
        .execute::<()>()
        .await;
    if let Err(err) = written {
        tracing::warn!(%err, event_id, "could not record event activity");
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventPatch {
    name: Option<String>,
    #[serde(default)]
    reference_contacts: Option<ContactsPatch>,
}

/// A key left out keeps the phone; `null` or blank clears it.
#[derive(Debug, Clone, Deserialize)]
pub struct ContactsPatch {
    #[serde(default, deserialize_with = "crate::volunteers::present")]
    civil_defense_phone: Option<Option<String>>,
    #[serde(default, deserialize_with = "crate::volunteers::present")]
    child_protection_phone: Option<Option<String>>,
}

fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_event");
    error.detail = json!({ "field": field });
    error
}

fn phone(value: &Option<String>, field: &str) -> Result<Option<String>, ApiError> {
    let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > MAX_PHONE_CHARS {
        return Err(invalid(field));
    }
    Ok(Some(value.into()))
}

/// Applies the patch to the event; the caller has already checked who may do each part.
fn apply(event: &mut EventDoc, patch: &EventPatch) -> Result<(), ApiError> {
    if let Some(name) = &patch.name {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
            return Err(invalid("name"));
        }
        event.name = name.into();
        event.auto_named = false;
    }
    if let Some(contacts) = &patch.reference_contacts {
        if let Some(value) = &contacts.civil_defense_phone {
            event.reference_contacts.civil_defense_phone = phone(value, "civil_defense_phone")?;
        }
        if let Some(value) = &contacts.child_protection_phone {
            event.reference_contacts.child_protection_phone =
                phone(value, "child_protection_phone")?;
        }
    }
    Ok(())
}

fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

pub async fn patch_event(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(event_id): Path<String>,
    Json(patch): Json<EventPatch>,
) -> Result<Json<Value>, ApiError> {
    let civil_defense = volunteer.has_global(GlobalRole::CivilDefense);
    // Contacts: validator or Civil Defense. Name: an admin of a shelter of the event, or Civil Defense.
    if patch.reference_contacts.is_some() && !volunteer.is_validator_or_civil_defense() {
        return Err(forbidden());
    }
    let event = store::transact(&state.db, |db, tx| {
        let (event_id, volunteer) = (event_id.clone(), volunteer.clone());
        let patch = patch.clone();
        Box::pin(async move {
            let mut event: EventDoc = store::get(&db, COLLECTION, &event_id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "event_not_found"))?;
            if patch.name.is_some()
                && !civil_defense
                && !volunteer.memberships.iter().any(|m| {
                    m.event_id.as_deref() == Some(event_id.as_str())
                        && m.role == crate::members::ShelterRole::Admin
                })
            {
                return Err(forbidden());
            }
            apply(&mut event, &patch)?;
            tx_replace(&db, tx, COLLECTION, &event_id, &event)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new("event.update", "event", &event_id).by(&volunteer.id),
            )?;
            Ok(event)
        })
    })
    .await?;
    Ok(Json(json!({ "event": event.view(&event_id) })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> FirestoreInstant {
        rfc3339.parse().unwrap()
    }

    #[test]
    fn automatic_names_follow_city_state_and_month() {
        let when = at("2026-05-09T12:00:00Z");
        assert_eq!(
            auto_name(Some("Porto Alegre"), Some("RS"), when),
            "Porto Alegre/RS – 05/2026"
        );
        assert_eq!(auto_name(Some("Canoas"), None, when), "Canoas – 05/2026");
        assert_eq!(auto_name(None, Some("RS"), when), "Evento – 05/2026");
        assert_eq!(
            auto_name(None, None, at("2026-12-31T23:59:59Z")),
            "Evento – 12/2026"
        );
    }

    fn event(name: &str, auto_named: bool) -> EventDoc {
        EventDoc {
            name: name.into(),
            auto_named,
            status: EventStatus::Active,
            created_at: at("2026-05-09T12:00:00Z"),
            last_activity_at: at("2026-05-09T12:00:00Z"),
            closed_at: None,
            closure_reason: None,
            purge_at: None,
            reference_contacts: ReferenceContacts::default(),
        }
    }

    fn address(city: Option<&str>, state: Option<&str>) -> Address {
        Address {
            text: None,
            city: city.map(Into::into),
            state: state.map(Into::into),
            pending: false,
        }
    }

    #[test]
    fn only_a_placeholder_name_is_redone_when_the_address_arrives() {
        let placeholder = event("Evento – 05/2026", true);
        assert_eq!(
            placeholder
                .renamed_for(&address(Some("Canoas"), Some("RS")))
                .as_deref(),
            Some("Canoas/RS – 05/2026")
        );
        // An address without a city cannot name it yet.
        assert_eq!(placeholder.renamed_for(&address(None, Some("RS"))), None);
        // Already named after a city, or renamed by a person: left alone.
        assert_eq!(
            event("Canoas/RS – 05/2026", true)
                .renamed_for(&address(Some("Porto Alegre"), Some("RS"))),
            None
        );
        assert_eq!(
            event("Evento – 05/2026", false).renamed_for(&address(Some("Canoas"), None)),
            None
        );
    }

    #[test]
    fn rename_validation_and_contacts() {
        let mut e = event("Evento – 05/2026", true);
        let patch = |value: Value| serde_json::from_value::<EventPatch>(value).unwrap();
        assert!(apply(&mut e, &patch(json!({ "name": "  " }))).is_err());
        assert!(apply(&mut e, &patch(json!({ "name": "x".repeat(101) }))).is_err());
        apply(&mut e, &patch(json!({ "name": " Cheia do Guaíba " }))).unwrap();
        assert_eq!((e.name.as_str(), e.auto_named), ("Cheia do Guaíba", false));

        apply(&mut e, &patch(json!({ "reference_contacts": { "civil_defense_phone": "199", "child_protection_phone": "100" } }))).unwrap();
        assert_eq!(
            e.reference_contacts.civil_defense_phone.as_deref(),
            Some("199")
        );
        // A key left out keeps its phone; null clears; too long is refused.
        apply(
            &mut e,
            &patch(json!({ "reference_contacts": { "child_protection_phone": null } })),
        )
        .unwrap();
        assert_eq!(
            e.reference_contacts,
            ReferenceContacts {
                civil_defense_phone: Some("199".into()),
                child_protection_phone: None
            }
        );
        assert!(
            apply(
                &mut e,
                &patch(json!({ "reference_contacts": { "civil_defense_phone": "1".repeat(31) } }))
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn activity_is_written_at_most_once_an_hour_per_event() {
        let app = crate::test_support::TestApp::new().await;
        let t0 = Instant::now();
        assert!(activity_due(&app.state, "e1", t0));
        assert!(!activity_due(
            &app.state,
            "e1",
            t0 + Duration::from_secs(10)
        ));
        assert!(!activity_due(
            &app.state,
            "e1",
            t0 + Duration::from_secs(3599)
        ));
        assert!(
            activity_due(&app.state, "e2", t0 + Duration::from_secs(10)),
            "another event"
        );
        assert!(activity_due(
            &app.state,
            "e1",
            t0 + Duration::from_secs(3600)
        ));
        assert!(!activity_due(
            &app.state,
            "e1",
            t0 + Duration::from_secs(3601)
        ));
    }
}

#[cfg(test)]
mod patch_tests {
    use super::*;
    use crate::test_support::{ORIGIN, TestApp, audit_actions, new_id, north_of_origin};

    async fn event_of(app: &TestApp, shelter: &str) -> String {
        let doc: crate::shelters::ShelterDoc =
            store::get(&app.state.db, crate::shelters::COLLECTION, shelter)
                .await
                .unwrap()
                .unwrap();
        doc.event_id.unwrap()
    }

    #[tokio::test]
    async fn the_name_is_changed_by_an_admin_of_the_event_or_the_civil_defense_only() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.create_shelter(&admin, "A", ORIGIN).await;
        let event = event_of(&app, &shelter).await;
        let volunteer = app
            .join(
                &app.invite(&admin, &shelter, "qr", "volunteer").await,
                "Vol",
            )
            .await;
        // An admin of a shelter in another event.
        let far_admin = app.actor("Far").await;
        app.create_shelter(&far_admin, "Longe", north_of_origin(200_000.0))
            .await;
        let validator = app.actor("Val").await;
        app.grant(&validator, GlobalRole::Validator).await;
        let civil_defense = app.actor("DC").await;
        app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let path = format!("/v1/events/{event}");
        let rename = |name: &str| Some(json!({ "name": name }));

        for who in [&volunteer, &far_admin, &validator] {
            assert_eq!(
                app.send("PATCH", &path, &who.secret, rename("Nova"))
                    .await
                    .0,
                StatusCode::FORBIDDEN,
                "{}",
                who.id
            );
        }
        let (status, body) = app
            .send("PATCH", &path, &admin.secret, rename("Cheia de outubro"))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["event"],
            json!({ "id": event, "name": "Cheia de outubro", "auto_named": false, "status": "active", "reference_contacts": {} })
        );
        assert_eq!(
            app.send("PATCH", &path, &civil_defense.secret, rename("Cheia 2026"))
                .await
                .0,
            StatusCode::OK
        );
        assert!(
            audit_actions(&app.state.db)
                .await
                .contains(&"event.update".to_string())
        );
        assert_eq!(
            app.request("PATCH", &path, &[], rename("x")).await.0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn the_reference_contacts_are_set_by_a_validator_or_the_civil_defense_only() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.create_shelter(&admin, "A", ORIGIN).await;
        let event = event_of(&app, &shelter).await;
        let path = format!("/v1/events/{event}");
        let contacts = Some(
            json!({ "reference_contacts": { "civil_defense_phone": "199", "child_protection_phone": "100" } }),
        );
        // Not even the admin of the event's shelter.
        assert_eq!(
            app.send("PATCH", &path, &admin.secret, contacts.clone())
                .await
                .0,
            StatusCode::FORBIDDEN
        );

        let validator = app.actor("Val").await;
        app.grant(&validator, GlobalRole::Validator).await;
        let (status, body) = app.send("PATCH", &path, &validator.secret, contacts).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["event"]["reference_contacts"],
            json!({ "civil_defense_phone": "199", "child_protection_phone": "100" })
        );
        // The validator is no admin, so the name stays out of reach, even in the same request.
        let (status, _) = app
            .send(
                "PATCH",
                &path,
                &validator.secret,
                Some(
                    json!({ "name": "x", "reference_contacts": { "civil_defense_phone": "190" } }),
                ),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let stored: EventDoc = store::get(&app.state.db, COLLECTION, &event)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.reference_contacts.civil_defense_phone.as_deref(),
            Some("199"),
            "a refused request changes nothing"
        );
        // The shelter's own view carries the contacts for the child alerts of phase 3.
        let (_, seen) = app
            .send(
                "GET",
                &format!("/v1/shelters/{shelter}"),
                &admin.secret,
                None,
            )
            .await;
        assert_eq!(
            seen["shelter"]["event"]["reference_contacts"]["child_protection_phone"],
            "100"
        );
    }

    #[tokio::test]
    async fn invalid_values_and_unknown_events_are_refused() {
        let app = TestApp::new().await;
        let civil_defense = app.actor("DC").await;
        app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let admin = app.actor("Admin").await;
        let event = event_of(&app, &app.create_shelter(&admin, "A", ORIGIN).await).await;
        for bad in [
            json!({ "name": "  " }),
            json!({ "name": "x".repeat(101) }),
            json!({ "reference_contacts": { "civil_defense_phone": "1".repeat(31) } }),
        ] {
            let (status, body) = app
                .send(
                    "PATCH",
                    &format!("/v1/events/{event}"),
                    &civil_defense.secret,
                    Some(bad),
                )
                .await;
            assert_eq!(
                (status, body["error"].as_str()),
                (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_event"))
            );
        }
        let (status, body) = app
            .send(
                "PATCH",
                &format!("/v1/events/{}", new_id()),
                &civil_defense.secret,
                Some(json!({ "name": "x" })),
            )
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::NOT_FOUND, Some("event_not_found"))
        );
    }

    #[tokio::test]
    async fn the_activity_write_goes_through_the_api_without_touching_the_name() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.create_shelter(&admin, "A", ORIGIN).await;
        let event = event_of(&app, &shelter).await;
        let civil_defense = app.actor("DC").await;
        app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        app.send(
            "PATCH",
            &format!("/v1/events/{event}"),
            &civil_defense.secret,
            Some(
                json!({ "name": "Cheia", "reference_contacts": { "civil_defense_phone": "199" } }),
            ),
        )
        .await;
        app.state.event_activity.lock().unwrap().clear();
        touch(&app.state, Some(&event)).await;
        let stored: EventDoc = store::get(&app.state.db, COLLECTION, &event)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                stored.name.as_str(),
                stored.auto_named,
                stored.reference_contacts.civil_defense_phone.as_deref()
            ),
            ("Cheia", false, Some("199"))
        );
        // An event that does not exist is not created by the activity write.
        touch(&app.state, Some("ghost")).await;
        assert!(
            store::get::<EventDoc>(&app.state.db, COLLECTION, "ghost")
                .await
                .unwrap()
                .is_none()
        );
        touch(&app.state, None).await;
    }

    /// Closes the event of `shelter_id` the way the hourly task does.
    async fn close_event_of(app: &crate::test_support::TestApp, shelter_id: &str) -> String {
        let shelter: ShelterDoc = store::get(&app.state.db, shelters::COLLECTION, shelter_id)
            .await
            .unwrap()
            .unwrap();
        let event_id = shelter.event_id.unwrap();
        let mut event: EventDoc = store::get(&app.state.db, COLLECTION, &event_id)
            .await
            .unwrap()
            .unwrap();
        event.status = EventStatus::Closed;
        event.closed_at = Some(store::now());
        store::replace(&app.state.db, COLLECTION, &event_id, &event)
            .await
            .unwrap();
        event_id
    }

    #[tokio::test]
    async fn every_write_of_a_shelter_of_a_closed_event_is_409_and_reads_still_work() {
        use crate::exits::fixtures::{child, person, put};
        use crate::people::{ChildStatus, Level};
        use crate::test_support::{ORIGIN, TestApp, new_id};

        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.create_shelter(&admin, "A", ORIGIN).await;

        let (adult, kid, guardian) = (new_id(), new_id(), new_id());
        put(&app, &adult, &person(Level::Assisted, Some(&shelter))).await;
        put(&app, &guardian, &person(Level::Verified, Some(&shelter))).await;
        put(
            &app,
            &kid,
            &child(&shelter, ChildStatus::Unaccompanied, vec![]),
        )
        .await;
        let household = new_id();
        let (status, body) = app
            .send(
                "POST",
                &format!("/v1/shelters/{shelter}/households"),
                &admin.secret,
                Some(json!({ "id": household, "member_ids": [adult], "client_created_at": "2026-10-21T10:00:00Z" })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let card = "ABCD2345";
        let issue = json!({ "code": card, "client_issued_at": "2026-10-21T10:00:00Z" });
        let (status, body) = app
            .send(
                "POST",
                &format!("/v1/households/{household}/cards"),
                &admin.secret,
                Some(issue),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");

        close_event_of(&app, &shelter).await;

        let at = "2026-10-21T11:00:00Z";
        let writes = [
            (
                "POST",
                format!("/v1/shelters/{shelter}/checkins"),
                json!({ "id": new_id(), "legal_basis": "consent", "client_created_at": at, "data": {}, "no_biometrics": true }),
            ),
            (
                "PATCH",
                format!("/v1/people/{adult}"),
                json!({ "data": {}, "client_edited_at": at }),
            ),
            (
                "PUT",
                format!("/v1/people/{adult}/biometrics"),
                json!({ "photo_base64": "AAAA" }),
            ),
            (
                "POST",
                format!("/v1/people/{kid}/guardians"),
                json!({ "guardian_id": guardian, "link": "declared" }),
            ),
            (
                "POST",
                format!("/v1/shelters/{shelter}/exits"),
                json!({ "person_id": adult, "exit_destination": "own_home" }),
            ),
            (
                "POST",
                format!("/v1/shelters/{shelter}/households"),
                json!({ "id": new_id(), "member_ids": [guardian], "client_created_at": at }),
            ),
            (
                "PATCH",
                format!("/v1/households/{household}"),
                json!({ "add_member_ids": [guardian] }),
            ),
            (
                "POST",
                format!("/v1/households/{household}/cards"),
                json!({ "code": "EFGH6789", "client_issued_at": at }),
            ),
            ("POST", format!("/v1/cards/{card}/cancel"), json!({})),
        ];
        for (method, path, body) in writes {
            let (status, answer) = app.send(method, &path, &admin.secret, Some(body)).await;
            assert_eq!(
                (status, answer["error"].as_str()),
                (StatusCode::CONFLICT, Some("event_closed")),
                "{method} {path}"
            );
        }
        let (status, _) = app
            .send("GET", &format!("/v1/people/{adult}"), &admin.secret, None)
            .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = app
            .send(
                "GET",
                &format!("/v1/households/{household}"),
                &admin.secret,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn ensure_open_lets_active_missing_and_absent_events_through() {
        let db = crate::test_support::test_db().await;
        let mut doc = EventDoc::new(
            &Address {
                text: None,
                city: None,
                state: None,
                pending: false,
            },
            store::now(),
        );
        store::replace(&db, COLLECTION, "open", &doc).await.unwrap();
        doc.status = EventStatus::Closed;
        store::replace(&db, COLLECTION, "shut", &doc).await.unwrap();
        assert!(ensure_open(&db, None).await.is_ok());
        assert!(ensure_open(&db, Some("open")).await.is_ok());
        assert!(ensure_open(&db, Some("ghost")).await.is_ok());
        let err = ensure_open(&db, Some("shut")).await.unwrap_err();
        assert_eq!(
            (err.status, err.code),
            (StatusCode::CONFLICT, "event_closed")
        );
    }
}
