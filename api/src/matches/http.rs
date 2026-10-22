//! The routes of the matches module: the queues, the check and the decision.

use std::collections::{BTreeSet, HashMap};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::domain::{
    self, Decision, attends, clean_note, decide, queues_of, sees_history, whatsapp_link,
};
use super::model::{
    COLLECTION, DecidedBy, LIST_LIMIT, MatchStatus, forbidden, invalid_request, match_not_found,
};
use super::persist::{audit_now, get_many, load_match, match_audit, queue_page, shelter_name};
use super::view::history_view;
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::people::{self, PersonDoc};
use crate::search_photos;
use crate::searches::{self, SearchDoc};
use crate::shelters::ShelterDoc;
use crate::store::{self, tx_replace};
use crate::thumbnail;

/// `GET /v1/matches`: the queues the volunteer attends (spec 4.8), open ones first, then the
/// decided ones newest first. No contact of the family.
pub async fn list_matches(
    State(state): State<AppState>,
    volunteer: Volunteer,
) -> Result<Json<Value>, ApiError> {
    let queues = queues_of(&volunteer);
    if queues.is_empty() {
        return Err(forbidden());
    }
    let mut found = vec![];
    let mut has_more = false;
    for (kind, shelter_id) in &queues {
        for status in MatchStatus::ALL {
            let page = queue_page(&state.db, *kind, shelter_id.as_deref(), status).await?;
            let (page, more) = domain::cap(page, LIST_LIMIT);
            has_more |= more;
            found.extend(page);
        }
    }
    found.retain(|m| attends(&volunteer, m));
    domain::sort_for_listing(&mut found);
    let search_ids: BTreeSet<_> = found.iter().map(|m| m.search_id.clone()).collect();
    let shelter_ids: BTreeSet<_> = found
        .iter()
        .filter_map(|m| m.mediator_shelter_id.clone())
        .collect();
    let searches: HashMap<String, SearchDoc> = get_many(
        &state.db,
        searches::COLLECTION,
        search_ids.into_iter().collect(),
    )
    .await?;
    let shelters: HashMap<String, ShelterDoc> = get_many(
        &state.db,
        crate::shelters::COLLECTION,
        shelter_ids.into_iter().collect(),
    )
    .await?;
    let listed: Vec<Value> = found
        .iter()
        .filter_map(|m| {
            // A search that was purged leaves nothing to check the person against.
            let search = searches.get(&m.search_id)?;
            let shelter = m
                .mediator_shelter_id
                .as_ref()
                .and_then(|id| shelters.get(id))
                .map(|s| &s.name);
            Some(json!({
                "id": m.id,
                "status": m.status,
                "mediator_kind": m.mediator_kind,
                "is_child": m.is_child,
                "missing_person_name": search.missing_person.name,
                "shelter_name": shelter,
                "created_at": m.created_at.to_string(),
            }))
        })
        .collect();
    if !listed.is_empty() {
        // The listing names the missing persons: a read of match data (spec 4.10).
        let entry = AuditEntry::new("match.list", "match_queue", &volunteer.id)
            .by(&volunteer.id)
            .basis(LegalBasis::VitalInterest);
        audit_now(&state, entry).await?;
    }
    Ok(Json(json!({ "matches": listed, "has_more": has_more })))
}

/// `GET /v1/matches/{id}`: what the check needs, without the family's contact. The history of the
/// person only goes to `mediator`, `protection` and the Civil Defense, and only while the match
/// is open (spec 4.11).
pub async fn get_match(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(match_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let m = load_match(&state.db, &match_id).await?;
    if !attends(&volunteer, &m) {
        return Err(forbidden());
    }
    let search: SearchDoc = store::get(&state.db, searches::COLLECTION, &m.search_id)
        .await?
        .ok_or_else(match_not_found)?;
    let person: PersonDoc = store::get(&state.db, people::COLLECTION, &m.person_id)
        .await?
        .ok_or_else(match_not_found)?;
    let shelter = match &person.current_shelter_id {
        Some(id) => shelter_name(&state.db, id).await?,
        None => None,
    };
    let mut view = json!({
        "id": match_id,
        "status": m.status,
        "mediator_kind": m.mediator_kind,
        "is_child": m.is_child,
        "missing_person": search.missing_person,
        "family": {
            "name": search.family.name,
            "relationship": search.family.relationship,
            "language": search.family.language,
        },
        "person": {
            "name": person.name,
            "social_name": person.social_name,
            "wristband": person.wristband,
            "has_thumbnail": person.thumbnail_encrypted.is_some(),
        },
        "shelter_name": shelter,
        "search_photos": if m.status == MatchStatus::Open { search.retained_photos } else { 0 },
        "created_at": m.created_at.to_string(),
    });
    if m.status == MatchStatus::Open && sees_history(&volunteer) {
        view["history"] = history_view(&person);
    }
    let entry = match_audit("match.view", &volunteer, &m, ("match", &match_id));
    audit_now(&state, entry).await?;
    Ok(Json(view))
}

/// `GET /v1/matches/{id}/thumbnail`: the thumbnail of the person of the match, for who attends
/// that match; the person id never leaves the server. Every access is audited.
pub async fn get_match_thumbnail(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(match_id): Path<String>,
) -> Result<Response, ApiError> {
    let m = load_match(&state.db, &match_id).await?;
    if !attends(&volunteer, &m) {
        return Err(forbidden());
    }
    let person: PersonDoc = store::get(&state.db, people::COLLECTION, &m.person_id)
        .await?
        .ok_or_else(match_not_found)?;
    let encrypted = person
        .thumbnail_encrypted
        .as_deref()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "thumbnail_not_found"))?;
    let jpeg = thumbnail::decrypt(state.thumbnail_key(), encrypted)?;
    let entry = match_audit("thumbnail.view", &volunteer, &m, ("person", &m.person_id));
    audit_now(&state, entry).await?;
    Ok((
        [(CONTENT_TYPE, "image/jpeg"), (CACHE_CONTROL, "no-store")],
        jpeg,
    )
        .into_response())
}

/// `GET /v1/matches/{id}/search-photos/{n}`: a photo the family sent with the search, for who
/// attends the match and only while it is open; the person id never leaves the server. Every
/// access is audited.
pub async fn get_match_search_photo(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path((match_id, n)): Path<(String, u32)>,
) -> Result<Response, ApiError> {
    let m = load_match(&state.db, &match_id).await?;
    if !attends(&volunteer, &m) {
        return Err(forbidden());
    }
    let not_found = || ApiError::new(StatusCode::NOT_FOUND, "photo_not_found");
    if m.status != MatchStatus::Open {
        return Err(not_found());
    }
    let search: SearchDoc = store::get(&state.db, searches::COLLECTION, &m.search_id)
        .await?
        .ok_or_else(match_not_found)?;
    if n >= search.retained_photos {
        return Err(not_found());
    }
    let photo: search_photos::PhotoDoc = store::get(
        &state.db,
        search_photos::COLLECTION,
        &search_photos::photo_id(&m.search_id, n),
    )
    .await?
    .ok_or_else(not_found)?;
    let encrypted = photo.photo_encrypted.as_deref().ok_or_else(not_found)?;
    let bytes = thumbnail::decrypt(state.thumbnail_key(), encrypted)?;
    let entry = match_audit(
        "search_photo.view",
        &volunteer,
        &m,
        ("search", &m.search_id),
    );
    audit_now(&state, entry).await?;
    let content_type = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else {
        "image/jpeg"
    };
    Ok((
        [(CONTENT_TYPE, content_type), (CACHE_CONTROL, "no-store")],
        bytes,
    )
        .into_response())
}

/// The audit action of a decision.
fn decision_action(decision: Decision) -> &'static str {
    match decision {
        Decision::Accept => "match.accept",
        Decision::Decline => "match.decline",
        Decision::NotThePerson => "match.not_the_person",
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionBody {
    decision: Decision,
    decided_by: DecidedBy,
    note: Option<String>,
}

/// `POST /v1/matches/{id}/decision`: `accept | decline | not_the_person`; the first to decide closes
/// it. Reading the match and writing the decision are one transaction, so of two decisions at the
/// same time exactly one wins and the other gets `409 match_already_decided`. Only `accept`
/// returns the family's contact. `decline` and `not_the_person` leave the search active; the
/// closed match stays in place, so the same person is never matched to that search again.
pub async fn post_decision(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(match_id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let body: DecisionBody =
        serde_json::from_value(body).map_err(|err| invalid_request(err.to_string()))?;
    let note = clean_note(body.note)?;
    let (decision, decided_by) = (body.decision, body.decided_by);
    let family = store::transact(&state.db, |db, tx| {
        let (match_id, volunteer, note) = (match_id.clone(), volunteer.clone(), note.clone());
        Box::pin(async move {
            let m = load_match(&db, &match_id).await?;
            let m = decide(&volunteer, m, decision, decided_by, note, store::now())?;
            let family = if decision.is_accept() {
                let search: SearchDoc = store::get(&db, searches::COLLECTION, &m.search_id)
                    .await?
                    .ok_or_else(match_not_found)?;
                Some(search.family)
            } else {
                None
            };
            tx_replace(&db, tx, COLLECTION, &match_id, &m)?;
            let entry = match_audit(
                decision_action(decision),
                &volunteer,
                &m,
                ("match", &match_id),
            );
            audit::add(&db, tx, &entry)?;
            // The family's photos go when the person is found: a match accepted and none open.
            // A decline keeps them: the search stays active for other people (spec 4.8).
            if search_photos::found(&db, &m.search_id, Some((&match_id, m.status))).await? {
                if let Some(mut search) =
                    store::get::<SearchDoc>(&db, searches::COLLECTION, &m.search_id).await?
                {
                    search_photos::discard(&db, tx, &m.search_id, &mut search, "found")?;
                }
            }
            Ok(family)
        })
    })
    .await?;
    Ok(Json(match family {
        Some(family) => json!({ "whatsapp_link": whatsapp_link(&family), "family": family }),
        None => json!({}),
    }))
}
