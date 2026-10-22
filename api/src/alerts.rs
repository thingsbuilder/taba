//! Alerts (spec 7, 8.8): `GET /v1/alerts` and `POST /v1/alerts/{id}/resolve`.
//! Phase 2 raises `shelter_flagged`, `event_overlap`, `volunteer_removed` and `endorser_flagged`;
//! phase 4 adds `search_expiring` and `family_search_received`, which only their volunteer sees.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use firestore::{FirestoreDb, FirestoreInstant, FirestoreTransaction, path};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::store::{self, fs_error, tx_create, tx_replace};
use crate::volunteers::GlobalRole;

pub const COLLECTION: &str = "alerts";
/// The most alerts one source returns (see `Source`): its newest `LIST_LIMIT` open ones.
// ponytail: each source is queried for its own recipients, so other recipients' alerts never push
// one out; a single source holding more than 200 open alerts leaves its oldest out of the listing.
// Add a cursor if one shelter, or the Civil Defense, ever keeps more than that open.
const LIST_LIMIT: u32 = 200;

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
    /// Phase 4: an active search without an accepted match when its event closes (spec 3.7 item 9).
    SearchExpiring,
    /// Phase 4: the family sent a search through the volunteer's link (spec 3.7 item 11); informational.
    FamilySearchReceived,
    /// Phase 6, informational (spec 4.10): volume of check-ins or rescues of one volunteer above the limit.
    AnomalyVolume,
    /// Phase 6, informational: the same person registered 50 km apart within an hour.
    AnomalyDistance,
    /// Phase 6, informational: a rescue farther than `EVENT_RADIUS_KM` from every shelter of the volunteer.
    OutOfArea,
}

impl AlertKind {
    /// The snake_case name the kind is stored under; a test keeps it equal to the serde name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ShelterFlagged => "shelter_flagged",
            Self::EventOverlap => "event_overlap",
            Self::VolunteerRemoved => "volunteer_removed",
            Self::EndorserFlagged => "endorser_flagged",
            Self::ChildSeparated => "child_separated",
            Self::RevalidationFailed => "revalidation_failed",
            Self::ExceptionalExit => "exceptional_exit",
            Self::HouseholdConflict => "household_conflict",
            Self::UnicoLimit => "unico_limit",
            Self::SearchExpiring => "search_expiring",
            Self::FamilySearchReceived => "family_search_received",
            Self::AnomalyVolume => "anomaly_volume",
            Self::AnomalyDistance => "anomaly_distance",
            Self::OutOfArea => "out_of_area",
        }
    }

    /// Kept for callers outside this module (audit actions).
    pub fn name(self) -> String {
        self.as_str().to_string()
    }

    /// Kinds that only the volunteers in `volunteer_ids` receive and resolve, not even the Civil
    /// Defense (they concern one volunteer's searches, which nobody else reads).
    fn is_personal(self) -> bool {
        matches!(self, Self::SearchExpiring | Self::FamilySearchReceived)
    }

    /// Informational anomalies (spec 4.10): only validators and the Civil Defense read them, never
    /// the volunteer they are about nor the shelter admins.
    fn is_anomaly(self) -> bool {
        matches!(
            self,
            Self::AnomalyVolume | Self::AnomalyDistance | Self::OutOfArea
        )
    }
}

/// The kinds a validator who is not Civil Defense receives (`is_recipient`).
const VALIDATOR_KINDS: [AlertKind; 4] = [
    AlertKind::EndorserFlagged,
    AlertKind::AnomalyVolume,
    AlertKind::AnomalyDistance,
    AlertKind::OutOfArea,
];

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

    /// An informational alert for one volunteer alone (`search_expiring`, `family_search_received`).
    pub fn personal(
        kind: AlertKind,
        target_kind: &str,
        target_id: &str,
        volunteer_id: &str,
        detail: Value,
    ) -> Self {
        let mut alert = Self::new(kind, target_kind, target_id, detail)
            .about_volunteers(vec![volunteer_id.into()]);
        alert.informational = true;
        alert.for_civil_defense = false;
        alert
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
/// and never to the endorser it is about. `search_expiring` and `family_search_received` go to
/// the volunteers in `volunteer_ids` only.
pub fn is_recipient(volunteer: &Volunteer, alert: &AlertDoc) -> bool {
    if alert.kind.is_personal() {
        return alert.volunteer_ids.contains(&volunteer.id);
    }
    if alert.kind.is_anomaly() {
        return volunteer.is_validator_or_civil_defense()
            && !alert.volunteer_ids.contains(&volunteer.id);
    }
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

/// Anyone who can register a search may get an alert about it: shelter members and mediators too.
fn can_receive_any(volunteer: &Volunteer) -> bool {
    volunteer.is_validator_or_civil_defense()
        || volunteer.has_global(GlobalRole::Protection)
        || volunteer.has_global(GlobalRole::Mediator)
        || !volunteer.memberships.is_empty()
}

/// One way into the alerts a volunteer may receive. Each is a query by recipient, run before the
/// limit, so alerts of other recipients cannot crowd a recipient's own out of the page.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// Everything raised for the Civil Defense.
    CivilDefense,
    /// The kinds only validators and the Civil Defense read.
    ValidatorKinds,
    Protection,
    /// The non-informational alerts of a shelter the volunteer administers.
    Shelter(String),
    /// The alerts that name the volunteer in `volunteer_ids`.
    Personal,
}

impl Source {
    /// Whether the source's query returns `alert` for `volunteer_id`; mirrors `open_from`. Kept
    /// for the reachability test, which cannot run the queries over every combination.
    #[cfg(test)]
    fn admits(&self, alert: &AlertDoc, volunteer_id: &str) -> bool {
        match self {
            Self::CivilDefense => alert.for_civil_defense,
            Self::ValidatorKinds => VALIDATOR_KINDS.contains(&alert.kind),
            Self::Protection => alert.for_protection,
            Self::Shelter(id) => !alert.informational && alert.shelter_ids.contains(id),
            Self::Personal => alert.volunteer_ids.iter().any(|v| v == volunteer_id),
        }
    }
}

fn sources_of(volunteer: &Volunteer) -> Vec<Source> {
    let mut sources = vec![Source::Personal];
    if volunteer.has_global(GlobalRole::CivilDefense) {
        sources.push(Source::CivilDefense);
    } else if volunteer.has_global(GlobalRole::Validator) {
        sources.push(Source::ValidatorKinds);
    }
    if volunteer.has_global(GlobalRole::Protection) {
        sources.push(Source::Protection);
    }
    sources.extend(
        volunteer
            .memberships
            .iter()
            .filter(|m| volunteer.is_admin_of(&m.shelter_id))
            .map(|m| Source::Shelter(m.shelter_id.clone())),
    );
    sources
}

/// The newest `LIST_LIMIT` open alerts of one source.
async fn open_from(
    db: &FirestoreDb,
    source: &Source,
    volunteer_id: &str,
) -> Result<Vec<AlertDoc>, ApiError> {
    db.fluent()
        .select()
        .from(COLLECTION)
        .filter(|q| {
            let open = q.field(path!(AlertDoc::resolved)).eq(false);
            match source {
                Source::CivilDefense => {
                    q.for_all([open, q.field(path!(AlertDoc::for_civil_defense)).eq(true)])
                }
                Source::ValidatorKinds => q.for_all([
                    open,
                    q.field(path!(AlertDoc::kind))
                        .is_in(VALIDATOR_KINDS.map(AlertKind::as_str).to_vec()),
                ]),
                Source::Protection => {
                    q.for_all([open, q.field(path!(AlertDoc::for_protection)).eq(true)])
                }
                Source::Shelter(id) => q.for_all([
                    open,
                    q.field(path!(AlertDoc::informational)).eq(false),
                    q.field(path!(AlertDoc::shelter_ids)).array_contains(id),
                ]),
                Source::Personal => q.for_all([
                    open,
                    q.field(path!(AlertDoc::volunteer_ids))
                        .array_contains(volunteer_id),
                ]),
            }
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

/// The open alerts `volunteer` receives: the union of their sources, without repeats, newest first
/// (ties by id), then the final `is_recipient` check.
fn arrange(mut alerts: Vec<AlertDoc>, volunteer: &Volunteer) -> Vec<AlertDoc> {
    alerts.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    alerts.dedup_by(|a, b| a.id == b.id);
    alerts.retain(|alert| is_recipient(volunteer, alert));
    alerts
}

pub async fn get_alerts(
    State(state): State<AppState>,
    volunteer: Volunteer,
) -> Result<Json<Value>, ApiError> {
    if !can_receive_any(&volunteer) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden"));
    }
    let mut found = vec![];
    for source in sources_of(&volunteer) {
        found.extend(open_from(&state.db, &source, &volunteer.id).await?);
    }
    let visible: Vec<Value> = arrange(found, &volunteer).iter().map(view).collect();
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
                &AuditEntry::new("alert.resolve", "alert", &alert_id)
                    .basis(LegalBasis::PublicPolicy)
                    .by(&volunteer.id),
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
        // A member of a shelter may own searches, so the box opens for them, empty of shelter alerts.
        assert!(listed(&w.app, &w.volunteer_a).await.is_empty());
        let (status, body) = w
            .app
            .send("GET", "/v1/alerts", &stranger.secret, None)
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::FORBIDDEN, Some("forbidden"))
        );
        assert_eq!(
            w.app.request("GET", "/v1/alerts", &[], None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }

    fn personal(kind: AlertKind, volunteer: &Actor) -> AlertDoc {
        AlertDoc::personal(
            kind,
            "search",
            "search-1",
            &volunteer.id,
            json!({ "purge_at": "2026-12-01T00:00:00Z" }),
        )
    }

    #[tokio::test]
    async fn search_alerts_reach_only_the_volunteer_they_name() {
        let w = world().await;
        let mediator = w.app.actor("Med").await;
        w.app.grant(&mediator, GlobalRole::Mediator).await;
        let validator = w.app.actor("Val").await;
        w.app.grant(&validator, GlobalRole::Validator).await;
        let stranger = w.app.actor("Ze").await;

        let expiring = seed(&w.app, personal(AlertKind::SearchExpiring, &w.volunteer_a)).await;
        let family = seed(
            &w.app,
            personal(AlertKind::FamilySearchReceived, &w.admin_a),
        )
        .await;
        let shelter_alert = seed(&w.app, alert_for(&[&w.shelter_a])).await;
        let mediator_alert = seed(&w.app, personal(AlertKind::SearchExpiring, &mediator)).await;

        assert_eq!(listed(&w.app, &w.volunteer_a).await, [expiring.clone()]);
        let mut admin_sees = listed(&w.app, &w.admin_a).await;
        admin_sees.sort();
        let mut expected = vec![family.clone(), shelter_alert.clone()];
        expected.sort();
        assert_eq!(admin_sees, expected);
        assert!(listed(&w.app, &w.admin_b).await.is_empty());
        assert_eq!(listed(&w.app, &mediator).await, [mediator_alert.clone()]);
        // Not even the Civil Defense, who reads every other alert, sees them; a validator neither.
        assert_eq!(listed(&w.app, &w.civil_defense).await, [shelter_alert]);
        assert!(listed(&w.app, &validator).await.is_empty());
        assert_eq!(
            w.app
                .send("GET", "/v1/alerts", &stranger.secret, None)
                .await
                .0,
            StatusCode::FORBIDDEN
        );

        // Only the volunteer resolves it; it leaves their list and nobody else is affected.
        let path = format!("/v1/alerts/{expiring}/resolve");
        for who in [&w.admin_a, &w.civil_defense, &mediator, &validator] {
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
                .send("POST", &path, &w.volunteer_a.secret, None)
                .await
                .0,
            StatusCode::OK
        );
        assert!(listed(&w.app, &w.volunteer_a).await.is_empty());
        assert_eq!(listed(&w.app, &mediator).await, [mediator_alert]);
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

    const ALL_KINDS: [AlertKind; 14] = [
        AlertKind::ShelterFlagged,
        AlertKind::EventOverlap,
        AlertKind::VolunteerRemoved,
        AlertKind::EndorserFlagged,
        AlertKind::ChildSeparated,
        AlertKind::RevalidationFailed,
        AlertKind::ExceptionalExit,
        AlertKind::HouseholdConflict,
        AlertKind::UnicoLimit,
        AlertKind::SearchExpiring,
        AlertKind::FamilySearchReceived,
        AlertKind::AnomalyVolume,
        AlertKind::AnomalyDistance,
        AlertKind::OutOfArea,
    ];

    #[test]
    fn the_kind_name_is_the_serde_name_of_every_variant() {
        for kind in ALL_KINDS {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                json!(kind.as_str()),
                "{kind:?}"
            );
        }
    }

    fn viewer(id: &str, roles: &[GlobalRole], admin_of: &[&str], member_of: &[&str]) -> Volunteer {
        use crate::auth::Membership;
        use crate::members::ShelterRole;
        use crate::shelters::Confirmation;
        let now = store::now();
        let membership = |shelter: &&str, role| Membership {
            shelter_id: shelter.to_string(),
            name: String::new(),
            role,
            confirmation: Confirmation::Confirmed,
            event_id: None,
        };
        Volunteer {
            id: id.into(),
            doc: crate::volunteers::VolunteerDoc {
                name: "V".into(),
                phone: None,
                show_phone: false,
                language: "pt-BR".into(),
                global_roles: roles.to_vec(),
                role_grants: vec![],
                banned: false,
                banned_by: None,
                banned_at: None,
                flagged: false,
                flag_reasons: vec![],
                created_at: now,
                last_seen_at: now,
            },
            memberships: admin_of
                .iter()
                .map(|s| membership(s, ShelterRole::Admin))
                .chain(
                    member_of
                        .iter()
                        .map(|s| membership(s, ShelterRole::Volunteer)),
                )
                .collect(),
            session_id: "s".into(),
        }
    }

    #[test]
    fn every_alert_a_volunteer_receives_is_reachable_by_one_of_their_sources() {
        let volunteers = [
            viewer("cd", &[GlobalRole::CivilDefense], &[], &[]),
            viewer("validator", &[GlobalRole::Validator], &[], &[]),
            viewer("protection", &[GlobalRole::Protection], &[], &[]),
            viewer("mediator", &[GlobalRole::Mediator], &[], &[]),
            viewer("admin", &[], &["a"], &["b"]),
            viewer("member", &[], &[], &["a"]),
            viewer("stranger", &[], &[], &[]),
        ];
        let mut received = 0;
        for kind in ALL_KINDS {
            for informational in [false, true] {
                for for_protection in [false, true] {
                    for shelters in [vec![], vec!["a".to_string()], vec!["b".to_string()]] {
                        for about in [vec![], vec!["admin".to_string()], vec!["cd".to_string()]] {
                            let mut alert = AlertDoc::new(kind, "t", "t", json!({}));
                            alert.informational = informational;
                            alert.for_protection = for_protection;
                            alert.shelter_ids = shelters.clone();
                            alert.volunteer_ids = about.clone();
                            // As `new` and `personal` build them: only personal kinds skip the Civil Defense.
                            alert.for_civil_defense = !kind.is_personal();
                            for volunteer in &volunteers {
                                if !is_recipient(volunteer, &alert) {
                                    continue;
                                }
                                received += 1;
                                assert!(
                                    sources_of(volunteer)
                                        .iter()
                                        .any(|s| s.admits(&alert, &volunteer.id)),
                                    "{} cannot reach {alert:?}",
                                    volunteer.id
                                );
                            }
                        }
                    }
                }
            }
        }
        assert!(received > 100);
    }

    #[test]
    fn an_alert_reached_by_two_sources_is_listed_once_newest_first() {
        let now = store::now();
        let mut older = alert_for(&["a"]);
        older.created_at = now;
        let mut newer = alert_for(&["a"]);
        newer.created_at = store::now();
        let mut tie = newer.clone();
        let (older_id, newer_id, tie_id) = ("1".to_string(), "2".to_string(), "3".to_string());
        older.id = Some(older_id.clone());
        newer.id = Some(newer_id.clone());
        tie.id = Some(tie_id.clone());
        // The Civil Defense admin of shelter A is reached through both sources.
        let volunteer = viewer("both", &[GlobalRole::CivilDefense], &["a"], &[]);
        let found = vec![
            older.clone(),
            newer.clone(),
            older.clone(),
            tie.clone(),
            newer.clone(),
        ];
        let ids: Vec<_> = arrange(found, &volunteer)
            .into_iter()
            .map(|a| a.id.unwrap())
            .collect();
        assert_eq!(ids, [newer_id, tie_id, older_id]);
    }

    /// More open alerts of other recipients than the page holds; the ones listed are still found.
    #[tokio::test]
    async fn alerts_for_a_recipient_are_not_pushed_out_by_other_recipients_alerts() {
        let w = world().await;
        let protection_officer = w.app.actor("Prot").await;
        w.app
            .grant(&protection_officer, GlobalRole::Protection)
            .await;
        let validator = w.app.actor("Val").await;
        w.app.grant(&validator, GlobalRole::Validator).await;

        let mut separated = AlertDoc::new(AlertKind::ChildSeparated, "child", "c1", json!({}))
            .for_shelters(vec![w.shelter_a.clone()]);
        separated.for_protection = true;
        let separated = seed(&w.app, separated).await;
        let mut anomaly = AlertDoc::new(AlertKind::AnomalyVolume, "volunteer", "v1", json!({}))
            .about_volunteers(vec!["someone".into()]);
        anomaly.informational = true;
        let anomaly = seed(&w.app, anomaly).await;
        let expiring = seed(&w.app, personal(AlertKind::SearchExpiring, &w.volunteer_a)).await;
        for _ in 0..=LIST_LIMIT {
            seed(&w.app, alert_for(&[&w.shelter_b])).await;
        }

        assert_eq!(listed(&w.app, &w.admin_a).await, [separated.clone()]);
        assert_eq!(listed(&w.app, &protection_officer).await, [separated]);
        assert_eq!(listed(&w.app, &validator).await, [anomaly]);
        assert_eq!(listed(&w.app, &w.volunteer_a).await, [expiring]);
        // A source holding more than the limit lists its newest `LIST_LIMIT` (the ponytail above).
        assert_eq!(listed(&w.app, &w.admin_b).await.len(), LIST_LIMIT as usize);
    }
}
