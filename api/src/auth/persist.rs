//! Reads and writes of the login routes and of the `Volunteer` extractor. Each transaction reads
//! what it needs, applies the rules of `domain` and writes the result with its audit entry.

use axum::http::StatusCode;
use firestore::errors::BackoffError;
use firestore::{FirestoreDb, FirestoreTransaction, FirestoreWritePrecondition, paths};

use super::domain::{
    MAX_LOGIN_ATTEMPTS, RENEW_AFTER, consumed, days, derive_id, new_session, session_id,
    terminal_error, volunteer_for_login,
};
use super::model::{
    AppState, CAPTURES, CaptureDoc, CaptureStatus, Purpose, SESSIONS, SessionDoc, Volunteer,
    unauthorized,
};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::error::ApiError;
use crate::members;
use crate::store::{self, fs_error, tx_create, tx_replace};
use crate::unico::ProcessResult;
use crate::volunteers::{self, ProfileInput, VolunteerDoc};

pub(super) async fn create_capture(
    state: &AppState,
    id: &str,
    doc: &CaptureDoc,
    entry: &AuditEntry,
) -> Result<(), ApiError> {
    state
        .db
        .run_transaction(|db, tx| {
            let (id, doc, entry) = (id.to_string(), doc.clone(), entry.clone());

            Box::pin(async move {
                let mut write = || -> Result<(), ApiError> {
                    tx_create(&db, tx, CAPTURES, &id, &doc)?;
                    audit::add(&db, tx, &entry)
                };
                write().map_err(BackoffError::permanent)
            })
        })
        .await
        .map_err(fs_error)
}

/// Gets the person behind a capture, asking Unico only while the capture is unresolved.
pub async fn resolve_person(
    state: &AppState,
    capture_id: &str,
    capture: &mut CaptureDoc,
) -> Result<String, ApiError> {
    if let Some(error) = terminal_error(capture.status) {
        return Err(error);
    }
    if let (CaptureStatus::Ok, Some(person_id)) = (capture.status, &capture.person_id) {
        return Ok(person_id.clone());
    }
    let (status, person_id) = match state.unico.get_process(&capture.process_id).await? {
        ProcessResult::Pending => {
            return Err(ApiError::new(StatusCode::ACCEPTED, "capture_pending"));
        }
        ProcessResult::Token(token) => {
            (CaptureStatus::Ok, Some(derive_id(&state.hmac_key, &token)))
        }
        ProcessResult::Inconclusive => (CaptureStatus::Inconclusive, None),
        ProcessResult::Failed => (CaptureStatus::Failed, None),
        ProcessResult::Canceled => (CaptureStatus::Canceled, None),
    };
    capture.status = status;
    capture.person_id = person_id.clone();
    // A token is not written here: the login transaction records it together with `consumed`.
    if status == CaptureStatus::Ok {
        return person_id.ok_or_else(ApiError::internal);
    }
    remember_result(state, capture_id, capture).await?;
    Err(terminal_error(status).unwrap_or_else(ApiError::internal))
}

/// Saves what Unico said about the capture (only `status` and `person_id`: a whole-document
/// write could undo a concurrent `consumed = true`).
pub async fn remember_result(
    state: &AppState,
    capture_id: &str,
    capture: &CaptureDoc,
) -> Result<(), ApiError> {
    state
        .db
        .fluent()
        .update()
        .fields(paths!(CaptureDoc::{status, person_id}))
        .in_col(CAPTURES)
        .document_id(capture_id)
        .object(capture)
        .execute::<()>()
        .await
        .map_err(fs_error)
}

#[derive(Clone)]
pub(super) struct Login {
    pub(super) capture_id: String,
    pub(super) device_id: String,
    pub(super) person_id: String,
    pub(super) profile: Option<ProfileInput>,
    pub(super) secret: String,
    pub(super) session_days: u32,
}

/// The capture as it stands before Unico is asked: it must exist, belong to this device and
/// purpose, and be unused and unexpired (spec 5.2.4).
pub async fn usable_capture(
    state: &AppState,
    device: &str,
    capture_id: &str,
    purpose: Purpose,
) -> Result<CaptureDoc, ApiError> {
    let capture: CaptureDoc = store::get(&state.db, CAPTURES, capture_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "capture_not_found"))?;
    if capture.device_id != device {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "capture_wrong_device"));
    }
    if capture.purpose != purpose {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "wrong_capture_purpose",
        ));
    }
    if capture.consumed {
        return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
    }
    if crate::store::now() >= capture.expires_at {
        return Err(ApiError::new(StatusCode::GONE, "capture_expired"));
    }
    Ok(capture)
}

/// One transaction: consume the capture, create or load the volunteer, open the session.
pub(super) async fn login(
    db: &FirestoreDb,
    tx: &mut FirestoreTransaction<'_>,
    a: &Login,
) -> Result<(VolunteerDoc, SessionDoc), ApiError> {
    let capture: CaptureDoc = store::get(db, CAPTURES, &a.capture_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "capture_not_found"))?;
    if capture.consumed {
        return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
    }
    if capture.purpose != Purpose::Login {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "wrong_capture_purpose",
        ));
    }
    let existing: Option<VolunteerDoc> =
        store::get(db, volunteers::COLLECTION, &a.person_id).await?;
    let now = crate::store::now();
    let (volunteer, is_new) = volunteer_for_login(existing, a.profile.as_ref(), now)?;
    let session = new_session(&a.person_id, &a.device_id, a.session_days, now);

    tx_replace(db, tx, volunteers::COLLECTION, &a.person_id, &volunteer)?;
    tx_replace(
        db,
        tx,
        CAPTURES,
        &a.capture_id,
        &consumed(capture, &a.person_id),
    )?;
    tx_create(db, tx, SESSIONS, &session_id(&a.secret), &session)?;
    let audit_entry = |action: &str, kind: &str, target: &str| {
        AuditEntry::new(action, kind, target)
            .by(&a.person_id)
            .on_device(&a.device_id)
            .basis(LegalBasis::FraudPrevention)
    };
    if is_new {
        audit::add(
            db,
            tx,
            &audit_entry(super::ACTION_VOLUNTEER_CREATE, "volunteer", &a.person_id),
        )?;
    }
    audit::add(
        db,
        tx,
        &audit_entry(
            super::ACTION_SESSION_CREATE,
            "session",
            &session_id(&a.secret),
        ),
    )?;
    Ok((volunteer, session))
}

/// Opens the session of a capture that Unico resolved to `person_id`.
pub(super) async fn open_session(
    state: &AppState,
    args: &Login,
    capture: &mut CaptureDoc,
    person_id: &str,
) -> Result<(VolunteerDoc, SessionDoc), ApiError> {
    // Under contention Firestore aborts the transactions that lose the lock. The capture is
    // single use, so each loser looks again: consumed by the winner means 409, otherwise retry.
    let mut attempts = 0;
    let (volunteer, session) = loop {
        let logged_in = state
            .db
            .run_transaction(|db, tx| {
                let args = args.clone();
                Box::pin(
                    async move { login(&db, tx, &args).await.map_err(BackoffError::permanent) },
                )
            })
            .await
            .map_err(fs_error);
        match logged_in {
            Err(busy) if busy.code == store::CONTENTION => {
                let capture =
                    store::get::<CaptureDoc>(&state.db, CAPTURES, &args.capture_id).await?;
                if capture.is_some_and(|c| c.consumed) {
                    return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
                }
                attempts += 1;
                if attempts == MAX_LOGIN_ATTEMPTS {
                    return Err(busy);
                }
            }
            // A new volunteer still owes the profile: keep the result, Unico is not asked again.
            Err(e) if e.code == "profile_required" => {
                capture.status = CaptureStatus::Ok;
                capture.person_id = Some(person_id.to_string());
                remember_result(state, &args.capture_id, capture).await?;
                return Err(e);
            }
            other => break other?,
        }
    };

    Ok((volunteer, session))
}

pub(super) async fn delete_session(
    state: &AppState,
    session_id: &str,
    entry: &AuditEntry,
) -> Result<(), ApiError> {
    state
        .db
        .run_transaction(|db, tx| {
            let (session_id, entry) = (session_id.to_string(), entry.clone());

            Box::pin(async move {
                let mut write = || -> Result<(), ApiError> {
                    store::tx_delete(&db, tx, SESSIONS, &session_id)?;
                    audit::add(&db, tx, &entry)
                };
                write().map_err(BackoffError::permanent)
            })
        })
        .await
        .map_err(fs_error)
}

/// The volunteer behind a session secret; renews the session when it is a day old.
pub(super) async fn authenticate(state: &AppState, secret: &str) -> Result<Volunteer, ApiError> {
    let session_id = session_id(secret);
    let mut session: SessionDoc = store::get(&state.db, SESSIONS, &session_id)
        .await?
        .ok_or_else(unauthorized)?;
    let now = crate::store::now();
    // The Firestore TTL deletes expired sessions some time after `expires_at`.
    if now >= session.expires_at {
        return Err(unauthorized());
    }
    let doc: VolunteerDoc = store::get(&state.db, volunteers::COLLECTION, &session.volunteer_id)
        .await?
        .ok_or_else(unauthorized)?;
    if doc.banned {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "banned"));
    }

    if now.duration_since(session.renewed_at) > RENEW_AFTER {
        session.renewed_at = now;
        session.expires_at = now + days(state.config.session_days);
        // Only the two dates, and only if the session still exists: a logout must not be undone.
        let renewed = state
            .db
            .fluent()
            .update()
            .fields(paths!(SessionDoc::{renewed_at, expires_at}))
            .in_col(SESSIONS)
            .precondition(FirestoreWritePrecondition::Exists(true))
            .document_id(&session_id)
            .object(&session)
            .execute::<()>()
            .await;
        if let Err(err) = renewed {
            tracing::warn!(%err, "could not renew the session");
        }
    }
    let memberships = members::memberships_of(&state.db, &session.volunteer_id).await?;
    Ok(Volunteer {
        id: session.volunteer_id,
        doc,
        memberships,
        session_id,
    })
}
