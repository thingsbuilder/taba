//! Invites to a shelter (spec 3.3, 8.4): a QR code for someone present (30 min, admin or
//! volunteer) or a link for someone remote (24 h, volunteer only). Single use, made on the
//! server, and always accepted with a liveness capture (`purpose = invite`).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use firestore::jiff::SignedDuration;
use firestore::{FirestoreDb, FirestoreInstant, path};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{
    self, AppState, CaptureStatus, Purpose, SESSIONS, SessionDoc, Volunteer, consumed, new_session,
    resolve_person, session_id, usable_capture, volunteer_for_login,
};
use crate::error::ApiError;
use crate::members::{self, MembershipDoc, MembershipStatus, ShelterRole, Via, membership_id};
use crate::shelters::{self, ShelterDoc};
use crate::store::{self, fs_error, tx_create, tx_replace};
use crate::volunteers::{self, ProfileInput, VolunteerDoc};

pub const COLLECTION: &str = "invites";
const QR_TTL: SignedDuration = SignedDuration::from_mins(30);
const LINK_TTL: SignedDuration = SignedDuration::from_hours(24);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InviteKind {
    Qr,
    Link,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InviteStatus {
    Active,
    Used,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteDoc {
    pub shelter_id: String,
    pub kind: InviteKind,
    pub role: ShelterRole,
    pub created_by: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub expires_at: FirestoreInstant,
    pub status: InviteStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_by: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "firestore::serialize_as_optional_timestamp"
    )]
    pub used_at: Option<FirestoreInstant>,
}

impl InviteDoc {
    /// Whether the invite can still be accepted: not used, not canceled, not expired.
    pub fn is_valid(&self, now: FirestoreInstant) -> bool {
        self.status == InviteStatus::Active && now < self.expires_at
    }

    fn via(&self) -> Via {
        match self.kind {
            InviteKind::Qr => Via::Qr,
            InviteKind::Link => Via::Link,
        }
    }
}

/// 128 random bits, base64url (spec 7).
fn new_code() -> String {
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn url(state: &AppState, code: &str) -> String {
    format!("https://{}/invite/{code}", state.domain())
}

fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

fn invite_invalid() -> ApiError {
    ApiError::new(StatusCode::GONE, "invite_invalid")
}

// ---- POST /v1/shelters/{id}/invites ----

#[derive(Debug, Deserialize)]
pub struct CreateInvite {
    kind: InviteKind,
    role: ShelterRole,
}

pub async fn create_invite(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
    Json(request): Json<CreateInvite>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if !volunteer.is_admin_of(&shelter_id) {
        return Err(forbidden());
    }
    crate::events::ensure_shelter_open(&state.db, &shelter_id).await?;
    if request.kind == InviteKind::Link && request.role == ShelterRole::Admin {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "link_volunteer_only",
        ));
    }
    let now = store::now();
    let ttl = match request.kind {
        InviteKind::Qr => QR_TTL,
        InviteKind::Link => LINK_TTL,
    };
    let code = new_code();
    let invite = InviteDoc {
        shelter_id: shelter_id.clone(),
        kind: request.kind,
        role: request.role,
        created_by: volunteer.id.clone(),
        created_at: now,
        expires_at: now + ttl,
        status: InviteStatus::Active,
        used_by: None,
        used_at: None,
    };
    store::transact(&state.db, |db, tx| {
        let (code, invite, by) = (code.clone(), invite.clone(), volunteer.id.clone());
        Box::pin(async move {
            tx_create(&db, tx, COLLECTION, &code, &invite)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new("invite.create", "invite", &code)
                    .basis(LegalBasis::FraudPrevention)
                    .by(&by)
                    .in_shelter(&invite.shelter_id),
            )
        })
    })
    .await?;
    members::touch_shelter_event(&state, &shelter_id).await;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({ "code": code, "url": url(&state, &code), "expires_at": invite.expires_at.to_string() }),
        ),
    ))
}

// ---- GET /v1/shelters/{id}/invites ----

pub async fn list_invites(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !volunteer.is_admin_of(&shelter_id) {
        return Err(forbidden());
    }
    #[derive(Deserialize)]
    struct Listed {
        #[serde(alias = "_firestore_id")]
        id: String,
        #[serde(flatten)]
        invite: InviteDoc,
    }
    let now = store::now();
    let mut listed: Vec<Listed> = state
        .db
        .fluent()
        .select()
        .from(COLLECTION)
        .filter(|q| {
            q.for_all([
                q.field(path!(InviteDoc::shelter_id))
                    .eq(shelter_id.as_str()),
                q.field(path!(InviteDoc::status)).eq(InviteStatus::Active),
            ])
        })
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    listed.retain(|l| l.invite.is_valid(now));
    listed.sort_by_key(|l| l.invite.created_at);
    let invites: Vec<Value> = listed
        .iter()
        .map(|l| {
            json!({
                "code": l.id,
                "url": url(&state, &l.id),
                "kind": l.invite.kind,
                "role": l.invite.role,
                "created_at": l.invite.created_at.to_string(),
                "expires_at": l.invite.expires_at.to_string(),
            })
        })
        .collect();

    let mut joined_by_link = Vec::new();
    for member in members::active_in_shelter(&state.db, &shelter_id).await? {
        if member.via != Via::Link {
            continue;
        }
        let profile: Option<VolunteerDoc> =
            store::get(&state.db, volunteers::COLLECTION, &member.volunteer_id).await?;
        joined_by_link.push(json!({
            "volunteer_id": member.volunteer_id,
            "name": profile.map(|p| p.name),
            "invite_code": member.invite_code,
            "joined_at": member.joined_at.to_string(),
        }));
    }
    Ok(Json(
        json!({ "invites": invites, "joined_by_link": joined_by_link }),
    ))
}

// ---- DELETE /v1/invites/{code} ----

pub async fn cancel_invite(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(code): Path<String>,
) -> Result<StatusCode, ApiError> {
    let shelter_id = store::transact(&state.db, |db, tx| {
        let (code, volunteer) = (code.clone(), volunteer.clone());
        Box::pin(async move {
            let mut invite = store::get::<InviteDoc>(&db, COLLECTION, &code)
                .await?
                .filter(|i| i.is_valid(store::now()))
                .ok_or_else(invite_invalid)?;
            if !volunteer.is_admin_of(&invite.shelter_id) {
                return Err(forbidden());
            }
            crate::events::ensure_shelter_open(&db, &invite.shelter_id).await?;
            invite.status = InviteStatus::Canceled;
            tx_replace(&db, tx, COLLECTION, &code, &invite)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new("invite.cancel", "invite", &code)
                    .basis(LegalBasis::FraudPrevention)
                    .by(&volunteer.id)
                    .in_shelter(&invite.shelter_id),
            )?;
            Ok(invite.shelter_id)
        })
    })
    .await?;
    members::touch_shelter_event(&state, &shelter_id).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- GET /v1/invites/{code} ----

/// What the invited person sees before the selfie. An invite that cannot be used says so
/// and nothing else.
pub async fn get_invite(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let invite: Option<InviteDoc> = store::get(&state.db, COLLECTION, &code).await?;
    let usable = match invite.filter(|i| i.is_valid(store::now())) {
        Some(invite) => {
            store::get::<ShelterDoc>(&state.db, shelters::COLLECTION, &invite.shelter_id)
                .await?
                .map(|shelter| (invite, shelter))
        }
        None => None,
    };
    Ok(Json(match usable {
        Some((invite, shelter)) => json!({
            "valid": true,
            "shelter_name": shelter.name,
            "role": invite.role,
            "kind": invite.kind,
            "confirmed": shelter.confirmation == shelters::Confirmation::Confirmed,
        }),
        None => {
            json!({ "valid": false, "shelter_name": null, "role": null, "kind": null, "confirmed": null })
        }
    }))
}

// ---- POST /v1/invites/{code}/accept ----

#[derive(Debug, Deserialize)]
pub struct AcceptInvite {
    capture_id: String,
    #[serde(default)]
    profile: Option<ProfileInput>,
}

#[derive(Clone)]
struct Accept {
    code: String,
    capture_id: String,
    device_id: String,
    person_id: String,
    profile: Option<ProfileInput>,
    secret: String,
    session_days: u32,
}

struct Accepted {
    volunteer: VolunteerDoc,
    session: SessionDoc,
    shelter_id: String,
    shelter: ShelterDoc,
    role: ShelterRole,
}

/// One transaction: use the invite, consume the capture, create or load the volunteer, add the
/// membership and open the session. Every check reads inside the transaction, so of two
/// people racing for one invite exactly one gets in.
async fn accept_in(
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
    match previous.as_ref().map(|m| (m.status, invite.kind)) {
        Some((MembershipStatus::Active, _)) => {
            return Err(ApiError::new(StatusCode::CONFLICT, "already_member"));
        }
        // Someone removed from a shelter comes back only in person (spec 3.3.7).
        Some((MembershipStatus::Removed, InviteKind::Link)) => {
            return Err(ApiError::new(StatusCode::FORBIDDEN, "removed_from_shelter"));
        }
        _ => {}
    }
    let shelter: ShelterDoc = store::get(db, shelters::COLLECTION, &invite.shelter_id)
        .await?
        .ok_or_else(invite_invalid)?;

    let membership = MembershipDoc {
        shelter_id: invite.shelter_id.clone(),
        volunteer_id: a.person_id.clone(),
        role: invite.role,
        status: MembershipStatus::Active,
        via: invite.via(),
        invite_code: Some(a.code.clone()),
        invited_by: Some(invite.created_by.clone()),
        joined_at: now,
        removed_by: None,
        removed_at: None,
    };
    let session = new_session(&a.person_id, &a.device_id, a.session_days, now);
    invite.status = InviteStatus::Used;
    invite.used_by = Some(a.person_id.clone());
    invite.used_at = Some(now);

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
    audit::add(db, tx, &entry("invite.accept", "invite", &a.code))?;
    Ok(Accepted {
        volunteer,
        session,
        shelter_id: invite.shelter_id,
        shelter,
        role: invite.role,
    })
}

pub async fn accept_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
    Json(request): Json<AcceptInvite>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let device = auth::device_id(&headers)?;
    // A dead invite is refused before the capture is looked at, so it costs nothing and
    // leaves the capture unused.
    let invite = store::get::<InviteDoc>(&state.db, COLLECTION, &code)
        .await?
        .filter(|i| i.is_valid(store::now()))
        .ok_or_else(invite_invalid)?;
    crate::events::ensure_shelter_open(&state.db, &invite.shelter_id).await?;
    let mut capture = usable_capture(&state, &device, &request.capture_id, Purpose::Invite).await?;
    let person_id = resolve_person(&state, &request.capture_id, &mut capture).await?;

    let args = Accept {
        code,
        capture_id: request.capture_id,
        device_id: device,
        person_id: person_id.clone(),
        profile: request.profile,
        secret: auth::new_secret(),
        session_days: state.config.session_days,
    };
    let accepted = store::transact(&state.db, |db, tx| {
        let args = args.clone();
        Box::pin(async move { accept_in(&db, tx, &args).await })
    })
    .await;
    let accepted = match accepted {
        Ok(accepted) => accepted,
        // A new volunteer still owes the profile: keep what Unico said, it is not asked again.
        Err(e) if e.code == "profile_required" => {
            capture.status = CaptureStatus::Ok;
            capture.person_id = Some(person_id);
            auth::remember_result(&state, &args.capture_id, &capture).await?;
            return Err(e);
        }
        Err(e) => return Err(e),
    };
    members::touch_shelter_event(&state, &accepted.shelter_id).await;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "session": args.secret,
            "expires_at": accepted.session.expires_at.to_string(),
            "volunteer": volunteers::view(&person_id, &accepted.volunteer),
            "membership": {
                "shelter_id": accepted.shelter_id,
                "name": accepted.shelter.name,
                "role": accepted.role,
                "confirmation": accepted.shelter.confirmation,
            },
        })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, audit_actions, new_id};
    use crate::volunteers::GlobalRole;

    fn error_of(body: &Value) -> Option<&str> {
        body["error"].as_str()
    }

    async fn invite_doc(app: &TestApp, code: &str) -> InviteDoc {
        store::get(&app.state.db, COLLECTION, code)
            .await
            .unwrap()
            .unwrap()
    }

    /// Moves the invite's expiry `minutes` into the past (or future, if negative).
    async fn expired_for(app: &TestApp, code: &str, minutes: i64) {
        let mut invite = invite_doc(app, code).await;
        invite.expires_at = store::now() - SignedDuration::from_mins(minutes);
        store::replace(&app.state.db, COLLECTION, code, &invite)
            .await
            .unwrap();
    }

    async fn setup() -> (TestApp, Actor, String) {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.create_shelter(&admin, "Ginásio", ORIGIN).await;
        (app, admin, shelter)
    }

    // ---- creating ----

    #[tokio::test]
    async fn qr_invites_last_30_minutes_and_links_24_hours() {
        let (app, admin, shelter) = setup().await;
        for (kind, role, minutes) in [
            ("qr", "volunteer", 30),
            ("qr", "admin", 30),
            ("link", "volunteer", 24 * 60),
        ] {
            let (status, body) = app
                .send(
                    "POST",
                    &format!("/v1/shelters/{shelter}/invites"),
                    &admin.secret,
                    Some(json!({ "kind": kind, "role": role })),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{kind} {role}: {body}");
            let code = body["code"].as_str().unwrap();
            assert_eq!(
                body["url"],
                json!(format!("https://taba.example.org/invite/{code}"))
            );
            // 128 random bits in base64url: 22 characters.
            assert_eq!(code.len(), 22);
            let invite = invite_doc(&app, code).await;
            assert_eq!(
                (
                    invite.kind,
                    invite.role,
                    invite.status,
                    invite.created_by.as_str(),
                    invite.shelter_id.as_str()
                ),
                (
                    if kind == "qr" {
                        InviteKind::Qr
                    } else {
                        InviteKind::Link
                    },
                    if role == "admin" {
                        ShelterRole::Admin
                    } else {
                        ShelterRole::Volunteer
                    },
                    InviteStatus::Active,
                    admin.id.as_str(),
                    shelter.as_str()
                )
            );
            assert_eq!(
                invite.expires_at.as_second() - invite.created_at.as_second(),
                minutes * 60
            );
            assert_eq!(body["expires_at"], json!(invite.expires_at.to_string()));
        }
        assert_eq!(
            audit_actions(&app.state.db)
                .await
                .iter()
                .filter(|a| *a == "invite.create")
                .count(),
            3
        );
    }

    #[tokio::test]
    async fn a_link_cannot_make_an_admin() {
        let (app, admin, shelter) = setup().await;
        let (status, body) = app
            .send(
                "POST",
                &format!("/v1/shelters/{shelter}/invites"),
                &admin.secret,
                Some(json!({ "kind": "link", "role": "admin" })),
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("link_volunteer_only")
            )
        );
        assert!(all_docs(&app.state.db, COLLECTION).await.is_empty());
    }

    #[tokio::test]
    async fn only_an_admin_of_the_shelter_invites() {
        let (app, admin, shelter) = setup().await;
        let volunteer = app
            .join(
                &app.invite(&admin, &shelter, "qr", "volunteer").await,
                "Vol",
            )
            .await;
        let stranger = app.actor("Ze").await;
        let civil_defense = app.actor("DC").await;
        app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let other_admin = app.actor("Other").await;
        app.create_shelter(
            &other_admin,
            "Outro",
            crate::test_support::north_of_origin(5_000.0),
        )
        .await;
        for who in [&volunteer, &stranger, &civil_defense, &other_admin] {
            let (status, _) = app
                .send(
                    "POST",
                    &format!("/v1/shelters/{shelter}/invites"),
                    &who.secret,
                    Some(json!({ "kind": "qr", "role": "volunteer" })),
                )
                .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{}", who.id);
        }
        let (status, _, _) = app
            .request(
                "POST",
                &format!("/v1/shelters/{shelter}/invites"),
                &[],
                Some(json!({ "kind": "qr", "role": "volunteer" })),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // ---- looking ----

    #[tokio::test]
    async fn the_invite_page_shows_the_shelter_only_while_the_invite_works() {
        let (app, admin, shelter) = setup().await;
        let code = app.invite(&admin, &shelter, "qr", "admin").await;
        let (status, _, seen) = app
            .request("GET", &format!("/v1/invites/{code}"), &[], None)
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            seen,
            json!({ "valid": true, "shelter_name": "Ginásio", "role": "admin", "kind": "qr", "confirmed": false })
        );

        let unusable = json!({ "valid": false, "shelter_name": null, "role": null, "kind": null, "confirmed": null });
        let (_, _, unknown) = app.request("GET", "/v1/invites/nope", &[], None).await;
        assert_eq!(unknown, unusable);
        expired_for(&app, &code, 1).await;
        let (_, _, expired) = app
            .request("GET", &format!("/v1/invites/{code}"), &[], None)
            .await;
        assert_eq!(expired, unusable);
        let link = app.invite(&admin, &shelter, "link", "volunteer").await;
        app.send(
            "DELETE",
            &format!("/v1/invites/{link}"),
            &admin.secret,
            None,
        )
        .await;
        let (_, _, canceled) = app
            .request("GET", &format!("/v1/invites/{link}"), &[], None)
            .await;
        assert_eq!(canceled, unusable);
    }

    #[tokio::test]
    async fn the_invite_page_says_when_the_shelter_is_confirmed() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.confirmed_shelter(&admin, "Ginásio", ORIGIN).await;
        let code = app.invite(&admin, &shelter, "link", "volunteer").await;
        let (_, _, seen) = app
            .request("GET", &format!("/v1/invites/{code}"), &[], None)
            .await;
        assert_eq!(
            (&seen["confirmed"], &seen["kind"]),
            (&json!(true), &json!("link"))
        );
    }

    #[tokio::test]
    async fn the_admin_lists_active_invites_and_who_came_in_by_link() {
        let (app, admin, shelter) = setup().await;
        let qr = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let link_used = app.invite(&admin, &shelter, "link", "volunteer").await;
        let link_open = app.invite(&admin, &shelter, "link", "volunteer").await;
        let canceled = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let expired = app.invite(&admin, &shelter, "qr", "volunteer").await;
        app.send(
            "DELETE",
            &format!("/v1/invites/{canceled}"),
            &admin.secret,
            None,
        )
        .await;
        expired_for(&app, &expired, 1).await;
        let joiner = app.join(&link_used, "Bia").await;

        let (status, listing) = app
            .send(
                "GET",
                &format!("/v1/shelters/{shelter}/invites"),
                &admin.secret,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{listing}");
        let mut codes: Vec<&str> = listing["invites"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["code"].as_str().unwrap())
            .collect();
        codes.sort();
        let mut want = vec![qr.as_str(), link_open.as_str()];
        want.sort();
        assert_eq!(codes, want);
        let first = &listing["invites"][0];
        assert!(
            first["url"]
                .as_str()
                .unwrap()
                .starts_with("https://taba.example.org/invite/")
        );
        assert!(
            first["kind"].is_string()
                && first["role"].is_string()
                && first["expires_at"].is_string()
                && first["created_at"].is_string()
        );
        assert_eq!(
            listing["joined_by_link"],
            json!([{ "volunteer_id": joiner.id, "name": "Bia", "invite_code": link_used, "joined_at": listing["joined_by_link"][0]["joined_at"] }])
        );
        // A QR does not show up there, and a volunteer cannot list.
        let by_qr = app.join(&qr, "Caio").await;
        let (_, listing) = app
            .send(
                "GET",
                &format!("/v1/shelters/{shelter}/invites"),
                &admin.secret,
                None,
            )
            .await;
        assert_eq!(listing["joined_by_link"].as_array().unwrap().len(), 1);
        assert_eq!(
            app.send(
                "GET",
                &format!("/v1/shelters/{shelter}/invites"),
                &by_qr.secret,
                None
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }

    // ---- cancelling ----

    #[tokio::test]
    async fn an_admin_cancels_and_a_cancelled_invite_cannot_be_used() {
        let (app, admin, shelter) = setup().await;
        let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let stranger = app.actor("Ze").await;
        assert_eq!(
            app.send(
                "DELETE",
                &format!("/v1/invites/{code}"),
                &stranger.secret,
                None
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.send(
                "DELETE",
                &format!("/v1/invites/{code}"),
                &admin.secret,
                None
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(invite_doc(&app, &code).await.status, InviteStatus::Canceled);
        let (status, body) = app.accept(&code, "dev-bia", "tok-bia", "Bia").await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::GONE, Some("invite_invalid"))
        );
        // Cancelling it again, or something that never existed, is a 410.
        for gone in [code.as_str(), "nope"] {
            let (status, body) = app
                .send(
                    "DELETE",
                    &format!("/v1/invites/{gone}"),
                    &admin.secret,
                    None,
                )
                .await;
            assert_eq!(
                (status, error_of(&body)),
                (StatusCode::GONE, Some("invite_invalid"))
            );
        }
    }

    // ---- accepting ----

    #[tokio::test]
    async fn accepting_makes_a_member_a_session_and_uses_the_invite() {
        let (app, admin, shelter) = setup().await;
        let code = app.invite(&admin, &shelter, "qr", "admin").await;
        let (status, body) = app.accept(&code, "dev-bia", "tok-bia", "Bia").await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(
            body["membership"],
            json!({ "shelter_id": shelter, "name": "Ginásio", "role": "admin", "confirmation": "unconfirmed" })
        );
        assert_eq!(body["volunteer"]["name"], "Bia");
        assert!(body["expires_at"].is_string());

        // The session works and shows the shelter.
        let secret = body["session"].as_str().unwrap();
        let (_, me) = app.get_authed("/v1/me", secret).await;
        assert_eq!(me["memberships"][0]["role"], "admin");

        let invite = invite_doc(&app, &code).await;
        assert_eq!(
            (invite.status, invite.used_by.as_deref()),
            (InviteStatus::Used, body["volunteer"]["id"].as_str())
        );
        assert!(invite.used_at.is_some());
        let member: MembershipDoc = store::get(
            &app.state.db,
            members::COLLECTION,
            &membership_id(&shelter, body["volunteer"]["id"].as_str().unwrap()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            (
                member.role,
                member.status,
                member.via,
                member.invite_code.as_deref(),
                member.invited_by.as_deref()
            ),
            (
                ShelterRole::Admin,
                MembershipStatus::Active,
                Via::Qr,
                Some(code.as_str()),
                Some(admin.id.as_str())
            )
        );
        let actions = audit_actions(&app.state.db).await;
        for expected in ["invite.accept", "session.create", "volunteer.create"] {
            assert!(
                actions.contains(&expected.to_string()),
                "{expected} in {actions:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_new_person_must_give_a_profile_and_keeps_the_capture_meanwhile() {
        let (app, admin, shelter) = setup().await;
        let code = app.invite(&admin, &shelter, "link", "volunteer").await;
        let capture_id = app.invite_capture("dev-bia", "tok-bia").await;
        let path = format!("/v1/invites/{code}/accept");
        let (status, body) = app
            .post(&path, "dev-bia", json!({ "capture_id": capture_id }))
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("profile_required"))
        );
        assert_eq!(invite_doc(&app, &code).await.status, InviteStatus::Active);

        let asked = app.unico.get_count();
        let profile = json!({ "name": "Bia", "show_phone": false, "language": "en" });
        let (status, body) = app
            .post(
                &path,
                "dev-bia",
                json!({ "capture_id": capture_id, "profile": profile }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(
            app.unico.get_count(),
            asked,
            "Unico is not asked again for a finished capture"
        );
        assert_eq!(body["volunteer"]["language"], "en");
    }

    #[tokio::test]
    async fn someone_who_already_has_a_profile_only_does_the_selfie() {
        let (app, admin, shelter) = setup().await;
        let bia = app.actor("Bia").await;
        let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let capture_id = app.invite_capture(&bia.device, &bia.token).await;
        let (status, body) = app
            .post(
                &format!("/v1/invites/{code}/accept"),
                &bia.device,
                json!({ "capture_id": capture_id }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["volunteer"]["id"], json!(bia.id));
        // Her earlier session still works, and she is now in the shelter.
        let (_, me) = app.get_authed("/v1/me", &bia.secret).await;
        assert_eq!(me["memberships"][0]["shelter_id"], json!(shelter));
    }

    #[tokio::test]
    async fn a_qr_used_twice_is_gone_the_second_time() {
        let (app, admin, shelter) = setup().await;
        let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let (first, _) = app.accept(&code, "dev-bia", "tok-bia", "Bia").await;
        assert_eq!(first, StatusCode::CREATED);
        let capture_id = app.invite_capture("dev-caio", "tok-caio").await;
        let profile = json!({ "name": "Caio", "show_phone": false, "language": "pt-BR" });
        let (status, body) = app
            .post(
                &format!("/v1/invites/{code}/accept"),
                "dev-caio",
                json!({ "capture_id": capture_id, "profile": profile }),
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::GONE, Some("invite_invalid"))
        );
        // The refused person keeps an unused capture and no profile.
        let capture: crate::auth::CaptureDoc = store::get(&app.state.db, "captures", &capture_id)
            .await
            .unwrap()
            .unwrap();
        assert!(!capture.consumed);
        assert_eq!(
            all_docs(&app.state.db, "volunteers").await.len(),
            2,
            "the admin and Bia"
        );
    }

    #[tokio::test]
    async fn invites_expire_after_30_minutes_for_a_qr_and_24_hours_for_a_link() {
        let (app, admin, shelter) = setup().await;
        let qr = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let link = app.invite(&admin, &shelter, "link", "volunteer").await;
        // 29 minutes in, the QR still works; 31 minutes in it is gone.
        let fresh = app.invite(&admin, &shelter, "qr", "volunteer").await;
        expired_for(&app, &fresh, -1).await;
        assert_eq!(
            app.accept(&fresh, "dev-ana", "tok-ana", "Ana").await.0,
            StatusCode::CREATED
        );
        expired_for(&app, &qr, 1).await;
        let (status, body) = app.accept(&qr, "dev-bia", "tok-bia", "Bia").await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::GONE, Some("invite_invalid"))
        );
        expired_for(&app, &link, 1).await;
        assert_eq!(
            app.accept(&link, "dev-caio", "tok-caio", "Caio").await.0,
            StatusCode::GONE
        );
    }

    #[tokio::test]
    async fn of_many_people_racing_for_one_qr_exactly_one_gets_in() {
        let (app, admin, shelter) = setup().await;
        let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let mut handles = Vec::new();
        for n in 0..6 {
            let (app, code) = (app.clone(), code.clone());
            handles.push(tokio::spawn(async move {
                let (device, token) = (format!("dev-{n}"), format!("tok-{n}"));
                app.accept(&code, &device, &token, &format!("Pessoa {n}"))
                    .await
            }));
        }
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.unwrap());
        }
        let joined = results
            .iter()
            .filter(|r| r.0 == StatusCode::CREATED)
            .count();
        let gone = results.iter().filter(|r| r.0 == StatusCode::GONE).count();
        assert_eq!((joined, gone), (1, 5), "{results:?}");
        assert_eq!(
            members::active_in_shelter(&app.state.db, &shelter)
                .await
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            all_docs(&app.state.db, "sessions").await.len(),
            2,
            "the admin's and the winner's"
        );
    }

    #[tokio::test]
    async fn the_same_person_racing_with_two_captures_gets_one_membership() {
        let (app, admin, shelter) = setup().await;
        let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let (a, b) = tokio::join!(
            app.accept(&code, "dev-bia", "tok-bia", "Bia"),
            app.accept(&code, "dev-bia2", "tok-bia", "Bia"),
        );
        let mut statuses = [a.0, b.0];
        statuses.sort();
        assert_eq!(statuses, [StatusCode::CREATED, StatusCode::GONE]);
    }

    #[tokio::test]
    async fn someone_already_in_the_shelter_is_told_so_and_the_invite_stays() {
        let (app, admin, shelter) = setup().await;
        let first = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let bia = app.join(&first, "Bia").await;
        let second = app.invite(&admin, &shelter, "qr", "admin").await;
        let capture_id = app.invite_capture(&bia.device, &bia.token).await;
        let (status, body) = app
            .post(
                &format!("/v1/invites/{second}/accept"),
                &bia.device,
                json!({ "capture_id": capture_id }),
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::CONFLICT, Some("already_member"))
        );
        assert_eq!(invite_doc(&app, &second).await.status, InviteStatus::Active);
        // The admin themself too: the creator is a member already.
        let capture_id = app.invite_capture(&admin.device, &admin.token).await;
        let (status, _) = app
            .post(
                &format!("/v1/invites/{second}/accept"),
                &admin.device,
                json!({ "capture_id": capture_id }),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn a_banned_person_cannot_accept_and_the_invite_is_not_spent() {
        let (app, admin, shelter) = setup().await;
        let bia = app.actor("Bia").await;
        let civil_defense = app.actor("DC").await;
        app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let (status, _) = app
            .send(
                "POST",
                &format!("/v1/volunteers/{}/ban", bia.id),
                &civil_defense.secret,
                Some(json!({ "reason": "fraude" })),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let capture_id = app.invite_capture(&bia.device, &bia.token).await;
        let (status, body) = app
            .post(
                &format!("/v1/invites/{code}/accept"),
                &bia.device,
                json!({ "capture_id": capture_id }),
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::FORBIDDEN, Some("banned"))
        );
        assert_eq!(invite_doc(&app, &code).await.status, InviteStatus::Active);
        assert_eq!(
            members::active_in_shelter(&app.state.db, &shelter)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn someone_removed_returns_by_qr_but_not_by_link() {
        let (app, admin, shelter) = setup().await;
        let first = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let bia = app.join(&first, "Bia").await;
        let (status, _) = app
            .send(
                "DELETE",
                &format!("/v1/shelters/{shelter}/members/{}", bia.id),
                &admin.secret,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let link = app.invite(&admin, &shelter, "link", "volunteer").await;
        let capture_id = app.invite_capture(&bia.device, &bia.token).await;
        let (status, body) = app
            .post(
                &format!("/v1/invites/{link}/accept"),
                &bia.device,
                json!({ "capture_id": capture_id }),
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::FORBIDDEN, Some("removed_from_shelter"))
        );
        assert_eq!(
            invite_doc(&app, &link).await.status,
            InviteStatus::Active,
            "the link is not spent"
        );

        // A QR in person brings her back, this time as an admin if the QR says so.
        let qr = app.invite(&admin, &shelter, "qr", "admin").await;
        let capture_id = app.invite_capture(&bia.device, &bia.token).await;
        let (status, body) = app
            .post(
                &format!("/v1/invites/{qr}/accept"),
                &bia.device,
                json!({ "capture_id": capture_id }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let member: MembershipDoc = store::get(
            &app.state.db,
            members::COLLECTION,
            &membership_id(&shelter, &bia.id),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            (
                member.status,
                member.role,
                member.removed_by,
                member.removed_at
            ),
            (MembershipStatus::Active, ShelterRole::Admin, None, None)
        );
    }

    #[tokio::test]
    async fn someone_who_left_can_return_by_link() {
        let (app, admin, shelter) = setup().await;
        let first = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let bia = app.join(&first, "Bia").await;
        assert_eq!(
            app.send(
                "POST",
                &format!("/v1/shelters/{shelter}/leave"),
                &bia.secret,
                None
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        let link = app.invite(&admin, &shelter, "link", "volunteer").await;
        let capture_id = app.invite_capture(&bia.device, &bia.token).await;
        let (status, _) = app
            .post(
                &format!("/v1/invites/{link}/accept"),
                &bia.device,
                json!({ "capture_id": capture_id }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn the_capture_must_be_an_unused_invite_capture_of_this_device() {
        let (app, admin, shelter) = setup().await;
        let code = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let path = format!("/v1/invites/{code}/accept");
        let profile = json!({ "name": "Bia", "show_phone": false, "language": "pt-BR" });

        // A login capture does not do.
        let (_, login_capture) = app
            .post("/v1/captures", "dev-bia", json!({ "purpose": "login" }))
            .await;
        let login_id = login_capture["capture_id"].as_str().unwrap();
        let (status, body) = app
            .post(
                &path,
                "dev-bia",
                json!({ "capture_id": login_id, "profile": profile }),
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("wrong_capture_purpose")
            )
        );

        // Another device's does not do either, nor an unknown one, nor a missing device header.
        let capture_id = app.invite_capture("dev-bia", "tok-bia").await;
        let (status, body) = app
            .post(
                &path,
                "dev-other",
                json!({ "capture_id": capture_id, "profile": profile }),
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::FORBIDDEN, Some("capture_wrong_device"))
        );
        let (status, body) = app
            .post(
                &path,
                "dev-bia",
                json!({ "capture_id": new_id(), "profile": profile }),
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::NOT_FOUND, Some("capture_not_found"))
        );
        let (status, _, body) = app
            .request(
                "POST",
                &path,
                &[],
                Some(json!({ "capture_id": capture_id })),
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::BAD_REQUEST, Some("device_id_required"))
        );

        // Still pending at Unico: the app tries again later.
        let pending = app
            .post("/v1/captures", "dev-caio", json!({ "purpose": "invite" }))
            .await
            .1;
        let (status, body) = app
            .post(
                &path,
                "dev-caio",
                json!({ "capture_id": pending["capture_id"], "profile": profile }),
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::ACCEPTED, Some("capture_pending"))
        );

        // And once used, it is used.
        let (status, _) = app
            .post(
                &path,
                "dev-bia",
                json!({ "capture_id": capture_id, "profile": profile }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
        let second = app.invite(&admin, &shelter, "qr", "volunteer").await;
        let (status, body) = app
            .post(
                &format!("/v1/invites/{second}/accept"),
                "dev-bia",
                json!({ "capture_id": capture_id }),
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::CONFLICT, Some("capture_consumed"))
        );
    }

    #[tokio::test]
    async fn an_invite_capture_cannot_open_a_session_by_login() {
        let app = TestApp::new().await;
        let capture_id = app.invite_capture("dev-bia", "tok-bia").await;
        let profile = json!({ "name": "Bia", "show_phone": false, "language": "pt-BR" });
        let (status, body) = app
            .post(
                "/v1/sessions",
                "dev-bia",
                json!({ "capture_id": capture_id, "profile": profile }),
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("wrong_capture_purpose")
            )
        );
        assert!(all_docs(&app.state.db, "volunteers").await.is_empty());
    }
}
