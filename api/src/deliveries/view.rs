//! The JSON shown of a delivery: the answer to a delivery that was just made and the review list
//! (spec 8.6). What is told of the other delivery follows `previous_visibility` (spec 4.7).

use firestore::FirestoreDb;
use serde::Serialize;
use serde_json::{Value, json};

use super::domain::previous_visibility;
use super::model::{
    COLLECTION, DeliveryDoc, DeliveryStatus, ItemScope, Reason, ReasonKind, Verification,
};
use super::persist::{Seen, cached};
use crate::error::ApiError;
use crate::shelters::{self, ShelterDoc};
use crate::store;

// ---- the answer ----

/// `{delivery_id, status, reasons}` (spec 8.6). What is told of the other delivery follows
/// `previous_visibility`.
pub(super) async fn answer(
    db: &FirestoreDb,
    id: &str,
    doc: &DeliveryDoc,
    requester: &ShelterDoc,
    requester_id: &str,
) -> Result<Value, ApiError> {
    let mut reasons = Vec::new();
    for reason in &doc.reasons {
        let mut view = json!({ "kind": reason.kind });
        if let Some(previous_id) = &reason.previous_delivery_id {
            view["previous_delivery_id"] = json!(previous_id);
            if let Some(previous) = store::get::<DeliveryDoc>(db, COLLECTION, previous_id).await? {
                let other: Option<ShelterDoc> =
                    store::get(db, shelters::COLLECTION, &previous.shelter_id).await?;
                let (date, name) = previous_visibility(
                    requester,
                    requester_id,
                    &previous.shelter_id,
                    other.as_ref(),
                );
                if date {
                    view["previous_delivered_at"] = json!(previous.delivered_at.to_string());
                }
                if name {
                    view["previous_shelter_name"] = json!(previous.shelter_name);
                }
            }
        }
        reasons.push(view);
    }
    Ok(json!({ "delivery_id": id, "status": doc.status, "reasons": reasons }))
}

#[derive(Debug, Serialize)]
pub(super) struct ReasonView {
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
pub(super) struct DeliveryView {
    pub(super) id: String,
    pub(super) item: String,
    pub(super) scope: ItemScope,
    pub(super) quantity: u32,
    pub(super) status: DeliveryStatus,
    pub(super) verification: Verification,
    pub(super) reasons: Vec<ReasonView>,
    pub(super) delivered_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) person_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) card_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) delivered_by_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) approved_by_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) review_note: Option<String>,
}

pub(super) async fn reason_view(
    db: &FirestoreDb,
    seen: &mut Seen,
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
    let previous = cached(db, &mut seen.deliveries, COLLECTION, previous_id).await?;
    let Some(previous) = previous else {
        return Ok(view);
    };
    let other = cached(
        db,
        &mut seen.shelters,
        shelters::COLLECTION,
        &previous.shelter_id,
    )
    .await?;
    let (date, name) =
        previous_visibility(requester, shelter_id, &previous.shelter_id, other.as_ref());
    if date {
        view.previous_delivered_at = Some(previous.delivered_at.to_string());
    }
    if name {
        view.previous_shelter_name = other.map(|o| o.name);
    }
    Ok(view)
}
