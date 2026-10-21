//! In-person endorsement (spec 3.9, 8.3): the admin of a new shelter shows a code; an admin of
//! a confirmed shelter that may endorse reads it, within `SHELTER_RADIUS_M` of the first phone,
//! and confirms the new shelter.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use firestore::FirestoreInstant;
use firestore::jiff::SignedDuration;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::{self, AuditEntry};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::geo;
use crate::members;
use crate::shelters::{
    self, Confirmation, ConfirmationType, Endorsement, EndorsementKind, ShelterDoc, valid_position,
};
use crate::store::{self, tx_create, tx_replace};

pub const COLLECTION: &str = "endorsement_codes";
const CODE_TTL: SignedDuration = SignedDuration::from_mins(10);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndorsementCodeDoc {
    pub shelter_id: String,
    pub created_by: String,
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: f64,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub expires_at: FirestoreInstant,
    pub used: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Position {
    lat: f64,
    lon: f64,
    accuracy: f64,
}

fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

fn already_confirmed() -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "already_confirmed")
}

// ---- POST /v1/shelters/{id}/endorsements/in-person/code ----

pub async fn create_code(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
    Json(position): Json<Position>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if !volunteer.is_admin_of(&shelter_id) {
        return Err(forbidden());
    }
    valid_position(position.lat, position.lon, position.accuracy)?;
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes);
    let code = URL_SAFE_NO_PAD.encode(bytes);
    let doc = EndorsementCodeDoc {
        shelter_id: shelter_id.clone(),
        created_by: volunteer.id.clone(),
        lat: position.lat,
        lon: position.lon,
        accuracy_m: position.accuracy,
        expires_at: store::now() + CODE_TTL,
        used: false,
    };
    store::transact(&state.db, |db, tx| {
        let (shelter_id, code, doc, by) = (
            shelter_id.clone(),
            code.clone(),
            doc.clone(),
            volunteer.id.clone(),
        );
        Box::pin(async move {
            let shelter: ShelterDoc = store::get(&db, shelters::COLLECTION, &shelter_id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found"))?;
            if shelter.confirmation == Confirmation::Confirmed {
                return Err(already_confirmed());
            }
            tx_create(&db, tx, COLLECTION, &code, &doc)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new("endorsement.code", "shelter", &shelter_id)
                    .by(&by)
                    .in_shelter(&shelter_id),
            )
        })
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "code": code,
            "url": format!("https://{}/endorse/{code}", state.domain()),
            "expires_at": doc.expires_at.to_string(),
        })),
    ))
}

// ---- POST /v1/endorsements/in-person ----

#[derive(Debug, Clone, Deserialize)]
pub struct Endorse {
    code: String,
    endorser_shelter_id: String,
    #[serde(flatten)]
    position: Position,
}

fn too_far(distance_m: f64) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "too_far");
    error.detail = json!({ "distance_m": distance_m.round() });
    error
}

pub async fn endorse(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Json(request): Json<Endorse>,
) -> Result<Json<Value>, ApiError> {
    let radius_m = f64::from(state.config.shelter_radius_m);
    let (shelter_id, shelter) = store::transact(&state.db, |db, tx| {
        let (request, volunteer) = (request.clone(), volunteer.clone());
        Box::pin(async move {
            let mut code = store::get::<EndorsementCodeDoc>(&db, COLLECTION, &request.code)
                .await?
                .filter(|c| !c.used && store::now() < c.expires_at)
                .ok_or_else(|| ApiError::new(StatusCode::GONE, "endorsement_code_invalid"))?;
            let mut shelter: ShelterDoc = store::get(&db, shelters::COLLECTION, &code.shelter_id)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::GONE, "endorsement_code_invalid"))?;
            if shelter.confirmation == Confirmation::Confirmed {
                return Err(already_confirmed());
            }

            // The endorser: an admin of a confirmed shelter that may endorse, and not a member
            // of the new one (spec 3.9).
            let cannot = || ApiError::new(StatusCode::FORBIDDEN, "cannot_endorse");
            if !volunteer.is_admin_of(&request.endorser_shelter_id) {
                return Err(cannot());
            }
            let endorser_shelter: ShelterDoc =
                store::get(&db, shelters::COLLECTION, &request.endorser_shelter_id)
                    .await?
                    .ok_or_else(cannot)?;
            if endorser_shelter.confirmation != Confirmation::Confirmed
                || !endorser_shelter.can_endorse
            {
                return Err(cannot());
            }
            let member_of_new = store::get::<members::MembershipDoc>(
                &db,
                members::COLLECTION,
                &members::membership_id(&code.shelter_id, &volunteer.id),
            )
            .await?
            .is_some_and(|m| m.status == members::MembershipStatus::Active);
            if member_of_new {
                return Err(cannot());
            }

            // Both phones must know where they are, and be close (spec 3.9.3).
            let position = &request.position;
            valid_position(position.lat, position.lon, position.accuracy)?;
            let distance_m = geo::haversine_m(code.lat, code.lon, position.lat, position.lon);
            if distance_m > radius_m {
                return Err(too_far(distance_m));
            }

            shelter.confirm(ConfirmationType::InPerson);
            shelter.endorsements.push(Endorsement {
                kind: EndorsementKind::InPerson,
                volunteer_id: volunteer.id.clone(),
                endorser_shelter_id: request.endorser_shelter_id.clone(),
                at: store::now(),
                distance_m,
            });
            code.used = true;
            let shelter_id = code.shelter_id.clone();
            tx_replace(&db, tx, shelters::COLLECTION, &shelter_id, &shelter)?;
            tx_replace(&db, tx, COLLECTION, &request.code, &code)?;
            audit::add(
                &db,
                tx,
                &AuditEntry::new("endorsement.create", "shelter", &shelter_id)
                    .by(&volunteer.id)
                    .in_shelter(&shelter_id),
            )?;
            Ok((shelter_id, shelter))
        })
    })
    .await?;
    crate::events::touch(&state, shelter.event_id.as_deref()).await;
    Ok(Json(
        json!({ "shelter": shelters::view_for(&state.db, &volunteer, &shelter_id, &shelter).await? }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, audit_actions, north_of_origin};
    use crate::volunteers::GlobalRole;

    fn error_of(body: &Value) -> Option<&str> {
        body["error"].as_str()
    }

    /// A confirmed shelter with its admin, and a new unconfirmed one far from it: only the two
    /// phones need to be close, not the shelters.
    struct Scene {
        app: TestApp,
        endorser: Actor,
        endorser_shelter: String,
        newcomer: Actor,
        new_shelter: String,
    }

    async fn scene() -> Scene {
        let app = TestApp::new().await;
        let endorser = app.actor("Endorser").await;
        let endorser_shelter = app.confirmed_shelter(&endorser, "Confirmado", ORIGIN).await;
        let newcomer = app.actor("Newcomer").await;
        let new_shelter = app
            .create_shelter(&newcomer, "Novo", north_of_origin(20_000.0))
            .await;
        Scene {
            app,
            endorser,
            endorser_shelter,
            newcomer,
            new_shelter,
        }
    }

    async fn code_for(s: &Scene, at: (f64, f64), accuracy: f64) -> (StatusCode, Value) {
        s.app
            .send(
                "POST",
                &format!("/v1/shelters/{}/endorsements/in-person/code", s.new_shelter),
                &s.newcomer.secret,
                Some(json!({ "lat": at.0, "lon": at.1, "accuracy": accuracy })),
            )
            .await
    }

    async fn endorse_with(
        s: &Scene,
        who: &Actor,
        code: &str,
        from_shelter: &str,
        at: (f64, f64),
        accuracy: f64,
    ) -> (StatusCode, Value) {
        s.app
            .send(
                "POST",
                "/v1/endorsements/in-person",
                &who.secret,
                Some(json!({ "code": code, "endorser_shelter_id": from_shelter, "lat": at.0, "lon": at.1, "accuracy": accuracy })),
            )
            .await
    }

    /// Both phones in the same spot.
    fn spot() -> (f64, f64) {
        north_of_origin(20_000.0)
    }

    async fn shelter_of(s: &Scene, id: &str) -> crate::shelters::ShelterDoc {
        store::get(&s.app.state.db, shelters::COLLECTION, id)
            .await
            .unwrap()
            .unwrap()
    }

    // ---- the code ----

    #[tokio::test]
    async fn the_admin_gets_a_ten_minute_code_with_a_url_for_the_qr() {
        let s = scene().await;
        let (status, body) = code_for(&s, spot(), 12.0).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let code = body["code"].as_str().unwrap();
        assert_eq!(code.len(), 22);
        assert_eq!(
            body["url"],
            json!(format!("https://taba.example.org/endorse/{code}"))
        );
        let stored: EndorsementCodeDoc = store::get(&s.app.state.db, COLLECTION, code)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                stored.shelter_id.as_str(),
                stored.created_by.as_str(),
                stored.used,
                stored.accuracy_m
            ),
            (s.new_shelter.as_str(), s.newcomer.id.as_str(), false, 12.0)
        );
        assert!((stored.expires_at.as_second() - store::now().as_second() - 600).abs() <= 2);
        assert!(
            audit_actions(&s.app.state.db)
                .await
                .contains(&"endorsement.code".to_string())
        );
    }

    #[tokio::test]
    async fn the_code_needs_an_admin_a_good_position_and_an_unconfirmed_shelter() {
        let s = scene().await;
        let volunteer = s
            .app
            .join(
                &s.app
                    .invite(&s.newcomer, &s.new_shelter, "qr", "volunteer")
                    .await,
                "Vol",
            )
            .await;
        let path = format!("/v1/shelters/{}/endorsements/in-person/code", s.new_shelter);
        let position = Some(json!({ "lat": spot().0, "lon": spot().1, "accuracy": 10.0 }));
        for who in [&volunteer, &s.endorser] {
            assert_eq!(
                s.app
                    .send("POST", &path, &who.secret, position.clone())
                    .await
                    .0,
                StatusCode::FORBIDDEN
            );
        }
        let (status, body) = code_for(&s, spot(), 101.0).await;
        assert_eq!(
            (status, error_of(&body)),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("insufficient_accuracy")
            )
        );
        let (status, body) = code_for(&s, (95.0, 0.0), 10.0).await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_shelter"))
        );
        assert!(all_docs(&s.app.state.db, COLLECTION).await.is_empty());
        // A confirmed shelter has nothing left to endorse.
        let confirmed = s
            .app
            .confirmed_shelter(&s.newcomer, "Outro", north_of_origin(90_000.0))
            .await;
        let (status, body) = s
            .app
            .send(
                "POST",
                &format!("/v1/shelters/{confirmed}/endorsements/in-person/code"),
                &s.newcomer.secret,
                position,
            )
            .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::CONFLICT, Some("already_confirmed"))
        );
    }

    // ---- endorsing ----

    #[tokio::test]
    async fn an_admin_of_a_confirmed_shelter_endorses_with_phones_side_by_side() {
        let s = scene().await;
        let (_, code) = code_for(&s, spot(), 10.0).await;
        let code = code["code"].as_str().unwrap();
        let near = north_of_origin(20_030.0);
        let (status, body) =
            endorse_with(&s, &s.endorser, code, &s.endorser_shelter, near, 15.0).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        // The endorser is not a member of the new shelter, so sees the basics: now confirmed.
        assert_eq!(
            (&body["shelter"]["id"], &body["shelter"]["confirmation"]),
            (&json!(s.new_shelter), &json!("confirmed"))
        );

        let shelter = shelter_of(&s, &s.new_shelter).await;
        assert_eq!(
            (
                shelter.confirmation,
                shelter.confirmation_type,
                shelter.can_endorse
            ),
            (
                Confirmation::Confirmed,
                Some(ConfirmationType::InPerson),
                true
            )
        );
        assert_eq!(shelter.endorsements.len(), 1);
        let endorsement = &shelter.endorsements[0];
        assert_eq!(
            (
                endorsement.volunteer_id.as_str(),
                endorsement.endorser_shelter_id.as_str(),
                endorsement.kind
            ),
            (
                s.endorser.id.as_str(),
                s.endorser_shelter.as_str(),
                EndorsementKind::InPerson
            )
        );
        assert!(
            (endorsement.distance_m - 30.0).abs() < 1.0,
            "{}",
            endorsement.distance_m
        );
        let stored: EndorsementCodeDoc = store::get(&s.app.state.db, COLLECTION, code)
            .await
            .unwrap()
            .unwrap();
        assert!(stored.used);
        assert!(
            audit_actions(&s.app.state.db)
                .await
                .contains(&"endorsement.create".to_string())
        );
        // The newly confirmed shelter can now endorse others.
        let (_, me) = s.app.get_authed("/v1/me", &s.newcomer.secret).await;
        assert_eq!(me["memberships"][0]["confirmation"], "confirmed");
    }

    #[tokio::test]
    async fn phones_150_metres_apart_are_too_far_and_100_is_the_edge() {
        let s = scene().await;
        let (_, code) = code_for(&s, spot(), 10.0).await;
        let code = code["code"].as_str().unwrap();
        let (status, body) = endorse_with(
            &s,
            &s.endorser,
            code,
            &s.endorser_shelter,
            north_of_origin(20_150.0),
            10.0,
        )
        .await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("too_far"))
        );
        assert!((body["detail"]["distance_m"].as_f64().unwrap() - 150.0).abs() <= 1.0);
        assert_eq!(
            shelter_of(&s, &s.new_shelter).await.confirmation,
            Confirmation::Unconfirmed
        );
        assert!(
            !store::get::<EndorsementCodeDoc>(&s.app.state.db, COLLECTION, code)
                .await
                .unwrap()
                .unwrap()
                .used,
            "a failed try does not spend the code"
        );
        let (status, _) = endorse_with(
            &s,
            &s.endorser,
            code,
            &s.endorser_shelter,
            north_of_origin(20_101.0),
            10.0,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, _) = endorse_with(
            &s,
            &s.endorser,
            code,
            &s.endorser_shelter,
            north_of_origin(20_099.0),
            10.0,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn the_endorsers_phone_must_know_where_it_is_within_100_metres() {
        let s = scene().await;
        let (_, code) = code_for(&s, spot(), 10.0).await;
        let code = code["code"].as_str().unwrap();
        let (status, body) =
            endorse_with(&s, &s.endorser, code, &s.endorser_shelter, spot(), 101.0).await;
        assert_eq!(
            (status, error_of(&body)),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("insufficient_accuracy")
            )
        );
        assert_eq!(
            endorse_with(&s, &s.endorser, code, &s.endorser_shelter, spot(), 100.0)
                .await
                .0,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn someone_active_in_the_new_shelter_cannot_endorse_it() {
        let s = scene().await;
        // The endorser is also a volunteer in the new shelter.
        s.app
            .join_again(
                &s.app
                    .invite(&s.newcomer, &s.new_shelter, "qr", "volunteer")
                    .await,
                &s.endorser,
            )
            .await;
        let (_, code) = code_for(&s, spot(), 10.0).await;
        let code = code["code"].as_str().unwrap();
        let (status, body) =
            endorse_with(&s, &s.endorser, code, &s.endorser_shelter, spot(), 10.0).await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::FORBIDDEN, Some("cannot_endorse"))
        );
        // Having been removed from it makes them a stranger to it again.
        s.app
            .send(
                "DELETE",
                &format!("/v1/shelters/{}/members/{}", s.new_shelter, s.endorser.id),
                &s.newcomer.secret,
                None,
            )
            .await;
        assert_eq!(
            endorse_with(&s, &s.endorser, code, &s.endorser_shelter, spot(), 10.0)
                .await
                .0,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn only_an_admin_of_a_confirmed_shelter_that_may_endorse_can() {
        let s = scene().await;
        let (_, code) = code_for(&s, spot(), 10.0).await;
        let code = code["code"].as_str().unwrap();
        // A volunteer of the confirmed shelter.
        let volunteer = s
            .app
            .join(
                &s.app
                    .invite(&s.endorser, &s.endorser_shelter, "qr", "volunteer")
                    .await,
                "Vol",
            )
            .await;
        // An admin of a shelter nobody confirmed.
        let unconfirmed_admin = s.app.actor("Unconfirmed").await;
        let unconfirmed = s
            .app
            .create_shelter(&unconfirmed_admin, "Nao", north_of_origin(80_000.0))
            .await;
        // An admin of a confirmed shelter whose right to endorse was taken away.
        let barred = s.app.actor("Barred").await;
        let barred_shelter = s
            .app
            .confirmed_shelter(&barred, "Barrado", north_of_origin(150_000.0))
            .await;
        let mut doc = shelter_of(&s, &barred_shelter).await;
        doc.can_endorse = false;
        store::replace(&s.app.state.db, shelters::COLLECTION, &barred_shelter, &doc)
            .await
            .unwrap();
        let stranger = s.app.actor("Ze").await;

        let attempts = [
            (&volunteer, s.endorser_shelter.as_str()),
            (&unconfirmed_admin, unconfirmed.as_str()),
            (&barred, barred_shelter.as_str()),
            // Naming a shelter one is not an admin of.
            (&s.endorser, unconfirmed.as_str()),
            (&stranger, s.endorser_shelter.as_str()),
            // An unknown shelter.
            (&s.endorser, "nowhere"),
        ];
        for (who, shelter) in attempts {
            let (status, body) = endorse_with(&s, who, code, shelter, spot(), 10.0).await;
            assert_eq!(
                (status, error_of(&body)),
                (StatusCode::FORBIDDEN, Some("cannot_endorse")),
                "{} via {shelter}",
                who.id
            );
        }
        assert_eq!(
            shelter_of(&s, &s.new_shelter).await.confirmation,
            Confirmation::Unconfirmed
        );
    }

    #[tokio::test]
    async fn a_code_works_once_and_for_ten_minutes() {
        let s = scene().await;
        let (_, first) = code_for(&s, spot(), 10.0).await;
        let first = first["code"].as_str().unwrap().to_string();
        assert_eq!(
            endorse_with(&s, &s.endorser, &first, &s.endorser_shelter, spot(), 10.0)
                .await
                .0,
            StatusCode::OK
        );
        let (status, body) =
            endorse_with(&s, &s.endorser, &first, &s.endorser_shelter, spot(), 10.0).await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::GONE, Some("endorsement_code_invalid"))
        );

        // Unknown codes, and a code 11 minutes old, are the same answer.
        let (status, body) =
            endorse_with(&s, &s.endorser, "nope", &s.endorser_shelter, spot(), 10.0).await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::GONE, Some("endorsement_code_invalid"))
        );
        let other = s
            .app
            .create_shelter(&s.newcomer, "Segundo", north_of_origin(100_000.0))
            .await;
        let (_, code) = s
            .app
            .send(
                "POST",
                &format!("/v1/shelters/{other}/endorsements/in-person/code"),
                &s.newcomer.secret,
                Some(json!({ "lat": 1.0, "lon": 1.0, "accuracy": 5.0 })),
            )
            .await;
        let code = code["code"].as_str().unwrap();
        let mut doc: EndorsementCodeDoc = store::get(&s.app.state.db, COLLECTION, code)
            .await
            .unwrap()
            .unwrap();
        doc.expires_at = store::now() - SignedDuration::from_mins(1);
        store::replace(&s.app.state.db, COLLECTION, code, &doc)
            .await
            .unwrap();
        let (status, _) =
            endorse_with(&s, &s.endorser, code, &s.endorser_shelter, (1.0, 1.0), 5.0).await;
        assert_eq!(status, StatusCode::GONE);
    }

    #[tokio::test]
    async fn an_already_confirmed_shelter_is_not_endorsed_again() {
        let s = scene().await;
        let (_, code) = code_for(&s, spot(), 10.0).await;
        let code = code["code"].as_str().unwrap();
        // A validator confirms it while the code is still open.
        let validator = s.app.actor("Val").await;
        s.app.grant(&validator, GlobalRole::Validator).await;
        s.app
            .send(
                "POST",
                &format!("/v1/shelters/{}/confirm", s.new_shelter),
                &validator.secret,
                None,
            )
            .await;
        let (status, body) =
            endorse_with(&s, &s.endorser, code, &s.endorser_shelter, spot(), 10.0).await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::CONFLICT, Some("already_confirmed"))
        );
        assert!(shelter_of(&s, &s.new_shelter).await.endorsements.is_empty());
    }

    #[tokio::test]
    async fn one_code_read_by_many_confirms_once() {
        let s = scene().await;
        let (_, code) = code_for(&s, spot(), 10.0).await;
        let code = code["code"].as_str().unwrap().to_string();
        let mut handles = Vec::new();
        for n in 0..5 {
            let who = s.app.actor(&format!("Endorser{n}")).await;
            let shelter = s
                .app
                .confirmed_shelter(
                    &who,
                    &format!("Confirmado {n}"),
                    north_of_origin(100_000.0 + 3_000.0 * f64::from(n)),
                )
                .await;
            let (app, code) = (s.app.clone(), code.clone());
            handles.push(tokio::spawn(async move {
                app.send(
                    "POST",
                    "/v1/endorsements/in-person",
                    &who.secret,
                    Some(json!({ "code": code, "endorser_shelter_id": shelter, "lat": spot().0, "lon": spot().1, "accuracy": 10.0 })),
                )
                .await
            }));
        }
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.unwrap());
        }
        let ok = results.iter().filter(|r| r.0 == StatusCode::OK).count();
        let gone = results.iter().filter(|r| r.0 == StatusCode::GONE).count();
        assert_eq!((ok, gone), (1, 4), "{results:?}");
        assert_eq!(shelter_of(&s, &s.new_shelter).await.endorsements.len(), 1);
    }

    // ---- revoking (spec 3.9) ----

    async fn endorsed(s: &Scene) {
        let (_, code) = code_for(s, spot(), 10.0).await;
        let code = code["code"].as_str().unwrap();
        assert_eq!(
            endorse_with(s, &s.endorser, code, &s.endorser_shelter, spot(), 10.0)
                .await
                .0,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn revoking_flags_the_endorser_and_alerts_the_civil_defense_and_validators() {
        let s = scene().await;
        endorsed(&s).await;
        let civil_defense = s.app.actor("DC").await;
        s.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let path = format!("/v1/shelters/{}/revoke", s.new_shelter);
        let (status, body) = s
            .app
            .send(
                "POST",
                &path,
                &civil_defense.secret,
                Some(json!({ "reason": "o local não existe" })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            (
                &body["shelter"]["confirmation"],
                &body["shelter"]["can_endorse"]
            ),
            (&json!("unconfirmed"), &json!(false))
        );

        let endorser: crate::volunteers::VolunteerDoc = store::get(
            &s.app.state.db,
            crate::volunteers::COLLECTION,
            &s.endorser.id,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(endorser.flagged);
        assert_eq!(
            endorser.flag_reasons,
            [format!("endorsed_revoked_shelter:{}", s.new_shelter)]
        );
        let alerts = all_docs(&s.app.state.db, "alerts").await;
        assert_eq!(alerts.len(), 1);
        assert_eq!(
            (
                &alerts[0]["kind"],
                &alerts[0]["target_id"],
                &alerts[0]["shelter_ids"],
                &alerts[0]["volunteer_ids"],
                &alerts[0]["for_civil_defense"]
            ),
            (
                &json!("endorser_flagged"),
                &json!(s.endorser.id),
                &json!([]),
                &json!([s.endorser.id]),
                &json!(true)
            )
        );
        assert_eq!(
            alerts[0]["detail"]["revoked_shelter_id"],
            json!(s.new_shelter)
        );
        // The Civil Defense and the validators see it. The endorser it is about does not (and
        // cannot resolve it), nor do the admins of the new shelter or of the endorser's shelter.
        let validator = s.app.actor("Val").await;
        s.app.grant(&validator, GlobalRole::Validator).await;
        let admin_of_endorser_shelter = s.app.actor("OtherAdmin").await;
        s.app
            .join_again(
                &s.app
                    .invite(&s.endorser, &s.endorser_shelter, "qr", "admin")
                    .await,
                &admin_of_endorser_shelter,
            )
            .await;
        let alert_id = alerts[0]["_firestore_id"].as_str().unwrap().to_string();
        for (who, expected) in [
            (&validator, 1),
            (&civil_defense, 1),
            (&s.endorser, 0),
            (&admin_of_endorser_shelter, 0),
            (&s.newcomer, 0),
        ] {
            let (_, listing) = s.app.send("GET", "/v1/alerts", &who.secret, None).await;
            assert_eq!(
                listing["alerts"].as_array().unwrap().len(),
                expected,
                "{}",
                who.id
            );
            if expected == 0 {
                let path = format!("/v1/alerts/{alert_id}/resolve");
                assert_eq!(
                    s.app.send("POST", &path, &who.secret, None).await.0,
                    StatusCode::FORBIDDEN,
                    "{}",
                    who.id
                );
            }
        }
        // A validator who is the flagged endorser does not see the alert about themself either.
        s.app.grant(&s.endorser, GlobalRole::Validator).await;
        let (_, own) = s
            .app
            .send("GET", "/v1/alerts", &s.endorser.secret, None)
            .await;
        assert!(own["alerts"].as_array().unwrap().is_empty());
        let path = format!("/v1/alerts/{alert_id}/resolve");
        assert_eq!(
            s.app.send("POST", &path, &s.endorser.secret, None).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            s.app.send("POST", &path, &validator.secret, None).await.0,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn revoking_a_shelter_a_validator_confirmed_flags_no_one() {
        let s = scene().await;
        let civil_defense = s.app.actor("DC").await;
        s.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let (status, _) = s
            .app
            .send(
                "POST",
                &format!("/v1/shelters/{}/revoke", s.endorser_shelter),
                &civil_defense.secret,
                Some(json!({ "reason": "x" })),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(all_docs(&s.app.state.db, "alerts").await.is_empty());
    }

    #[tokio::test]
    async fn a_second_revocation_flags_only_the_new_endorsers() {
        let s = scene().await;
        endorsed(&s).await;
        let civil_defense = s.app.actor("DC").await;
        s.app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let path = format!("/v1/shelters/{}/revoke", s.new_shelter);
        s.app
            .send(
                "POST",
                &path,
                &civil_defense.secret,
                Some(json!({ "reason": "um" })),
            )
            .await;

        // Endorsed again by someone else, then revoked again.
        let second = s.app.actor("Second").await;
        let second_shelter = s
            .app
            .confirmed_shelter(&second, "Outro confirmado", north_of_origin(100_000.0))
            .await;
        let (_, code) = code_for(&s, spot(), 10.0).await;
        let code = code["code"].as_str().unwrap();
        assert_eq!(
            endorse_with(&s, &second, code, &second_shelter, spot(), 10.0)
                .await
                .0,
            StatusCode::OK
        );
        s.app
            .send(
                "POST",
                &path,
                &civil_defense.secret,
                Some(json!({ "reason": "dois" })),
            )
            .await;

        let alerts = all_docs(&s.app.state.db, "alerts").await;
        let mut targets: Vec<&str> = alerts
            .iter()
            .map(|a| a["target_id"].as_str().unwrap())
            .collect();
        targets.sort();
        let mut expected = vec![s.endorser.id.as_str(), second.id.as_str()];
        expected.sort();
        assert_eq!(
            targets, expected,
            "one alert for each endorser, none repeated"
        );
        assert_eq!(shelter_of(&s, &s.new_shelter).await.revocations.len(), 2);
    }
}
