//! The routes of the alerts module.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::domain::{arrange, can_receive_any, is_recipient, sources_of};
use super::model::{AlertDoc, COLLECTION};
use super::persist::open_from;
use super::view::view;
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::store::{self, tx_replace};

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
