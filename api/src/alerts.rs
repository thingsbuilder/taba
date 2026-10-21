//! Alerts (spec 7, 8.8): `GET /v1/alerts` and `POST /v1/alerts/{id}/resolve`.
//! Phase 2 raises `shelter_flagged`, `event_overlap`, `volunteer_removed` and `endorser_flagged`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use firestore::{FirestoreDb, FirestoreInstant, FirestoreTransaction, path};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::{self, AuditEntry};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::store::{self, fs_error, tx_create, tx_replace};
use crate::volunteers::GlobalRole;

pub const COLLECTION: &str = "alerts";
/// A page of one listing; recipients are filtered in memory.
// ponytail: alerts are filtered per recipient after the query. Fine for a pilot's volume; add a
// `shelter_ids` array-contains query per admin shelter if the open alerts ever pass a few hundred.
const LIST_LIMIT: u32 = 200;

#[allow(dead_code)] // the phase 3 kinds are raised by the modules of their owners
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    ShelterFlagged,
    EventOverlap,
    VolunteerRemoved,
    EndorserFlagged,
    ChildSeparated,
    RevalidationFailed,
    ExceptionalExit,
    HouseholdConflict,
    UnicoLimit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    pub kind: AlertKind,
    pub informational: bool,
    pub target_kind: String,
    pub target_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default)]
    pub shelter_ids: Vec<String>,
    #[serde(default)]
    pub volunteer_ids: Vec<String>,
    pub for_civil_defense: bool,
    pub for_protection: bool,
    pub detail: Value,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    pub resolved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
}

impl AlertDoc {
    /// An open alert for the Civil Defense about one thing; add shelters to also reach their admins.
    pub fn new(kind: AlertKind, target_kind: &str, target_id: &str, detail: Value) -> Self {
        Self {
            id: None,
            kind,
            informational: false,
            target_kind: target_kind.into(),
            target_id: target_id.into(),
            event_id: None,
            shelter_ids: vec![],
            volunteer_ids: vec![],
            for_civil_defense: true,
            for_protection: false,
            detail,
            created_at: store::now(),
            resolved: false,
            resolved_by: None,
        }
    }

    pub fn for_shelters(mut self, shelter_ids: Vec<String>) -> Self {
        self.shelter_ids = shelter_ids;
        self
    }

    pub fn about_volunteers(mut self, volunteer_ids: Vec<String>) -> Self {
        self.volunteer_ids = volunteer_ids;
        self
    }

    pub fn in_event(mut self, event_id: &str) -> Self {
        self.event_id = Some(event_id.into());
        self
    }
}

/// Adds the alert to a transaction, so it commits together with the change that raised it.
pub fn add(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    alert: &AlertDoc,
) -> Result<(), ApiError> {
    tx_create(db, tx, COLLECTION, &uuid::Uuid::new_v4().to_string(), alert)
}

/// Who receives an alert (spec 8.8): the Civil Defense gets all of them; admins get the
/// non-informational ones of their shelters; `protection` gets the protection ones.
/// `endorser_flagged` goes to the Civil Defense and the validators, never to the shelter admins,
/// and never to the endorser it is about.
pub fn is_recipient(volunteer: &Volunteer, alert: &AlertDoc) -> bool {
    if alert.kind == AlertKind::EndorserFlagged {
        return volunteer.is_validator_or_civil_defense()
            && !alert.volunteer_ids.contains(&volunteer.id);
    }
    volunteer.has_global(GlobalRole::CivilDefense)
        || (alert.for_protection && volunteer.has_global(GlobalRole::Protection))
        || (!alert.informational
            && alert
                .shelter_ids
                .iter()
                .any(|shelter| volunteer.is_admin_of(shelter)))
}

fn view(alert: &AlertDoc) -> Value {
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

fn can_receive_any(volunteer: &Volunteer) -> bool {
    volunteer.is_validator_or_civil_defense()
        || volunteer.has_global(GlobalRole::Protection)
        || volunteer.has_admin_membership()
}

async fn open_alerts(db: &FirestoreDb, informational: bool) -> Result<Vec<AlertDoc>, ApiError> {
    db.fluent()
        .select()
        .from(COLLECTION)
        .filter(|q| {
            q.for_all([
                q.field(path!(AlertDoc::resolved)).eq(false),
                q.field(path!(AlertDoc::informational)).eq(informational),
            ])
        })
        .order_by([(
            path!(AlertDoc::created_at),
            firestore::FirestoreQueryDirection::Descending,
        )])
        .limit(LIST_LIMIT)
        .obj()
        .query()
        .await
        .map_err(fs_error)
}

pub async fn get_alerts(
    State(state): State<AppState>,
    volunteer: Volunteer,
) -> Result<Json<Value>, ApiError> {
    if !can_receive_any(&volunteer) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden"));
    }
    let mut alerts = open_alerts(&state.db, false).await?;
    if volunteer.has_global(GlobalRole::CivilDefense) {
        alerts.extend(open_alerts(&state.db, true).await?);
        alerts.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    }
    let visible: Vec<Value> = alerts
        .iter()
        .filter(|alert| is_recipient(&volunteer, alert))
        .map(view)
        .collect();
    Ok(Json(json!({ "alerts": visible })))
}

pub async fn resolve_alert(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(alert_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    store::transact(&state.db, |db, tx| {
        let (alert_id, volunteer) = (alert_id.clone(), volunteer.clone());
        Box::pin(async move {
            let mut alert: AlertDoc = store::get(&db, COLLECTION, &alert_id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "alert_not_found"))?;
            if !is_recipient(&volunteer, &alert) {
                return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden"));
            }
            if alert.resolved {
                return Ok(());
            }
            alert.resolved = true;
            alert.resolved_by = Some(volunteer.id.clone());
            tx_replace(&db, tx, COLLECTION, &alert_id, &alert)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new("alert.resolve", "alert", &alert_id).by(&volunteer.id),
            )
        })
    })
    .await?;
    Ok(Json(json!({})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, audit_actions, north_of_origin};

    fn error_of(body: &Value) -> Option<&str> {
        body["error"].as_str()
    }

    async fn seed(app: &TestApp, alert: AlertDoc) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        store::replace(&app.state.db, COLLECTION, &id, &alert)
            .await
            .unwrap();
        id
    }

    fn alert_for(shelter_ids: &[&str]) -> AlertDoc {
        AlertDoc::new(
            AlertKind::VolunteerRemoved,
            "volunteer",
            "someone",
            json!({}),
        )
        .for_shelters(shelter_ids.iter().map(|s| s.to_string()).collect())
    }

    async fn listed(app: &TestApp, who: &Actor) -> Vec<String> {
        let (status, body) = app.send("GET", "/v1/alerts", &who.secret, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["id"].as_str().unwrap().to_string())
            .collect()
    }

    struct World {
        app: TestApp,
        admin_a: Actor,
        shelter_a: String,
        admin_b: Actor,
        shelter_b: String,
        volunteer_a: Actor,
        civil_defense: Actor,
    }

    async fn world() -> World {
        let app = TestApp::new().await;
        let admin_a = app.actor("AdminA").await;
        let shelter_a = app.create_shelter(&admin_a, "A", ORIGIN).await;
        let admin_b = app.actor("AdminB").await;
        let shelter_b = app
            .create_shelter(&admin_b, "B", north_of_origin(5_000.0))
            .await;
        let volunteer_a = app
            .join(
                &app.invite(&admin_a, &shelter_a, "qr", "volunteer").await,
                "VolA",
            )
            .await;
        let civil_defense = app.actor("DC").await;
        app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        World {
            app,
            admin_a,
            shelter_a,
            admin_b,
            shelter_b,
            volunteer_a,
            civil_defense,
        }
    }

    #[tokio::test]
    async fn each_recipient_sees_their_own_alerts() {
        let w = world().await;
        let for_a = seed(&w.app, alert_for(&[&w.shelter_a])).await;
        let for_b = seed(&w.app, alert_for(&[&w.shelter_b])).await;
        let for_both = seed(&w.app, alert_for(&[&w.shelter_a, &w.shelter_b])).await;
        let mut informational = alert_for(&[&w.shelter_a]);
        informational.informational = true;
        let quiet = seed(&w.app, informational).await;
        let mut protection = alert_for(&[]);
        protection.for_protection = true;
        let for_protection = seed(&w.app, protection).await;

        let sorted = |mut ids: Vec<String>| {
            ids.sort();
            ids
        };
        assert_eq!(
            sorted(listed(&w.app, &w.admin_a).await),
            sorted(vec![for_a.clone(), for_both.clone()])
        );
        assert_eq!(
            sorted(listed(&w.app, &w.admin_b).await),
            sorted(vec![for_b.clone(), for_both.clone()])
        );
        // The Civil Defense gets everything, informational ones included.
        assert_eq!(
            sorted(listed(&w.app, &w.civil_defense).await),
            sorted(vec![for_a, for_b, for_both, quiet, for_protection.clone()])
        );
        // `protection` gets the ones for it.
        let protection_officer = w.app.actor("Prot").await;
        w.app
            .grant(&protection_officer, GlobalRole::Protection)
            .await;
        assert_eq!(listed(&w.app, &protection_officer).await, [for_protection]);
    }

    #[tokio::test]
    async fn volunteers_and_strangers_have_no_alert_box() {
        let w = world().await;
        seed(&w.app, alert_for(&[&w.shelter_a])).await;
        let validator = w.app.actor("Val").await;
        w.app.grant(&validator, GlobalRole::Validator).await;
        let stranger = w.app.actor("Ze").await;
        // A validator reads alerts, but of `endorser_flagged` only (checked in the endorsement tests).
        let (status, body) = w
            .app
            .send("GET", "/v1/alerts", &validator.secret, None)
            .await;
        assert_eq!(
            (status, body["alerts"].as_array().unwrap().len()),
            (StatusCode::OK, 0)
        );
        for who in [&w.volunteer_a, &stranger] {
            let (status, body) = w.app.send("GET", "/v1/alerts", &who.secret, None).await;
            assert_eq!(
                (status, error_of(&body)),
                (StatusCode::FORBIDDEN, Some("forbidden")),
                "{}",
                who.id
            );
        }
        assert_eq!(
            w.app.request("GET", "/v1/alerts", &[], None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn the_newest_alert_comes_first_and_the_view_has_the_documented_fields() {
        let w = world().await;
        let older = seed(&w.app, alert_for(&[&w.shelter_a])).await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let newer = seed(&w.app, alert_for(&[&w.shelter_a]).in_event("event-1")).await;
        assert_eq!(listed(&w.app, &w.admin_a).await, [newer.clone(), older]);
        let (_, body) = w
            .app
            .send("GET", "/v1/alerts", &w.admin_a.secret, None)
            .await;
        let first = &body["alerts"][0];
        assert_eq!(
            (
                &first["kind"],
                &first["event_id"],
                &first["target_kind"],
                &first["informational"],
                &first["resolved"],
                &first["shelter_ids"],
                &first["volunteer_ids"]
            ),
            (
                &json!("volunteer_removed"),
                &json!("event-1"),
                &json!("volunteer"),
                &json!(false),
                &json!(false),
                &json!([w.shelter_a]),
                &json!([])
            )
        );
        assert!(first["created_at"].is_string() && first["detail"].is_object());
    }

    #[tokio::test]
    async fn a_recipient_resolves_an_alert_and_it_leaves_everyone_s_list() {
        let w = world().await;
        let both = seed(&w.app, alert_for(&[&w.shelter_a, &w.shelter_b])).await;
        let path = format!("/v1/alerts/{both}/resolve");
        let (status, body) = w.app.send("POST", &path, &w.admin_a.secret, None).await;
        assert_eq!((status, body), (StatusCode::OK, json!({})));
        for who in [&w.admin_a, &w.admin_b, &w.civil_defense] {
            assert!(listed(&w.app, who).await.is_empty(), "{}", who.id);
        }
        let stored: AlertDoc = store::get(&w.app.state.db, COLLECTION, &both)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (stored.resolved, stored.resolved_by.as_deref()),
            (true, Some(w.admin_a.id.as_str()))
        );
        assert!(
            audit_actions(&w.app.state.db)
                .await
                .contains(&"alert.resolve".to_string())
        );
        // Resolving it again is fine and keeps who did it first.
        assert_eq!(
            w.app.send("POST", &path, &w.admin_b.secret, None).await.0,
            StatusCode::OK
        );
        let stored: AlertDoc = store::get(&w.app.state.db, COLLECTION, &both)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.resolved_by.as_deref(), Some(w.admin_a.id.as_str()));
        assert_eq!(
            audit_actions(&w.app.state.db)
                .await
                .iter()
                .filter(|a| *a == "alert.resolve")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn only_a_recipient_resolves_and_unknown_alerts_are_404() {
        let w = world().await;
        let for_a = seed(&w.app, alert_for(&[&w.shelter_a])).await;
        let path = format!("/v1/alerts/{for_a}/resolve");
        for who in [&w.admin_b, &w.volunteer_a] {
            let (status, body) = w.app.send("POST", &path, &who.secret, None).await;
            assert_eq!(
                (status, error_of(&body)),
                (StatusCode::FORBIDDEN, Some("forbidden")),
                "{}",
                who.id
            );
        }
        assert_eq!(
            w.app
                .send("POST", &path, &w.civil_defense.secret, None)
                .await
                .0,
            StatusCode::OK
        );
        let (status, body) = w
            .app
            .send(
                "POST",
                "/v1/alerts/nope/resolve",
                &w.civil_defense.secret,
                None,
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::NOT_FOUND, Some("alert_not_found"))
        );
        assert_eq!(
            w.app.request("POST", &path, &[], None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(all_docs(&w.app.state.db, COLLECTION).await.len(), 1);
    }

    #[test]
    fn alerts_serialize_with_the_documented_kind_names() {
        let kinds = [
            (AlertKind::ShelterFlagged, "shelter_flagged"),
            (AlertKind::EventOverlap, "event_overlap"),
            (AlertKind::VolunteerRemoved, "volunteer_removed"),
            (AlertKind::EndorserFlagged, "endorser_flagged"),
        ];
        for (kind, name) in kinds {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(name));
        }
    }
}
