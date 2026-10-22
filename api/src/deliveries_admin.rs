//! The admin side of the deliveries (spec 3.5.2, 3.5.3 item 8, 8.6): `PUT /v1/shelters/{id}/items`,
//! `GET /v1/shelters/{id}/deliveries?status=` and `POST /v1/deliveries/{id}/review`. The types are
//! in `deliveries.rs`.

use std::collections::{BTreeSet, HashMap};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use firestore::{FirestoreDb, FirestoreInstant, path};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::deliveries::{
    self, DeliveryDoc, DeliveryStatus, ItemScope, Items, Reason, ReasonKind, Verification,
};
use crate::error::ApiError;
use crate::events;
use crate::people::{self, PersonDoc};
use crate::shelters::{self, ShelterDoc};
use crate::store::{self, fs_error, tx_replace};
use crate::volunteers::{self, VolunteerDoc};

const MAX_ITEMS: usize = 50;
const MAX_ITEM_NAME_CHARS: usize = 60;
const MAX_PERIOD_DAYS: u32 = 365;
const MAX_NOTE_CHARS: usize = 500;
/// Deliveries per page of the review list.
const LIST_LIMIT: u32 = 100;

fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
    error.detail = json!({ "field": field });
    error
}

fn delivery_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "delivery_not_found")
}

fn not_in_review() -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "delivery_not_in_review")
}

// ---- PUT /v1/shelters/{id}/items ----

#[derive(Debug, Deserialize)]
pub struct PutItems {
    items: Items,
}

/// Trims the names, refuses the ones that are empty, long, with control characters or that repeat
/// (ignoring case), and drops `period_days` where `once_per_event` makes it meaningless.
fn valid_items(items: Items) -> Result<Items, ApiError> {
    if items.len() > MAX_ITEMS {
        return Err(invalid("items"));
    }
    let mut seen = BTreeSet::new();
    let mut valid = Items::new();
    for (name, mut spec) in items {
        let name = name.trim().to_string();
        if name.is_empty()
            || name.chars().count() > MAX_ITEM_NAME_CHARS
            || name.chars().any(char::is_control)
            || !seen.insert(name.to_lowercase())
        {
            return Err(invalid("item"));
        }
        if spec.once_per_event {
            spec.period_days = None;
        } else if spec
            .period_days
            .is_some_and(|days| days == 0 || days > MAX_PERIOD_DAYS)
        {
            return Err(invalid("period_days"));
        }
        valid.insert(name, spec);
    }
    Ok(valid)
}

/// `PUT /v1/shelters/{id}/items`: `{items: {<name>: {scope, period_days?, once_per_event, high_value}}}`, by an admin of the shelter; replaces `shelters.items`.
pub async fn put_items(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(id): Path<String>,
    Json(body): Json<PutItems>,
) -> Result<Json<Value>, ApiError> {
    if !volunteer.is_admin_of(&id) {
        return Err(forbidden());
    }
    events::ensure_shelter_open(&state.db, &id).await?;
    let items = valid_items(body.items)?;

    let shelter = store::transact(&state.db, |db, tx| {
        let (id, items, volunteer) = (id.clone(), items.clone(), volunteer.clone());
        Box::pin(async move {
            let mut shelter: ShelterDoc = store::get(&db, shelters::COLLECTION, &id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found"))?;
            shelter.items = items;
            tx_replace(&db, tx, shelters::COLLECTION, &id, &shelter)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new("shelter.items", "shelter", &id)
                    .by(&volunteer.id)
                    .in_shelter(&id),
            )?;
            Ok(shelter)
        })
    })
    .await?;
    events::touch(&state, shelter.event_id.as_deref()).await;
    Ok(Json(json!({ "items": shelter.items })))
}

// ---- GET /v1/shelters/{id}/deliveries ----

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    status: Option<String>,
}

/// The fields of a delivery the list needs; the id is the document's.
#[derive(Debug, Serialize, Deserialize)]
struct Row {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    id: String,
    shelter_id: String,
    #[serde(default)]
    person_id: Option<String>,
    #[serde(default)]
    card_code: Option<String>,
    item: String,
    scope: ItemScope,
    quantity: u32,
    delivered_by: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    delivered_at: FirestoreInstant,
    verification: Verification,
    #[serde(default)]
    approved_by: Option<String>,
    status: DeliveryStatus,
    #[serde(default)]
    reasons: Vec<Reason>,
    #[serde(default)]
    review_note: Option<String>,
}

#[derive(Debug, Serialize)]
struct ReasonView {
    kind: ReasonKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_delivery_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_delivered_at: Option<String>,
    /// Only when the other delivery's shelter is confirmed, or is this very shelter (spec 4.7).
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_shelter_name: Option<String>,
}

#[derive(Debug, Serialize)]
struct DeliveryView {
    id: String,
    item: String,
    scope: ItemScope,
    quantity: u32,
    status: DeliveryStatus,
    verification: Verification,
    reasons: Vec<ReasonView>,
    delivered_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    person_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    card_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivered_by_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    approved_by_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    review_note: Option<String>,
}

fn status_name(status: DeliveryStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

async fn volunteer_name(
    db: &FirestoreDb,
    cache: &mut HashMap<String, Option<String>>,
    id: &str,
) -> Result<Option<String>, ApiError> {
    if let Some(name) = cache.get(id) {
        return Ok(name.clone());
    }
    let doc: Option<VolunteerDoc> = store::get(db, volunteers::COLLECTION, id).await?;
    let name = doc.map(|d| d.name);
    cache.insert(id.to_string(), name.clone());
    Ok(name)
}

async fn person_name(
    db: &FirestoreDb,
    cache: &mut HashMap<String, Option<String>>,
    id: &str,
) -> Result<Option<String>, ApiError> {
    if let Some(name) = cache.get(id) {
        return Ok(name.clone());
    }
    let doc: Option<PersonDoc> = store::get(db, people::COLLECTION, id).await?;
    let name = doc.and_then(|p| p.social_name.or(p.name));
    cache.insert(id.to_string(), name.clone());
    Ok(name)
}

async fn reason_view(
    db: &FirestoreDb,
    requester: &ShelterDoc,
    shelter_id: &str,
    reason: &Reason,
) -> Result<ReasonView, ApiError> {
    let mut view = ReasonView {
        kind: reason.kind,
        previous_delivery_id: reason.previous_delivery_id.clone(),
        previous_delivered_at: None,
        previous_shelter_name: None,
    };
    let Some(previous_id) = &reason.previous_delivery_id else {
        return Ok(view);
    };
    let previous: Option<DeliveryDoc> = store::get(db, deliveries::COLLECTION, previous_id).await?;
    let Some(previous) = previous else {
        return Ok(view);
    };
    let other: Option<ShelterDoc> =
        store::get(db, shelters::COLLECTION, &previous.shelter_id).await?;
    let (date, name) = deliveries::previous_visibility(
        requester,
        shelter_id,
        &previous.shelter_id,
        other.as_ref(),
    );
    if date {
        view.previous_delivered_at = Some(previous.delivered_at.to_string());
    }
    if name {
        view.previous_shelter_name = other.map(|o| o.name);
    }
    Ok(view)
}

/// `GET /v1/shelters/{id}/deliveries?status=`: the shelter's deliveries of that status, newest
/// first, by an admin of it. `status` is required: the index is `(shelter_id, status, delivered_at)`.
pub async fn list_deliveries(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(id): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    if !volunteer.is_admin_of(&id) {
        return Err(forbidden());
    }
    let status: DeliveryStatus = query
        .status
        .as_deref()
        .and_then(|s| serde_json::from_value(Value::String(s.to_string())).ok())
        .ok_or_else(|| invalid("status"))?;
    let wanted = status_name(status);
    let requester: ShelterDoc = store::get(&state.db, shelters::COLLECTION, &id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found"))?;

    let rows: Vec<Row> = state
        .db
        .fluent()
        .select()
        .from(deliveries::COLLECTION)
        .filter(|q| {
            q.for_all([
                q.field(path!(Row::shelter_id)).eq(id.clone()),
                q.field(path!(Row::status)).eq(wanted.clone()),
            ])
        })
        .order_by([(
            path!(Row::delivered_at),
            firestore::FirestoreQueryDirection::Descending,
        )])
        .limit(LIST_LIMIT)
        .obj()
        .query()
        .await
        .map_err(fs_error)?;

    let (mut volunteers_seen, mut people_seen) = (HashMap::new(), HashMap::new());
    let mut views = Vec::with_capacity(rows.len());
    for row in rows {
        let mut reasons = Vec::with_capacity(row.reasons.len());
        for reason in &row.reasons {
            reasons.push(reason_view(&state.db, &requester, &id, reason).await?);
        }
        let person_name = match &row.person_id {
            Some(person_id) => person_name(&state.db, &mut people_seen, person_id).await?,
            None => None,
        };
        let approved_by_name = match &row.approved_by {
            Some(admin_id) => volunteer_name(&state.db, &mut volunteers_seen, admin_id).await?,
            None => None,
        };
        views.push(DeliveryView {
            id: row.id,
            item: row.item,
            scope: row.scope,
            quantity: row.quantity,
            status: row.status,
            verification: row.verification,
            reasons,
            delivered_at: row.delivered_at.to_string(),
            person_name,
            card_code: row.card_code,
            delivered_by_name: volunteer_name(&state.db, &mut volunteers_seen, &row.delivered_by)
                .await?,
            approved_by_name,
            review_note: row.review_note,
        });
    }
    Ok(Json(json!({ "deliveries": views })))
}

// ---- POST /v1/deliveries/{id}/review ----

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Decision {
    ReviewedOk,
    Irregular,
}

#[derive(Debug, Deserialize)]
pub struct ReviewBody {
    decision: Decision,
    #[serde(default)]
    note: String,
}

/// `POST /v1/deliveries/{id}/review`: `{decision: reviewed_ok | irregular, note}`, by an admin of the delivery's shelter.
pub async fn review_delivery(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(id): Path<String>,
    Json(body): Json<ReviewBody>,
) -> Result<Json<Value>, ApiError> {
    let note = body.note.trim();
    if note.chars().count() > MAX_NOTE_CHARS {
        return Err(invalid("note"));
    }
    let note = (!note.is_empty()).then(|| note.to_string());
    let status = match body.decision {
        Decision::ReviewedOk => DeliveryStatus::ReviewedOk,
        Decision::Irregular => DeliveryStatus::Irregular,
    };

    let delivery: DeliveryDoc = store::get(&state.db, deliveries::COLLECTION, &id)
        .await?
        .ok_or_else(delivery_not_found)?;
    if !volunteer.is_admin_of(&delivery.shelter_id) {
        return Err(forbidden());
    }
    events::ensure_shelter_open(&state.db, &delivery.shelter_id).await?;

    let reviewed = store::transact(&state.db, |db, tx| {
        let (id, note, volunteer) = (id.clone(), note.clone(), volunteer.clone());
        Box::pin(async move {
            let mut delivery: DeliveryDoc = store::get(&db, deliveries::COLLECTION, &id)
                .await?
                .ok_or_else(delivery_not_found)?;
            if delivery.status != DeliveryStatus::InReview {
                return Err(not_in_review());
            }
            delivery.status = status;
            delivery.reviewed_by = Some(volunteer.id.clone());
            delivery.review_note = note;
            tx_replace(&db, tx, deliveries::COLLECTION, &id, &delivery)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new("delivery.review", "delivery", &id)
                    .by(&volunteer.id)
                    .in_shelter(&delivery.shelter_id)
                    .basis(LegalBasis::FraudPrevention),
            )?;
            Ok(delivery)
        })
    })
    .await?;
    events::touch(&state, reviewed.event_id.as_deref()).await;
    Ok(Json(
        json!({ "delivery_id": id, "status": reviewed.status }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deliveries::ItemSpec;
    use crate::events::{self as events_mod, EventDoc, EventStatus};
    use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, audit_actions, new_id};
    use crate::volunteers::GlobalRole;

    fn spec(scope: &str, extra: Value) -> Value {
        let mut body = json!({ "scope": scope, "once_per_event": false, "high_value": false });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        body
    }

    fn items_body(items: Value) -> Option<Value> {
        Some(json!({ "items": items }))
    }

    struct World {
        app: TestApp,
        admin: Actor,
        volunteer: Actor,
        shelter: String,
    }

    async fn world() -> World {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.create_shelter(&admin, "Ginásio", ORIGIN).await;
        let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let volunteer = app.join(&code, "Vol").await;
        World {
            app,
            admin,
            volunteer,
            shelter,
        }
    }

    async fn put_items_as(w: &World, who: &Actor, body: Option<Value>) -> (StatusCode, Value) {
        w.app
            .send(
                "PUT",
                &format!("/v1/shelters/{}/items", w.shelter),
                &who.secret,
                body,
            )
            .await
    }

    async fn close_event_of(app: &TestApp, shelter_id: &str) {
        let shelter: ShelterDoc = store::get(&app.state.db, shelters::COLLECTION, shelter_id)
            .await
            .unwrap()
            .unwrap();
        let event_id = shelter.event_id.unwrap();
        let mut event: EventDoc = store::get(&app.state.db, events_mod::COLLECTION, &event_id)
            .await
            .unwrap()
            .unwrap();
        event.status = EventStatus::Closed;
        event.closed_at = Some(store::now());
        store::replace(&app.state.db, events_mod::COLLECTION, &event_id, &event)
            .await
            .unwrap();
    }

    // -- items --

    #[tokio::test]
    async fn put_items_saves_returns_replaces_and_audits() {
        let w = world().await;
        let first = json!({
            "cesta": spec("per_household", json!({ "period_days": 7 })),
            "colchão": spec("per_person", json!({ "once_per_event": true, "high_value": true, "period_days": 3 })),
        });
        let (status, body) = put_items_as(&w, &w.admin, items_body(first)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        // `period_days` is dropped where `once_per_event` applies.
        assert_eq!(
            body["items"],
            json!({
                "cesta": { "scope": "per_household", "period_days": 7, "once_per_event": false, "high_value": false },
                "colchão": { "scope": "per_person", "once_per_event": true, "high_value": true },
            })
        );
        let (_, view) = w
            .app
            .send(
                "GET",
                &format!("/v1/shelters/{}", w.shelter),
                &w.admin.secret,
                None,
            )
            .await;
        assert_eq!(view["shelter"]["items"], body["items"]);

        let second = json!({ "kit": spec("per_person", json!({})) });
        let (status, body) = put_items_as(&w, &w.admin, items_body(second)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["items"],
            json!({ "kit": { "scope": "per_person", "once_per_event": false, "high_value": false } })
        );
        let stored: ShelterDoc = store::get(&w.app.state.db, shelters::COLLECTION, &w.shelter)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.items.keys().collect::<Vec<_>>(), vec!["kit"]);
        assert_eq!(
            stored.items["kit"],
            ItemSpec {
                scope: ItemScope::PerPerson,
                period_days: None,
                once_per_event: false,
                high_value: false
            }
        );

        let (status, body) = put_items_as(&w, &w.admin, items_body(json!({}))).await;
        assert_eq!((status, &body["items"]), (StatusCode::OK, &json!({})));
        let actions = audit_actions(&w.app.state.db).await;
        assert_eq!(
            actions.iter().filter(|a| *a == "shelter.items").count(),
            3,
            "{actions:?}"
        );
    }

    #[tokio::test]
    async fn put_items_refuses_invalid_lists_and_keeps_the_old_one() {
        let w = world().await;
        let keep = json!({ "cesta": spec("per_household", json!({})) });
        put_items_as(&w, &w.admin, items_body(keep.clone())).await;

        let many: serde_json::Map<String, Value> = (0..=MAX_ITEMS)
            .map(|n| (format!("item {n}"), spec("per_person", json!({}))))
            .collect();
        let cases = [
            ("item", json!({ "": spec("per_person", json!({})) })),
            ("item", json!({ "   ": spec("per_person", json!({})) })),
            (
                "item",
                json!({ "x".repeat(MAX_ITEM_NAME_CHARS + 1): spec("per_person", json!({})) }),
            ),
            ("item", json!({ "a\nb": spec("per_person", json!({})) })),
            (
                "item",
                json!({ "Cesta": spec("per_person", json!({})), " cesta ": spec("per_household", json!({})) }),
            ),
            (
                "period_days",
                json!({ "a": spec("per_person", json!({ "period_days": 0 })) }),
            ),
            (
                "period_days",
                json!({ "a": spec("per_person", json!({ "period_days": MAX_PERIOD_DAYS + 1 })) }),
            ),
            ("items", Value::Object(many)),
        ];
        for (field, items) in cases {
            let (status, body) = put_items_as(&w, &w.admin, items_body(items.clone())).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{items}");
            assert_eq!(body["error"], "invalid_request", "{items}");
            assert_eq!(body["detail"]["field"], field, "{items}");
        }
        // The list as it was, and a bad scope is refused at the door.
        let stored: ShelterDoc = store::get(&w.app.state.db, shelters::COLLECTION, &w.shelter)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.items.keys().collect::<Vec<_>>(), vec!["cesta"]);
        let (status, _) = put_items_as(
            &w,
            &w.admin,
            items_body(json!({ "a": spec("everyone", json!({})) })),
        )
        .await;
        assert!(status.is_client_error());
        // The limits themselves are accepted.
        let edge = json!({
            "x".repeat(MAX_ITEM_NAME_CHARS): spec("per_person", json!({ "period_days": MAX_PERIOD_DAYS })),
        });
        let (status, _) = put_items_as(&w, &w.admin, items_body(edge)).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn put_items_follows_the_permission_matrix() {
        let w = world().await;
        let body = items_body(json!({ "cesta": spec("per_household", json!({})) }));
        let civil_defense = w.app.actor("DC").await;
        w.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let validator = w.app.actor("Val").await;
        w.app.grant(&validator, GlobalRole::Validator).await;
        let stranger = w.app.actor("Ze").await;
        for who in [&w.volunteer, &civil_defense, &validator, &stranger] {
            let (status, answer) = put_items_as(&w, who, body.clone()).await;
            assert_eq!(
                (status, answer["error"].as_str()),
                (StatusCode::FORBIDDEN, Some("forbidden"))
            );
        }
        let (status, _, answer) = w
            .app
            .request(
                "PUT",
                &format!("/v1/shelters/{}/items", w.shelter),
                &[],
                body.clone(),
            )
            .await;
        assert_eq!(
            (status, answer["error"].as_str()),
            (StatusCode::UNAUTHORIZED, Some("unauthorized"))
        );

        // Unconfirmed shelter: the admin configures (matrix 2.3).
        let (status, _) = put_items_as(&w, &w.admin, body.clone()).await;
        assert_eq!(status, StatusCode::OK);
        // Confirmed shelter: the admin too.
        let confirmed = w
            .app
            .confirmed_shelter(
                &w.admin,
                "Escola",
                crate::test_support::north_of_origin(5000.0),
            )
            .await;
        let (status, answer) = w
            .app
            .send(
                "PUT",
                &format!("/v1/shelters/{confirmed}/items"),
                &w.admin.secret,
                body,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
    }

    #[tokio::test]
    async fn put_items_on_a_closed_event_is_409() {
        let w = world().await;
        close_event_of(&w.app, &w.shelter).await;
        let (status, body) = put_items_as(
            &w,
            &w.admin,
            items_body(json!({ "cesta": spec("per_household", json!({})) })),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::CONFLICT, Some("event_closed"))
        );
    }

    // -- list and review --

    struct Seed<'a> {
        id: &'a str,
        shelter: &'a str,
        person: Option<&'a str>,
        by: &'a Actor,
        status: DeliveryStatus,
        at: &'a str,
        reasons: Vec<Reason>,
    }

    async fn seed(w: &World, s: Seed<'_>) {
        let at: FirestoreInstant = s.at.parse().unwrap();
        let doc = DeliveryDoc {
            shelter_id: s.shelter.into(),
            shelter_name: "x".into(),
            event_id: None,
            person_id: s.person.map(str::to_string),
            household_id: Some("h1".into()),
            household_pseudonym: None,
            card_code: Some("ABCD2345".into()),
            item: "cesta".into(),
            scope: ItemScope::PerHousehold,
            quantity: 1,
            delivered_by: s.by.id.clone(),
            delivered_at: at,
            received_at: at,
            verification: Verification::Card,
            approved_by: None,
            status: s.status,
            reasons: s.reasons,
            offline_claimed: false,
            reviewed_by: None,
            review_note: None,
            pseudonymized: false,
        };
        store::replace(&w.app.state.db, deliveries::COLLECTION, s.id, &doc)
            .await
            .unwrap();
    }

    async fn seed_person(w: &World, id: &str, name: &str) {
        let mut person = crate::exits::fixtures::person(crate::people::Level::Verified, None);
        person.name = Some(name.into());
        crate::exits::fixtures::put(&w.app, id, &person).await;
    }

    async fn list_as(w: &World, who: &Actor, shelter: &str, status: &str) -> (StatusCode, Value) {
        w.app
            .send(
                "GET",
                &format!("/v1/shelters/{shelter}/deliveries?status={status}"),
                &who.secret,
                None,
            )
            .await
    }

    #[tokio::test]
    async fn list_filters_orders_names_and_never_shows_the_person_id() {
        let w = world().await;
        let person = new_id();
        seed_person(&w, &person, "Maria Souza").await;
        for (id, status, at) in [
            ("d-old", DeliveryStatus::InReview, "2026-10-21T10:00:00Z"),
            ("d-new", DeliveryStatus::InReview, "2026-10-21T12:00:00Z"),
            ("d-mid", DeliveryStatus::InReview, "2026-10-21T11:00:00Z"),
            ("d-ok", DeliveryStatus::Ok, "2026-10-21T13:00:00Z"),
            ("d-irr", DeliveryStatus::Irregular, "2026-10-21T14:00:00Z"),
            ("d-rok", DeliveryStatus::ReviewedOk, "2026-10-21T15:00:00Z"),
        ] {
            seed(
                &w,
                Seed {
                    id,
                    shelter: &w.shelter,
                    person: Some(&person),
                    by: &w.volunteer,
                    status,
                    at,
                    reasons: vec![],
                },
            )
            .await;
        }
        // Another shelter's delivery never shows.
        seed(
            &w,
            Seed {
                id: "d-other",
                shelter: "other-shelter",
                person: Some(&person),
                by: &w.volunteer,
                status: DeliveryStatus::InReview,
                at: "2026-10-21T16:00:00Z",
                reasons: vec![],
            },
        )
        .await;

        let (status, body) = list_as(&w, &w.admin, &w.shelter, "in_review").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let ids: Vec<&str> = body["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["d-new", "d-mid", "d-old"]);
        let first = &body["deliveries"][0];
        assert_eq!(first["person_name"], "Maria Souza");
        assert_eq!(first["delivered_by_name"], "Vol");
        assert_eq!(first["card_code"], "ABCD2345");
        assert_eq!(first["item"], "cesta");
        assert!(!body.to_string().contains(&person), "person_id leaked");
        assert!(first.get("person_id").is_none());

        for (filter, id) in [
            ("ok", "d-ok"),
            ("irregular", "d-irr"),
            ("reviewed_ok", "d-rok"),
        ] {
            let (_, body) = list_as(&w, &w.admin, &w.shelter, filter).await;
            assert_eq!(body["deliveries"].as_array().unwrap().len(), 1, "{filter}");
            assert_eq!(body["deliveries"][0]["id"], id);
        }
    }

    #[tokio::test]
    async fn list_is_one_page_of_at_most_the_limit() {
        let w = world().await;
        for n in 0..(LIST_LIMIT + 3) {
            seed(
                &w,
                Seed {
                    id: &format!("d{n}"),
                    shelter: &w.shelter,
                    person: None,
                    by: &w.volunteer,
                    status: DeliveryStatus::InReview,
                    at: "2026-10-21T10:00:00Z",
                    reasons: vec![],
                },
            )
            .await;
        }
        let (_, body) = list_as(&w, &w.admin, &w.shelter, "in_review").await;
        assert_eq!(
            body["deliveries"].as_array().unwrap().len(),
            LIST_LIMIT as usize
        );
    }

    #[tokio::test]
    async fn list_refuses_a_missing_or_unknown_status_and_non_admins() {
        let w = world().await;
        for query in ["", "?status=", "?status=nope"] {
            let (status, body) = w
                .app
                .send(
                    "GET",
                    &format!("/v1/shelters/{}/deliveries{query}", w.shelter),
                    &w.admin.secret,
                    None,
                )
                .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
            assert_eq!(body["detail"]["field"], "status", "{query}");
        }
        let civil_defense = w.app.actor("DC").await;
        w.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let stranger = w.app.actor("Ze").await;
        for who in [&w.volunteer, &civil_defense, &stranger] {
            let (status, _) = list_as(&w, who, &w.shelter, "in_review").await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }
        // Admin of another shelter does not see this one.
        let other_admin = w.app.actor("Other").await;
        w.app
            .create_shelter(
                &other_admin,
                "Outro",
                crate::test_support::north_of_origin(9000.0),
            )
            .await;
        let (status, _) = list_as(&w, &other_admin, &w.shelter, "in_review").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = w
            .app
            .request(
                "GET",
                &format!("/v1/shelters/{}/deliveries?status=ok", w.shelter),
                &[],
                None,
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_other_shelter_is_named_only_when_it_is_confirmed() {
        let w = world().await;
        let validator = w.app.actor("Val").await;
        w.app.grant(&validator, GlobalRole::Validator).await;
        let far = w.app.actor("Far").await;
        let unconfirmed = w
            .app
            .create_shelter(
                &far,
                "Abrigo escondido",
                crate::test_support::north_of_origin(4000.0),
            )
            .await;
        let confirmed = w
            .app
            .confirmed_shelter(
                &far,
                "Abrigo aberto",
                crate::test_support::north_of_origin(8000.0),
            )
            .await;
        for (id, shelter) in [
            ("p-unc", &unconfirmed),
            ("p-con", &confirmed),
            ("p-own", &w.shelter),
        ] {
            seed(
                &w,
                Seed {
                    id,
                    shelter,
                    person: None,
                    by: &w.volunteer,
                    status: DeliveryStatus::Ok,
                    at: "2026-10-20T10:00:00Z",
                    reasons: vec![],
                },
            )
            .await;
        }
        for (id, previous) in [
            ("d1", "p-unc"),
            ("d2", "p-con"),
            ("d3", "p-own"),
            ("d4", "gone"),
        ] {
            seed(
                &w,
                Seed {
                    id,
                    shelter: &w.shelter,
                    person: None,
                    by: &w.volunteer,
                    status: DeliveryStatus::InReview,
                    at: "2026-10-21T10:00:00Z",
                    reasons: vec![Reason {
                        kind: ReasonKind::DuplicateInPeriod,
                        previous_delivery_id: Some(previous.into()),
                    }],
                },
            )
            .await;
        }
        let (_, body) = list_as(&w, &w.admin, &w.shelter, "in_review").await;
        let reason_of = |id: &str| {
            body["deliveries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|d| d["id"] == id)
                .unwrap()["reasons"][0]
                .clone()
        };
        let unc = reason_of("d1");
        assert_eq!(unc["previous_delivery_id"], "p-unc");
        // This shelter is not confirmed: of another shelter's delivery it is told nothing.
        for other in [unc, reason_of("d2")] {
            assert!(other.get("previous_delivered_at").is_none(), "{other}");
            assert!(other.get("previous_shelter_name").is_none(), "{other}");
        }
        assert!(!body.to_string().contains("Abrigo escondido"));
        assert!(!body.to_string().contains("Abrigo aberto"));
        // Its own delivery it is told, with the date.
        assert_eq!(reason_of("d3")["previous_shelter_name"], "Ginásio");
        assert!(reason_of("d3").get("previous_delivered_at").is_some());
        let gone = reason_of("d4");
        assert_eq!(gone["previous_delivery_id"], "gone");
        assert!(gone.get("previous_delivered_at").is_none());
    }

    async fn review_as(w: &World, who: &Actor, id: &str, body: Value) -> (StatusCode, Value) {
        w.app
            .send(
                "POST",
                &format!("/v1/deliveries/{id}/review"),
                &who.secret,
                Some(body),
            )
            .await
    }

    async fn stored(w: &World, id: &str) -> DeliveryDoc {
        store::get(&w.app.state.db, deliveries::COLLECTION, id)
            .await
            .unwrap()
            .unwrap()
    }

    async fn in_review(w: &World, id: &str) {
        seed(
            w,
            Seed {
                id,
                shelter: &w.shelter,
                person: None,
                by: &w.volunteer,
                status: DeliveryStatus::InReview,
                at: "2026-10-21T10:00:00Z",
                reasons: vec![],
            },
        )
        .await;
    }

    #[tokio::test]
    async fn review_decides_records_the_admin_and_audits_with_fraud_prevention() {
        let w = world().await;
        in_review(&w, "d1").await;
        in_review(&w, "d2").await;
        let (status, body) = review_as(
            &w,
            &w.admin,
            "d1",
            json!({ "decision": "reviewed_ok", "note": "  conferido com a família  " }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body,
            json!({ "delivery_id": "d1", "status": "reviewed_ok" })
        );
        let d1 = stored(&w, "d1").await;
        assert_eq!(d1.status, DeliveryStatus::ReviewedOk);
        assert_eq!(d1.reviewed_by.as_deref(), Some(w.admin.id.as_str()));
        assert_eq!(d1.review_note.as_deref(), Some("conferido com a família"));

        let (status, _) = review_as(
            &w,
            &w.admin,
            "d2",
            json!({ "decision": "irregular", "note": "" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let d2 = stored(&w, "d2").await;
        assert_eq!(
            (d2.status, d2.review_note),
            (DeliveryStatus::Irregular, None)
        );

        let entries: Vec<Value> = all_docs(&w.app.state.db, "audit_log")
            .await
            .into_iter()
            .filter(|e| e["action"] == "delivery.review")
            .collect();
        assert_eq!(entries.len(), 2);
        for entry in &entries {
            assert_eq!(entry["legal_basis"], "fraud_prevention");
            assert_eq!(entry["volunteer_id"], w.admin.id.as_str());
            assert_eq!(entry["shelter_id"], w.shelter.as_str());
            assert_eq!(entry["target_kind"], "delivery");
        }
        // Reviewed deliveries leave the in-review list and show in the other one.
        let (_, body) = list_as(&w, &w.admin, &w.shelter, "in_review").await;
        assert_eq!(body["deliveries"], json!([]));
        let (_, body) = list_as(&w, &w.admin, &w.shelter, "reviewed_ok").await;
        assert_eq!(
            body["deliveries"][0]["review_note"],
            "conferido com a família"
        );
    }

    #[tokio::test]
    async fn review_only_takes_deliveries_in_review() {
        let w = world().await;
        for (id, status) in [
            ("ok", DeliveryStatus::Ok),
            ("rok", DeliveryStatus::ReviewedOk),
            ("irr", DeliveryStatus::Irregular),
        ] {
            seed(
                &w,
                Seed {
                    id,
                    shelter: &w.shelter,
                    person: None,
                    by: &w.volunteer,
                    status,
                    at: "2026-10-21T10:00:00Z",
                    reasons: vec![],
                },
            )
            .await;
            let (code, body) = review_as(
                &w,
                &w.admin,
                id,
                json!({ "decision": "irregular", "note": "x" }),
            )
            .await;
            assert_eq!(
                (code, body["error"].as_str()),
                (StatusCode::CONFLICT, Some("delivery_not_in_review")),
                "{id}"
            );
            assert_eq!(stored(&w, id).await.status, status);
        }
        let (code, body) = review_as(
            &w,
            &w.admin,
            "missing",
            json!({ "decision": "irregular", "note": "" }),
        )
        .await;
        assert_eq!(
            (code, body["error"].as_str()),
            (StatusCode::NOT_FOUND, Some("delivery_not_found"))
        );
    }

    #[tokio::test]
    async fn review_refuses_bad_bodies_and_non_admins() {
        let w = world().await;
        in_review(&w, "d1").await;
        let (code, _) =
            review_as(&w, &w.admin, "d1", json!({ "decision": "ok", "note": "" })).await;
        assert!(code.is_client_error());
        let (code, body) = review_as(
            &w,
            &w.admin,
            "d1",
            json!({ "decision": "irregular", "note": "x".repeat(MAX_NOTE_CHARS + 1) }),
        )
        .await;
        assert_eq!(
            (code, body["detail"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("note"))
        );

        let civil_defense = w.app.actor("DC").await;
        w.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let stranger = w.app.actor("Ze").await;
        let other_admin = w.app.actor("Other").await;
        w.app
            .create_shelter(
                &other_admin,
                "Outro",
                crate::test_support::north_of_origin(9000.0),
            )
            .await;
        for who in [&w.volunteer, &civil_defense, &stranger, &other_admin] {
            let (code, body) = review_as(
                &w,
                who,
                "d1",
                json!({ "decision": "reviewed_ok", "note": "" }),
            )
            .await;
            assert_eq!(
                (code, body["error"].as_str()),
                (StatusCode::FORBIDDEN, Some("forbidden"))
            );
        }
        assert_eq!(stored(&w, "d1").await.status, DeliveryStatus::InReview);
        let (code, _, _) = w
            .app
            .request(
                "POST",
                "/v1/deliveries/d1/review",
                &[],
                Some(json!({ "decision": "irregular", "note": "" })),
            )
            .await;
        assert_eq!(code, StatusCode::UNAUTHORIZED);

        close_event_of(&w.app, &w.shelter).await;
        let (code, body) = review_as(
            &w,
            &w.admin,
            "d1",
            json!({ "decision": "reviewed_ok", "note": "" }),
        )
        .await;
        assert_eq!(
            (code, body["error"].as_str()),
            (StatusCode::CONFLICT, Some("event_closed"))
        );
    }

    #[tokio::test]
    async fn two_simultaneous_reviews_one_wins() {
        let w = world().await;
        let second_admin = {
            let code = w.app.invite(&w.admin, &w.shelter, "qr", "admin").await;
            w.app.join(&code, "Admin2").await
        };
        for round in 0..3 {
            let id = format!("race-{round}");
            in_review(&w, &id).await;
            let (a, b) = tokio::join!(
                review_as(
                    &w,
                    &w.admin,
                    &id,
                    json!({ "decision": "reviewed_ok", "note": "a" })
                ),
                review_as(
                    &w,
                    &second_admin,
                    &id,
                    json!({ "decision": "irregular", "note": "b" })
                ),
            );
            let mut codes = [a.0, b.0];
            codes.sort();
            assert_eq!(codes, [StatusCode::OK, StatusCode::CONFLICT], "{a:?} {b:?}");
            let loser = if a.0 == StatusCode::OK { &b } else { &a };
            assert_eq!(loser.1["error"], "delivery_not_in_review");
            // The stored decision is the winner's, whole.
            let doc = stored(&w, &id).await;
            let (winner, note) = if a.0 == StatusCode::OK {
                (&w.admin, "a")
            } else {
                (&second_admin, "b")
            };
            assert_eq!(doc.reviewed_by.as_deref(), Some(winner.id.as_str()));
            assert_eq!(doc.review_note.as_deref(), Some(note));
        }
        let reviews = all_docs(&w.app.state.db, "audit_log")
            .await
            .into_iter()
            .filter(|e| e["action"] == "delivery.review")
            .count();
        assert_eq!(reviews, 3, "one audit entry per decided delivery");
    }
}
