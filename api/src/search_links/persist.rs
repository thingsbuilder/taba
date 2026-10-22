//! Reads and writes of the search links: the claim of a link, the consumption after the search
//! and the release after a failure.

use std::future::Future;

use axum::Json;
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::model::{COLLECTION, LEASE, LinkStatus, SearchLinkDoc, busy};
use crate::alerts::{self, AlertDoc, AlertKind};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::AppState;
use crate::error::ApiError;
use crate::searches::{self, RegisterOutcome, SearchInput, SearchOwner, SearchVia};
use crate::store::{self, tx_replace};

/// Claim the link, run `register`, consume the link. `register` is `searches::register_photos`
/// in production; the tests pass a fake so no Unico is involved.
pub(super) async fn run_submit<F, Fut>(
    state: &AppState,
    code: &str,
    device: Option<String>,
    input: SearchInput,
    register: F,
) -> Result<(StatusCode, Json<Value>), ApiError>
where
    F: FnOnce(SearchOwner, SearchInput) -> Fut,
    Fut: Future<Output = Result<RegisterOutcome, ApiError>>,
{
    let link = store::transact(&state.db, |db, tx| {
        let code = code.to_string();
        Box::pin(async move {
            let now = store::now();
            let mut link = store::get::<SearchLinkDoc>(&db, COLLECTION, &code)
                .await?
                .filter(|l| l.is_valid(now))
                .ok_or_else(ApiError::search_link_invalid)?;
            // The family is told nothing of the shelter: a link of a closed event is just a dead link.
            crate::events::ensure_open(&db, link.event_id.as_deref())
                .await
                .map_err(|e| {
                    if e.code == "event_closed" {
                        ApiError::search_link_invalid()
                    } else {
                        e
                    }
                })?;
            if link.processing_until.is_some_and(|until| now < until) {
                return Err(busy());
            }
            link.processing_until = Some(now + LEASE);
            tx_replace(&db, tx, COLLECTION, &code, &link)?;
            Ok(link)
        })
    })
    .await?;

    let owner = SearchOwner {
        volunteer_id: link.created_by.clone(),
        device_id: device.clone(),
        origin_shelter_id: Some(link.shelter_id.clone()),
        event_id: link.event_id.clone(),
        via: SearchVia::FamilyLink,
        key: Some(code.to_string()),
    };
    let search_id = input.id.clone();
    let outcome = match register(owner, input).await {
        Ok(outcome) => outcome,
        Err(error) => {
            // Degraded Unico and contention are not the family's fault: no attempt is spent.
            let spent = u32::from(!matches!(error.code, "unico_degraded" | store::CONTENTION));
            release(state, code, spent).await;
            return Err(error);
        }
    };

    // No photo gave a face: nothing was registered and the link stays valid, with this try counted.
    if outcome.usable_photos == 0 {
        release(state, code, 1).await;
        return Ok(searches::answer(outcome));
    }
    store::transact(&state.db, |db, tx| {
        let (code, search_id, device) = (code.to_string(), search_id.clone(), device.clone());
        Box::pin(async move {
            let mut link = store::get::<SearchLinkDoc>(&db, COLLECTION, &code)
                .await?
                .ok_or_else(ApiError::search_link_invalid)?;
            let now = store::now();
            link.status = LinkStatus::Used;
            link.used_at = Some(now);
            link.search_id = Some(search_id.clone());
            link.processing_until = None;
            tx_replace(&db, tx, COLLECTION, &code, &link)?;
            let mut entry = AuditEntry::new("search_link.submit", "search_link", &code)
                .basis(LegalBasis::FamilyConsent)
                .by(&link.created_by)
                .in_shelter(&link.shelter_id);
            if let Some(device) = &device {
                entry = entry.on_device(device);
            }
            audit::add(&db, tx, &entry)?;
            // Informational, only for who made the link, never the content.
            let mut alert = AlertDoc::personal(
                AlertKind::FamilySearchReceived,
                "search",
                &search_id,
                &link.created_by,
                json!({ "usable_photos": outcome.usable_photos }),
            );
            alert.event_id = link.event_id.clone();
            alerts::add(&db, tx, &alert)
        })
    })
    .await?;
    crate::events::touch(state, link.event_id.as_deref()).await;
    Ok(searches::answer(outcome))
}
/// Gives the link back after a failed run, spending `spent` attempts.
pub(super) async fn release(state: &AppState, code: &str, spent: u32) {
    let released = store::transact(&state.db, |db, tx| {
        let code = code.to_string();
        Box::pin(async move {
            let Some(mut link) = store::get::<SearchLinkDoc>(&db, COLLECTION, &code).await? else {
                return Ok(());
            };
            link.processing_until = None;
            link.attempts += spent;
            tx_replace(&db, tx, COLLECTION, &code, &link)
        })
    })
    .await;
    if let Err(error) = released {
        // The lease expires by itself.
        tracing::warn!(code = error.code, "could not release a search link");
    }
}
