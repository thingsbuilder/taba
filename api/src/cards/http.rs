//! The routes (spec 3.5.1, 8.5): `POST /v1/households/{id}/cards`, `GET /v1/cards/{code}` and
//! `POST /v1/cards/{code}/cancel`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde_json::Value;

use super::domain::{is_canonical, normalize};
use super::model::{COLLECTION, CancelCard, CardDoc, CardStatus, IssueCard, invalid};
use super::persist::load_card;
use super::view::view;
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::households::{self, forbidden, load_household, load_members};
use crate::store::{self, now, transact, tx_create, tx_replace};

pub async fn post_card(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(household_id): Path<String>,
    Json(request): Json<IssueCard>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if !is_canonical(&request.code) {
        return Err(invalid("code"));
    }
    let client_issued_at: FirestoreInstant = request
        .client_issued_at
        .parse()
        .map_err(|_| invalid("client_issued_at"))?;
    let code = request.code;

    let issued = transact(&state.db, |db, tx| {
        let (volunteer, household_id, code) =
            (volunteer.clone(), household_id.clone(), code.clone());
        Box::pin(async move {
            let mut household = load_household(&db, &household_id).await?;
            let members = load_members(&db, &household).await?;
            let shelter =
                households::shelter_of_access(&volunteer, &members).ok_or_else(forbidden)?;
            if let Some(existing) = store::get::<CardDoc>(&db, COLLECTION, &code).await? {
                // The same request again (a lost response, a retry) gets the same answer.
                let same =
                    existing.household_id == household_id && existing.status == CardStatus::Active;
                return if same {
                    Ok(view(&existing, &members))
                } else {
                    Err(ApiError::code_taken())
                };
            }
            crate::events::ensure_shelter_open(&db, &shelter).await?;
            if household.card_code.is_some() {
                return Err(ApiError::household_has_card());
            }
            let at = now();
            let card = CardDoc {
                code: Some(code.clone()),
                household_id: household_id.clone(),
                shelter_id: shelter.clone(),
                status: CardStatus::Active,
                issued_by: volunteer.id.clone(),
                client_issued_at,
                created_at: at,
                canceled_at: None,
                replaced_by: None,
            };
            tx_create(&db, tx, COLLECTION, &code, &card)?;
            household.card_code = Some(code.clone());
            household.updated_at = at;
            tx_replace(&db, tx, households::COLLECTION, &household_id, &household)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new("card.issue", "card", &code)
                    .basis(LegalBasis::FraudPrevention)
                    .by(&volunteer.id)
                    .in_shelter(&shelter),
            )?;
            Ok(view(&card, &members))
        })
    })
    .await?;
    households::touch_event(&state, &household_id).await;
    Ok((StatusCode::CREATED, Json(issued)))
}

pub async fn get_card(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(code): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if volunteer.memberships.is_empty() {
        return Err(forbidden());
    }
    let card = load_card(&state.db, &normalize(&code, "code")?).await?;
    let household = load_household(&state.db, &card.household_id).await?;
    let members = load_members(&state.db, &household).await?;
    Ok(Json(view(&card, &members)))
}

/// `replaced_by` is the code of the card issued next, which cannot exist yet: the household
/// keeps this one active until the cancel commits (spec 3.5.1).
pub async fn cancel_card(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(code): Path<String>,
    body: Option<Json<CancelCard>>,
) -> Result<Json<Value>, ApiError> {
    let code = normalize(&code, "code")?;
    let replaced_by = body
        .and_then(|Json(b)| b.replaced_by)
        .map(|r| normalize(&r, "replaced_by"))
        .transpose()?
        .filter(|r| *r != code);

    let canceled = transact(&state.db, |db, tx| {
        let (volunteer, code, replaced_by) = (volunteer.clone(), code.clone(), replaced_by.clone());
        Box::pin(async move {
            let mut card = load_card(&db, &code).await?;
            let mut household = load_household(&db, &card.household_id).await?;
            let members = load_members(&db, &household).await?;
            let shelter =
                households::shelter_of_access(&volunteer, &members).ok_or_else(forbidden)?;
            if card.status == CardStatus::Canceled {
                return Ok(view(&card, &members));
            }
            crate::events::ensure_shelter_open(&db, &shelter).await?;
            let at = now();
            card.cancel(replaced_by, at);
            tx_replace(&db, tx, COLLECTION, &code, &card)?;
            if household.card_code.as_deref() == Some(code.as_str()) {
                household.card_code = None;
                household.updated_at = at;
                tx_replace(
                    &db,
                    tx,
                    households::COLLECTION,
                    &card.household_id,
                    &household,
                )?;
            }
            audit::add(
                &db,
                tx,
                &AuditEntry::new("card.cancel", "card", &code)
                    .basis(LegalBasis::FraudPrevention)
                    .by(&volunteer.id)
                    .in_shelter(&shelter),
            )?;
            Ok(view(&card, &members))
        })
    })
    .await?;
    if let Ok(card) = load_card(&state.db, &code).await {
        households::touch_event(&state, &card.household_id).await;
    }
    Ok(Json(canceled))
}
