//! Cards of a household (spec 3.5.1, 7, 8.5). Owner B.
//!
//! A card belongs to a household and works in any shelter of the network, so reading one needs
//! only a membership; changing one needs the household's shelter (see `households`). The code
//! is made on the phone: 8 characters of Crockford base32, uppercase. `cards/{code}` is created
//! with a precondition that it does not exist, so a code is never reused, not even after a cancel.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use firestore::{FirestoreDb, FirestoreInstant};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::{self, AuditEntry};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::households::{self, forbidden, load_household, load_members, members_view};
use crate::people::PersonDoc;
use crate::store::{self, now, transact, tx_create, tx_replace};

pub const COLLECTION: &str = "cards";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardStatus {
    Active,
    Canceled,
}

/// `cards/{code}`: 8 characters of Crockford base32, made on the phone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub code: Option<String>,
    pub household_id: String,
    pub shelter_id: String,
    pub status: CardStatus,
    pub issued_by: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub client_issued_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub canceled_at: Option<FirestoreInstant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_by: Option<String>,
}

// ---- code ----

const CODE_LEN: usize = 8;
const ALPHABET: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_card");
    error.detail = json!({ "field": field });
    error
}

fn is_canonical(code: &str) -> bool {
    code.chars().count() == CODE_LEN && code.chars().all(|c| ALPHABET.contains(c))
}

/// What someone typed or scanned, as Crockford reads it: case does not matter and `I`, `L` and
/// `O` stand for `1`, `1` and `0`. Anything else outside the alphabet is refused.
fn normalize(input: &str, field: &str) -> Result<String, ApiError> {
    let code: String = input
        .trim()
        .chars()
        .map(|c| match c.to_ascii_uppercase() {
            'I' | 'L' => '1',
            'O' => '0',
            other => other,
        })
        .collect();
    is_canonical(&code)
        .then_some(code)
        .ok_or_else(|| invalid(field))
}

fn card_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "card_not_found")
}

async fn load_card(db: &FirestoreDb, code: &str) -> Result<CardDoc, ApiError> {
    store::get(db, COLLECTION, code)
        .await?
        .ok_or_else(card_not_found)
}

fn view(card: &CardDoc, members: &[PersonDoc]) -> Value {
    json!({
        "code": card.code,
        "status": card.status,
        "household_id": card.household_id,
        "members": members_view(members),
    })
}

// ---- POST /v1/households/{id}/cards ----

#[derive(Debug, Deserialize)]
pub struct IssueCard {
    code: String,
    client_issued_at: String,
}

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
                    .by(&volunteer.id)
                    .in_shelter(&shelter),
            )?;
            Ok(view(&card, &members))
        })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(issued)))
}

// ---- GET /v1/cards/{code} ----

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

// ---- POST /v1/cards/{code}/cancel ----

#[derive(Debug, Default, Deserialize)]
pub struct CancelCard {
    #[serde(default)]
    replaced_by: Option<String>,
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
            let at = now();
            card.status = CardStatus::Canceled;
            card.canceled_at = Some(at);
            card.replaced_by = replaced_by;
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
                    .by(&volunteer.id)
                    .in_shelter(&shelter),
            )?;
            Ok(view(&card, &members))
        })
    })
    .await?;
    Ok(Json(canceled))
}
