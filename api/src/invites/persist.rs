//! Reads and writes of the invite routes. Each transaction reads what it needs, applies the
//! rules of `domain` and writes the result with its audit entry.

use axum::http::StatusCode;
use firestore::{FirestoreDb, path};
use serde::Deserialize;
use tokio::task::JoinSet;

use super::model::{COLLECTION, InviteDoc, InviteStatus, forbidden, invite_invalid};
use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{
    self, AppState, Purpose, SESSIONS, SessionDoc, Volunteer, consumed, new_session, session_id,
    volunteer_for_login,
};
use crate::error::ApiError;
use crate::members::{self, MembershipDoc, ShelterRole, Via, membership_id};
use crate::shelters::{self, ShelterDoc};
use crate::store::{self, fs_error, tx_create, tx_replace};
use crate::volunteers::{self, ProfileInput, VolunteerDoc};

/// Profiles of the people who joined by link that the admin's list reads at the same time.
const PROFILE_READS_AT_ONCE: usize = 20;

pub(super) async fn create(
    state: &AppState,
    code: &str,
    invite: &InviteDoc,
    by: &str,
) -> Result<(), ApiError> {
    store::transact(&state.db, |db, tx| {
        let (code, invite, by) = (code.to_string(), invite.clone(), by.to_string());

        Box::pin(async move {
            tx_create(&db, tx, COLLECTION, &code, &invite)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new(super::ACTION_CREATE, "invite", &code)
                    .basis(LegalBasis::FraudPrevention)
                    .by(&by)
                    .in_shelter(&invite.shelter_id),
            )
        })
    })
    .await
}

/// An invite with its code, as the admin's list reads it.
#[derive(Deserialize)]
pub(super) struct Listed {
    #[serde(alias = "_firestore_id")]
    pub(super) id: String,
    #[serde(flatten)]
    pub(super) invite: InviteDoc,
}

/// The invites of the shelter that can still be accepted, oldest first.
pub(super) async fn open_in_shelter(
    state: &AppState,
    shelter_id: &str,
) -> Result<Vec<Listed>, ApiError> {
    let now = store::now();
    let mut listed: Vec<Listed> = state
        .db
        .fluent()
        .select()
        .from(COLLECTION)
        .filter(|q| {
            q.for_all([
                q.field(path!(InviteDoc::shelter_id)).eq(shelter_id),
                q.field(path!(InviteDoc::status)).eq(InviteStatus::Active),
            ])
        })
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    listed.retain(|l| l.invite.is_valid(now));
    listed.sort_by_key(|l| l.invite.created_at);

    Ok(listed)
}

/// The members of the shelter who came in by link, with the name of each profile.
pub(super) async fn joined_by_link(
    state: &AppState,
    shelter_id: &str,
) -> Result<Vec<(MembershipDoc, Option<String>)>, ApiError> {
    let by_link: Vec<MembershipDoc> = members::active_in_shelter(&state.db, shelter_id)
        .await?
        .into_iter()
        .filter(|m| m.via == Via::Link)
        .collect();
    let mut joined = Vec::with_capacity(by_link.len());
    for chunk in by_link.chunks(PROFILE_READS_AT_ONCE) {
        let mut reads = JoinSet::new();
        for (position, member) in chunk.iter().enumerate() {
            let (db, id) = (state.db.clone(), member.volunteer_id.clone());
            reads.spawn(async move {
                (
                    position,
                    store::get::<VolunteerDoc>(&db, volunteers::COLLECTION, &id).await,
                )
            });
        }
        let mut names = vec![None; chunk.len()];
        while let Some(done) = reads.join_next().await {
            let (position, profile) = done.map_err(|_| ApiError::internal())?;
            names[position] = profile?.map(|p| p.name);
        }

        for (member, name) in chunk.iter().zip(names) {
            joined.push((member.clone(), name));
        }
    }
    Ok(joined)
}

pub(super) async fn cancel(
    state: &AppState,
    volunteer: &Volunteer,
    code: &str,
) -> Result<String, ApiError> {
    store::transact(&state.db, |db, tx| {
        let (code, volunteer) = (code.to_string(), volunteer.clone());

        Box::pin(async move {
            let mut invite = store::get::<InviteDoc>(&db, COLLECTION, &code)
                .await?
                .filter(|i| i.is_valid(store::now()))
                .ok_or_else(invite_invalid)?;
            if !volunteer.is_admin_of(&invite.shelter_id) {
                return Err(forbidden());
            }
            crate::events::ensure_shelter_open(&db, &invite.shelter_id).await?;
            invite.cancel(store::now())?;
            tx_replace(&db, tx, COLLECTION, &code, &invite)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new(super::ACTION_CANCEL, "invite", &code)
                    .basis(LegalBasis::FraudPrevention)
                    .by(&volunteer.id)
                    .in_shelter(&invite.shelter_id),
            )?;
            Ok(invite.shelter_id)
        })
    })
    .await
}

/// The invite if it can still be accepted.
pub(super) async fn live_invite(db: &FirestoreDb, code: &str) -> Result<InviteDoc, ApiError> {
    store::get::<InviteDoc>(db, COLLECTION, code)
        .await?
        .filter(|i| i.is_valid(store::now()))
        .ok_or_else(invite_invalid)
}

/// The invite and its shelter, when the invite can still be accepted.
pub(super) async fn usable(
    db: &FirestoreDb,
    code: &str,
) -> Result<Option<(InviteDoc, ShelterDoc)>, ApiError> {
    let invite: Option<InviteDoc> = store::get(db, COLLECTION, code).await?;

    let usable = match invite.filter(|i| i.is_valid(store::now())) {
        Some(invite) => store::get::<ShelterDoc>(db, shelters::COLLECTION, &invite.shelter_id)
            .await?
            .map(|shelter| (invite, shelter)),
        None => None,
    };

    Ok(usable)
}

#[derive(Clone)]
pub(super) struct Accept {
    pub(super) code: String,
    pub(super) capture_id: String,
    pub(super) device_id: String,
    pub(super) person_id: String,
    pub(super) profile: Option<ProfileInput>,
    pub(super) secret: String,
    pub(super) session_days: u32,
}

pub(super) struct Accepted {
    pub(super) volunteer: VolunteerDoc,
    pub(super) session: SessionDoc,
    pub(super) shelter_id: String,
    pub(super) shelter: ShelterDoc,
    pub(super) role: ShelterRole,
}

/// One transaction: use the invite, consume the capture, create or load the volunteer, add the
/// membership and open the session. Every check reads inside the transaction, so of two
/// people racing for one invite exactly one gets in.
pub(super) async fn accept_in(
    db: &FirestoreDb,
    tx: &mut firestore::FirestoreTransaction<'_>,
    a: &Accept,
) -> Result<Accepted, ApiError> {
    let now = store::now();
    let mut invite = store::get::<InviteDoc>(db, COLLECTION, &a.code)
        .await?
        .filter(|i| i.is_valid(now))
        .ok_or_else(invite_invalid)?;
    let capture: auth::CaptureDoc = store::get(db, "captures", &a.capture_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "capture_not_found"))?;
    if capture.consumed {
        return Err(ApiError::new(StatusCode::CONFLICT, "capture_consumed"));
    }
    if capture.purpose != Purpose::Invite {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "wrong_capture_purpose",
        ));
    }
    let existing: Option<VolunteerDoc> =
        store::get(db, volunteers::COLLECTION, &a.person_id).await?;
    let (volunteer, is_new) = volunteer_for_login(existing, a.profile.as_ref(), now)?;

    let previous = store::get::<MembershipDoc>(
        db,
        members::COLLECTION,
        &membership_id(&invite.shelter_id, &a.person_id),
    )
    .await?;
    invite.admits(previous.map(|m| m.status))?;
    let shelter: ShelterDoc = store::get(db, shelters::COLLECTION, &invite.shelter_id)
        .await?
        .ok_or_else(invite_invalid)?;

    let membership = invite.membership(&a.code, &a.person_id, now);
    let session = new_session(&a.person_id, &a.device_id, a.session_days, now);
    invite.redeem(&a.person_id, now)?;

    let membership_key = membership_id(&invite.shelter_id, &a.person_id);
    tx_replace(db, tx, volunteers::COLLECTION, &a.person_id, &volunteer)?;
    tx_replace(
        db,
        tx,
        "captures",
        &a.capture_id,
        &consumed(capture, &a.person_id),
    )?;
    tx_replace(db, tx, COLLECTION, &a.code, &invite)?;
    tx_replace(db, tx, members::COLLECTION, &membership_key, &membership)?;
    tx_create(db, tx, SESSIONS, &session_id(&a.secret), &session)?;
    let entry = |action: &str, kind: &str, target: &str| {
        AuditEntry::new(action, kind, target)
            .by(&a.person_id)
            .on_device(&a.device_id)
            .in_shelter(&invite.shelter_id)
            .basis(LegalBasis::FraudPrevention)
    };
    if is_new {
        audit::add(
            db,
            tx,
            &entry("volunteer.create", "volunteer", &a.person_id),
        )?;
    }
    audit::add(
        db,
        tx,
        &entry("session.create", "session", &session_id(&a.secret)),
    )?;
    audit::add(db, tx, &entry(super::ACTION_ACCEPT, "invite", &a.code))?;
    Ok(Accepted {
        volunteer,
        session,
        shelter_id: invite.shelter_id,
        shelter,
        role: invite.role,
    })
}

pub(super) async fn accept(state: &AppState, args: &Accept) -> Result<Accepted, ApiError> {
    store::transact(&state.db, |db, tx| {
        let args = args.clone();
        Box::pin(async move { accept_in(&db, tx, &args).await })
    })
    .await
}
