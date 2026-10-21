//! Households (spec 3.5.1, 7, 8.5). Owner B.
//!
//! A person is in at most one household: `people.household_id` is the single source of truth,
//! read and written in the same transaction as `households.member_ids` (spec 6.2.4). Reading a
//! household shows names and levels only, never the shelter a member is in nor their history
//! (spec 10.3, criterion 35).

use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use firestore::{FirestoreDb, FirestoreInstant};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::{self, AuditEntry};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::people::{self, PersonDoc};
use crate::store::{self, now, transact, tx_create, tx_replace};

pub const COLLECTION: &str = "households";

const MAX_MEMBERS: usize = 50;
const MAX_PETS: usize = 10;
const MAX_SPECIES_CHARS: usize = 40;
const MAX_PET_COUNT: u32 = 99;
const MAX_ID_CHARS: usize = 128;

/// Companion animals are only a count (spec 3.5.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pet {
    pub species: String,
    pub count: u32,
}

/// `households/{household_id}` (UUID from the client).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HouseholdDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    #[serde(default)]
    pub member_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_id: Option<String>,
    #[serde(default)]
    pub pets: Vec<Pet>,
    /// The household's `active` card. The transaction that issues or cancels a card keeps it in
    /// step, so "one active card per household" is a check on this one document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_code: Option<String>,
    pub created_by: String,
    pub created_shelter_id: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub client_created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub updated_at: FirestoreInstant,
}

// ---- errors and shared helpers (cards.rs uses them too) ----

fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_household");
    error.detail = json!({ "field": field });
    error
}

pub fn forbidden() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden")
}

fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "household_not_found")
}

fn person_not_found(person_id: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::NOT_FOUND, "person_not_found");
    error.detail = json!({ "person_id": person_id });
    error
}

fn in_other_household(person_id: &str, household_id: &str) -> ApiError {
    let mut error = ApiError::person_in_other_household();
    error.detail = json!({ "person_id": person_id, "household_id": household_id });
    error
}

pub async fn load_household(db: &FirestoreDb, id: &str) -> Result<HouseholdDoc, ApiError> {
    store::get(db, COLLECTION, id).await?.ok_or_else(not_found)
}

async fn load_person(db: &FirestoreDb, id: &str) -> Result<PersonDoc, ApiError> {
    store::get(db, people::COLLECTION, id)
        .await?
        .ok_or_else(|| person_not_found(id))
}

/// The members that still have a document, in `member_ids` order.
pub async fn load_members(
    db: &FirestoreDb,
    household: &HouseholdDoc,
) -> Result<Vec<PersonDoc>, ApiError> {
    let mut members = Vec::new();
    for id in &household.member_ids {
        members.extend(store::get::<PersonDoc>(db, people::COLLECTION, id).await?);
    }
    Ok(members)
}

/// The shelter that lets `volunteer` act on a household: one the volunteer belongs to and where
/// some member is now (spec 8.5). Matrix 2.3 gives volunteers and admins of confirmed and
/// unconfirmed shelters the same power here.
pub fn shelter_of_access(volunteer: &Volunteer, members: &[PersonDoc]) -> Option<String> {
    members
        .iter()
        .filter_map(|p| p.current_shelter_id.as_deref())
        .find(|shelter| volunteer.role_in(shelter).is_some())
        .map(str::to_string)
}

/// Name and level only: no shelter, no history (spec 10.3).
pub fn members_view(members: &[PersonDoc]) -> Vec<Value> {
    members
        .iter()
        .map(|p| {
            json!({
                "person_id": p.id,
                "name": p.name,
                "level": p.level,
            })
        })
        .collect()
}

fn view(household: &HouseholdDoc, members: &[PersonDoc]) -> Value {
    json!({
        "id": household.id,
        "members": members_view(members),
        "head_id": household.head_id,
        "pets": household.pets,
        "card_code": household.card_code,
    })
}

// ---- validation ----

fn valid_ids(ids: &[String], field: &str) -> Result<(), ApiError> {
    let mut seen = HashSet::new();
    let ok = ids.len() <= MAX_MEMBERS
        && ids
            .iter()
            .all(|id| valid_id(id) && seen.insert(id.as_str()));
    ok.then_some(()).ok_or_else(|| invalid(field))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().count() <= MAX_ID_CHARS && !id.contains('/')
}

fn valid_pets(pets: &[Pet]) -> Result<Vec<Pet>, ApiError> {
    if pets.len() > MAX_PETS {
        return Err(invalid("pets"));
    }
    pets.iter()
        .map(|pet| {
            let species = pet.species.trim();
            let ok = !species.is_empty()
                && species.chars().count() <= MAX_SPECIES_CHARS
                && (1..=MAX_PET_COUNT).contains(&pet.count);
            ok.then(|| Pet {
                species: species.to_string(),
                count: pet.count,
            })
            .ok_or_else(|| invalid("pets"))
        })
        .collect()
}

// ---- POST /v1/shelters/{id}/households ----

#[derive(Debug, Deserialize)]
pub struct CreateHousehold {
    id: String,
    member_ids: Vec<String>,
    #[serde(default)]
    head_id: Option<String>,
    #[serde(default)]
    pets: Vec<Pet>,
    client_created_at: String,
}

#[derive(Debug, Clone)]
struct NewHousehold {
    id: String,
    member_ids: Vec<String>,
    head_id: Option<String>,
    pets: Vec<Pet>,
    client_created_at: FirestoreInstant,
}

fn validate(request: CreateHousehold) -> Result<NewHousehold, ApiError> {
    uuid::Uuid::parse_str(&request.id).map_err(|_| invalid("id"))?;
    if request.member_ids.is_empty() {
        return Err(invalid("member_ids"));
    }
    valid_ids(&request.member_ids, "member_ids")?;
    if request
        .head_id
        .as_ref()
        .is_some_and(|head| !request.member_ids.contains(head))
    {
        return Err(invalid("head_id"));
    }
    Ok(NewHousehold {
        pets: valid_pets(&request.pets)?,
        client_created_at: request
            .client_created_at
            .parse()
            .map_err(|_| invalid("client_created_at"))?,
        id: request.id,
        member_ids: request.member_ids,
        head_id: request.head_id,
    })
}

pub async fn post_household(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
    Json(request): Json<CreateHousehold>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if volunteer.role_in(&shelter_id).is_none() {
        return Err(forbidden());
    }
    let new = validate(request)?;
    let created = transact(&state.db, |db, tx| {
        let (volunteer, shelter_id, new) = (volunteer.clone(), shelter_id.clone(), new.clone());
        Box::pin(async move {
            // The same request again (a lost response, a retry) gets the same answer.
            if let Some(existing) = store::get::<HouseholdDoc>(&db, COLLECTION, &new.id).await? {
                if existing.created_by != volunteer.id {
                    return Err(ApiError::new(StatusCode::CONFLICT, "household_exists"));
                }
                return Ok(view(&existing, &load_members(&db, &existing).await?));
            }
            let mut members = Vec::new();
            for person_id in &new.member_ids {
                let person = load_person(&db, person_id).await?;
                if let Some(other) = &person.household_id {
                    return Err(in_other_household(person_id, other));
                }
                members.push(person);
            }
            let at = now();
            let household = HouseholdDoc {
                id: Some(new.id.clone()),
                member_ids: new.member_ids.clone(),
                head_id: new.head_id.clone(),
                pets: new.pets.clone(),
                card_code: None,
                created_by: volunteer.id.clone(),
                created_shelter_id: shelter_id.clone(),
                client_created_at: new.client_created_at,
                created_at: at,
                updated_at: at,
            };
            tx_create(&db, tx, COLLECTION, &new.id, &household)?;
            for person in &mut members {
                person.household_id = Some(new.id.clone());
                person.updated_at = at;
                tx_replace(&db, tx, people::COLLECTION, &person_key(person), person)?;
            }
            audit::add(
                &db,
                tx,
                &AuditEntry::new("household.create", "household", &new.id)
                    .by(&volunteer.id)
                    .in_shelter(&shelter_id),
            )?;
            Ok(view(&household, &members))
        })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(json!({ "household": created }))))
}

fn person_key(person: &PersonDoc) -> String {
    person.id.clone().unwrap_or_default()
}

// ---- GET /v1/households/{id} ----

pub async fn get_household(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let household = load_household(&state.db, &id).await?;
    let members = load_members(&state.db, &household).await?;
    shelter_of_access(&volunteer, &members).ok_or_else(forbidden)?;
    Ok(Json(view(&household, &members)))
}

// ---- PATCH /v1/households/{id} ----

#[derive(Debug, Clone, Deserialize)]
pub struct PatchHousehold {
    #[serde(default)]
    add_member_ids: Vec<String>,
    #[serde(default)]
    remove_member_ids: Vec<String>,
    #[serde(default)]
    head_id: Option<String>,
    #[serde(default)]
    pets: Option<Vec<Pet>>,
}

pub async fn patch_household(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(id): Path<String>,
    Json(patch): Json<PatchHousehold>,
) -> Result<Json<Value>, ApiError> {
    valid_ids(&patch.add_member_ids, "add_member_ids")?;
    valid_ids(&patch.remove_member_ids, "remove_member_ids")?;
    if patch
        .add_member_ids
        .iter()
        .any(|id| patch.remove_member_ids.contains(id))
    {
        return Err(invalid("remove_member_ids"));
    }
    let pets = patch.pets.as_deref().map(valid_pets).transpose()?;

    let updated = transact(&state.db, |db, tx| {
        let (volunteer, id, patch, pets) =
            (volunteer.clone(), id.clone(), patch.clone(), pets.clone());
        Box::pin(async move {
            let mut household = load_household(&db, &id).await?;
            let members = load_members(&db, &household).await?;
            let shelter = shelter_of_access(&volunteer, &members).ok_or_else(forbidden)?;

            // People whose `household_id` changes, and the added ones as read.
            let mut changed: Vec<PersonDoc> = Vec::new();
            let mut added: Vec<PersonDoc> = Vec::new();
            for person_id in &patch.remove_member_ids {
                household.member_ids.retain(|m| m != person_id);
                let leaving = members.iter().find(|p| p.id.as_deref() == Some(person_id));
                if let Some(person) = leaving.filter(|p| p.household_id.as_deref() == Some(&id)) {
                    let mut person = person.clone();
                    person.household_id = None;
                    changed.push(person);
                }
            }
            for person_id in &patch.add_member_ids {
                if household.member_ids.contains(person_id) {
                    continue;
                }
                let mut person = load_person(&db, person_id).await?;
                if let Some(other) = person.household_id.as_ref().filter(|h| **h != id) {
                    return Err(in_other_household(person_id, other));
                }
                person.household_id = Some(id.clone());
                household.member_ids.push(person_id.clone());
                added.push(person.clone());
                changed.push(person);
            }
            if household.member_ids.len() > MAX_MEMBERS {
                return Err(invalid("add_member_ids"));
            }
            match &patch.head_id {
                Some(head) if household.member_ids.contains(head) => {
                    household.head_id = Some(head.clone());
                }
                Some(_) => return Err(invalid("head_id")),
                None => {
                    household.head_id = household
                        .head_id
                        .take()
                        .filter(|head| household.member_ids.contains(head));
                }
            }
            if let Some(pets) = pets {
                household.pets = pets;
            }
            let at = now();
            household.updated_at = at;
            tx_replace(&db, tx, COLLECTION, &id, &household)?;
            for person in &mut changed {
                person.updated_at = at;
                tx_replace(&db, tx, people::COLLECTION, &person_key(person), person)?;
            }
            audit::add(
                &db,
                tx,
                &AuditEntry::new("household.update", "household", &id)
                    .by(&volunteer.id)
                    .in_shelter(&shelter),
            )?;
            let now_members: Vec<PersonDoc> = household
                .member_ids
                .iter()
                .filter_map(|m| {
                    members
                        .iter()
                        .chain(&added)
                        .find(|p| p.id.as_deref() == Some(m))
                        .cloned()
                })
                .collect();
            Ok(view(&household, &now_members))
        })
    })
    .await?;
    Ok(Json(updated))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::audit::LegalBasis;
    use crate::people::{Level, Origin};
    use crate::test_support::{Actor, ORIGIN, TestApp, new_id, north_of_origin};
    use crate::volunteers::GlobalRole;

    /// Shelter `a` is unconfirmed, shelter `b` is confirmed (matrix 2.3 columns NC and C).
    pub struct World {
        pub app: TestApp,
        pub a: String,
        pub b: String,
        pub admin_a: Actor,
        pub vol_a: Actor,
        pub admin_b: Actor,
        pub vol_b: Actor,
        pub outsider: Actor,
        pub validator: Actor,
        pub civil_defense: Actor,
    }

    pub async fn world() -> World {
        let app = TestApp::new().await;
        let admin_a = app.actor("AdminA").await;
        let a = app.create_shelter(&admin_a, "Abrigo A", ORIGIN).await;
        let admin_b = app.actor("AdminB").await;
        let b = app
            .confirmed_shelter(&admin_b, "Abrigo B", north_of_origin(200_000.0))
            .await;
        let vol_a = app
            .join(&app.invite(&admin_a, &a, "qr", "volunteer").await, "VolA")
            .await;
        let vol_b = app
            .join(&app.invite(&admin_b, &b, "qr", "volunteer").await, "VolB")
            .await;
        let validator = app.actor("Validator").await;
        app.grant(&validator, GlobalRole::Validator).await;
        let civil_defense = app.actor("CivilDefense").await;
        app.grant(&civil_defense, GlobalRole::CivilDefense).await;
        let outsider = app.actor("Outsider").await;
        World {
            app,
            a,
            b,
            admin_a,
            vol_a,
            admin_b,
            vol_b,
            outsider,
            validator,
            civil_defense,
        }
    }

    /// A person written straight to the store: the people routes are another module's.
    pub async fn person(app: &TestApp, name: &str, shelter: Option<&str>) -> String {
        let id = new_id();
        let at = now();
        let doc = PersonDoc {
            id: None,
            level: Level::NoBiometrics,
            biometrics_declined: true,
            former_ids: vec![],
            name: Some(name.to_string()),
            social_name: None,
            wristband: None,
            birth_year: None,
            is_child: false,
            child_status: None,
            guardians: vec![],
            needs: Default::default(),
            vulnerabilities: Default::default(),
            household_id: None,
            current_shelter_id: shelter.map(str::to_string),
            stays: vec![],
            event_ids: vec![],
            rescue_ids: vec![],
            thumbnail_encrypted: None,
            origin: Origin::Checkin,
            legal_basis: LegalBasis::Consent,
            created_at: at,
            updated_at: at,
            client_edited_at: None,
        };
        store::replace(&app.state.db, people::COLLECTION, &id, &doc)
            .await
            .unwrap();
        id
    }

    pub async fn stored_person(app: &TestApp, id: &str) -> PersonDoc {
        store::get(&app.state.db, people::COLLECTION, id)
            .await
            .unwrap()
            .unwrap()
    }

    pub fn household_body(id: &str, members: &[&str]) -> Value {
        json!({ "id": id, "member_ids": members, "client_created_at": "2026-10-21T10:00:00Z" })
    }

    /// Creates a household in `shelter` as `who`; returns its id.
    pub async fn household(app: &TestApp, who: &Actor, shelter: &str, members: &[&str]) -> String {
        let id = new_id();
        let (status, body) = app
            .send(
                "POST",
                &format!("/v1/shelters/{shelter}/households"),
                &who.secret,
                Some(household_body(&id, members)),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        id
    }

    async fn post(w: &World, who: &Actor, shelter: &str, body: Value) -> (StatusCode, Value) {
        w.app
            .send(
                "POST",
                &format!("/v1/shelters/{shelter}/households"),
                &who.secret,
                Some(body),
            )
            .await
    }

    async fn patch(w: &World, who: &Actor, id: &str, body: Value) -> (StatusCode, Value) {
        w.app
            .send(
                "PATCH",
                &format!("/v1/households/{id}"),
                &who.secret,
                Some(body),
            )
            .await
    }

    async fn get(w: &World, who: &Actor, id: &str) -> (StatusCode, Value) {
        w.app
            .send("GET", &format!("/v1/households/{id}"), &who.secret, None)
            .await
    }

    fn error_of(body: &Value) -> Option<&str> {
        body["error"].as_str()
    }

    #[tokio::test]
    async fn creates_a_household_and_links_the_members() {
        let w = world().await;
        let (ana, beto) = (
            person(&w.app, "Ana", Some(&w.a)).await,
            person(&w.app, "Beto", Some(&w.a)).await,
        );
        let id = new_id();
        let mut body = household_body(&id, &[&ana, &beto]);
        body["head_id"] = json!(ana);
        body["pets"] = json!([{ "species": " cachorro ", "count": 2 }]);
        let (status, created) = post(&w, &w.vol_a, &w.a, body).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["household"]["id"], json!(id));
        for person_id in [&ana, &beto] {
            let stored = stored_person(&w.app, person_id).await;
            assert_eq!(stored.household_id.as_deref(), Some(id.as_str()));
        }
        let (status, view) = get(&w, &w.admin_a, &id).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["head_id"], json!(ana));
        assert_eq!(view["pets"], json!([{ "species": "cachorro", "count": 2 }]));
        assert_eq!(view["members"].as_array().unwrap().len(), 2);
        assert_eq!(view["members"][0]["name"], json!("Ana"));
        assert_eq!(view["members"][0]["level"], json!("no_biometrics"));
        assert_eq!(view["card_code"], Value::Null);
        let actions = crate::test_support::audit_actions(&w.app.state.db).await;
        assert!(actions.contains(&"household.create".to_string()));
    }

    // Criterion 35: no current shelter, no history.
    #[tokio::test]
    async fn reading_a_household_shows_no_shelter_and_no_history() {
        let w = world().await;
        let ana = person(&w.app, "Ana", Some(&w.a)).await;
        let id = household(&w.app, &w.vol_a, &w.a, &[&ana]).await;
        let (_, view) = get(&w, &w.vol_a, &id).await;
        let text = view.to_string();
        for hidden in [w.a.as_str(), "shelter", "stays", "history"] {
            assert!(!text.contains(hidden), "{hidden} in {text}");
        }
        let member = view["members"][0].as_object().unwrap();
        let mut keys: Vec<_> = member.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["level", "name", "person_id"]);
    }

    // Matrix 2.3: household in the own shelter, unconfirmed and confirmed alike.
    #[tokio::test]
    async fn matrix_volunteers_and_admins_of_both_trust_levels_act_in_their_own_shelter() {
        let w = world().await;
        for (who, shelter) in [
            (&w.vol_a, &w.a),
            (&w.admin_a, &w.a),
            (&w.vol_b, &w.b),
            (&w.admin_b, &w.b),
        ] {
            let p = person(&w.app, "P", Some(shelter)).await;
            let id = household(&w.app, who, shelter, &[&p]).await;
            assert_eq!(get(&w, who, &id).await.0, StatusCode::OK);
            let q = person(&w.app, "Q", Some(shelter)).await;
            let (status, body) = patch(&w, who, &id, json!({ "add_member_ids": [q] })).await;
            assert_eq!(status, StatusCode::OK, "{body}");
        }
    }

    #[tokio::test]
    async fn matrix_everyone_else_is_refused() {
        let w = world().await;
        let ana = person(&w.app, "Ana", Some(&w.a)).await;
        let id = household(&w.app, &w.vol_a, &w.a, &[&ana]).await;
        let other = person(&w.app, "Beto", Some(&w.a)).await;
        let refused = [
            &w.outsider,
            &w.validator,
            &w.civil_defense,
            &w.vol_b,
            &w.admin_b,
        ];
        for who in refused {
            let body = household_body(&new_id(), &[&other]);
            let (status, _) = post(&w, who, &w.a, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(get(&w, who, &id).await.0, StatusCode::FORBIDDEN);
            let (status, _) = patch(&w, who, &id, json!({ "pets": [] })).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }
        // Without a session nothing is answered.
        let (status, _, _) = w
            .app
            .request("GET", &format!("/v1/households/{id}"), &[], None)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(stored_person(&w.app, &other).await.household_id, None);
    }

    #[tokio::test]
    async fn a_person_in_another_household_is_a_conflict_and_nothing_is_written() {
        let w = world().await;
        let (ana, beto) = (
            person(&w.app, "Ana", Some(&w.a)).await,
            person(&w.app, "Beto", Some(&w.a)).await,
        );
        let first = household(&w.app, &w.vol_a, &w.a, &[&ana]).await;
        let body = household_body(&new_id(), &[&beto, &ana]);
        let (status, body) = post(&w, &w.vol_a, &w.a, body).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error_of(&body), Some("person_in_other_household"));
        assert_eq!(body["detail"]["household_id"], json!(first));
        assert_eq!(stored_person(&w.app, &beto).await.household_id, None);
        let all = crate::test_support::all_docs(&w.app.state.db, COLLECTION).await;
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn invalid_requests_are_refused_with_the_field() {
        let w = world().await;
        let ana = person(&w.app, "Ana", Some(&w.a)).await;
        type Edit = Box<dyn Fn(&mut Value)>;
        let cases: Vec<(&str, Edit)> = vec![
            ("id", Box::new(|b| b["id"] = json!("not-a-uuid"))),
            ("member_ids", Box::new(|b| b["member_ids"] = json!([]))),
            (
                "member_ids",
                Box::new(|b| b["member_ids"] = json!(["x", "x"])),
            ),
            ("member_ids", Box::new(|b| b["member_ids"] = json!(["a/b"]))),
            ("head_id", Box::new(|b| b["head_id"] = json!("stranger"))),
            (
                "pets",
                Box::new(|b| b["pets"] = json!([{ "species": " ", "count": 1 }])),
            ),
            (
                "pets",
                Box::new(|b| b["pets"] = json!([{ "species": "gato", "count": 0 }])),
            ),
            (
                "client_created_at",
                Box::new(|b| b["client_created_at"] = json!("ontem")),
            ),
        ];
        for (field, edit) in cases {
            let mut body = household_body(&new_id(), &[&ana]);
            edit(&mut body);
            let (status, body) = post(&w, &w.vol_a, &w.a, body).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field} {body}");
            assert_eq!(body["detail"]["field"], json!(field), "{body}");
        }
        let body = household_body(&new_id(), &["ghost"]);
        let (status, body) = post(&w, &w.vol_a, &w.a, body).await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::NOT_FOUND, Some("person_not_found"))
        );
        assert_eq!(stored_person(&w.app, &ana).await.household_id, None);
    }

    #[tokio::test]
    async fn the_same_request_again_gets_the_same_answer_but_not_from_someone_else() {
        let w = world().await;
        let ana = person(&w.app, "Ana", Some(&w.a)).await;
        let body = household_body(&new_id(), &[&ana]);
        let first = post(&w, &w.vol_a, &w.a, body.clone()).await;
        assert_eq!(first.0, StatusCode::CREATED);
        let again = post(&w, &w.vol_a, &w.a, body.clone()).await;
        assert_eq!(again.0, StatusCode::CREATED);
        let (status, other) = post(&w, &w.admin_a, &w.a, body).await;
        assert_eq!(
            (status, error_of(&other)),
            (StatusCode::CONFLICT, Some("household_exists"))
        );
    }

    #[tokio::test]
    async fn unknown_household_is_a_404() {
        let w = world().await;
        let (status, body) = get(&w, &w.vol_a, &new_id()).await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::NOT_FOUND, Some("household_not_found"))
        );
    }

    #[tokio::test]
    async fn patch_adds_removes_and_moves_the_head_and_the_pets() {
        let w = world().await;
        let (ana, beto, caio) = (
            person(&w.app, "Ana", Some(&w.a)).await,
            person(&w.app, "Beto", Some(&w.a)).await,
            person(&w.app, "Caio", Some(&w.a)).await,
        );
        let id = household(&w.app, &w.vol_a, &w.a, &[&ana, &beto]).await;
        let (status, view) = patch(
            &w,
            &w.vol_a,
            &id,
            json!({ "add_member_ids": [caio], "head_id": caio, "pets": [{ "species": "gato", "count": 1 }] }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["members"].as_array().unwrap().len(), 3);
        assert_eq!(view["head_id"], json!(caio));
        assert_eq!(view["pets"][0]["species"], json!("gato"));
        let stored = stored_person(&w.app, &caio).await;
        assert_eq!(stored.household_id.as_deref(), Some(id.as_str()));

        // Removing the head clears it; the person is free to join another household.
        let body = json!({ "remove_member_ids": [caio] });
        let (status, view) = patch(&w, &w.vol_a, &id, body).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["head_id"], Value::Null);
        assert_eq!(view["members"].as_array().unwrap().len(), 2);
        assert_eq!(stored_person(&w.app, &caio).await.household_id, None);
        household(&w.app, &w.vol_a, &w.a, &[&caio]).await;
        let stored: HouseholdDoc = store::get(&w.app.state.db, COLLECTION, &id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.member_ids, vec![ana, beto]);
    }

    #[tokio::test]
    async fn patch_refuses_what_does_not_hold() {
        let w = world().await;
        let (ana, beto, caio) = (
            person(&w.app, "Ana", Some(&w.a)).await,
            person(&w.app, "Beto", Some(&w.a)).await,
            person(&w.app, "Caio", Some(&w.a)).await,
        );
        let id = household(&w.app, &w.vol_a, &w.a, &[&ana]).await;
        let other = household(&w.app, &w.vol_a, &w.a, &[&beto]).await;
        let body = json!({ "add_member_ids": [caio, beto] });
        let (status, body) = patch(&w, &w.vol_a, &id, body).await;
        assert_eq!(
            (status, error_of(&body)),
            (StatusCode::CONFLICT, Some("person_in_other_household"))
        );
        assert_eq!(body["detail"]["household_id"], json!(other));
        assert_eq!(stored_person(&w.app, &caio).await.household_id, None);
        for (field, edit) in [
            ("head_id", json!({ "head_id": caio })),
            (
                "remove_member_ids",
                json!({ "add_member_ids": [caio], "remove_member_ids": [caio] }),
            ),
            (
                "pets",
                json!({ "pets": [{ "species": "gato", "count": 100 }] }),
            ),
        ] {
            let (status, body) = patch(&w, &w.vol_a, &id, edit).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
            assert_eq!(body["detail"]["field"], json!(field));
        }
        let (status, _) = patch(&w, &w.vol_a, &new_id(), json!({})).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // Criterion 34: one person in two households, concurrent.
    #[tokio::test]
    async fn of_many_households_racing_for_one_person_exactly_one_is_created() {
        let w = world().await;
        let ana = person(&w.app, "Ana", Some(&w.a)).await;
        let mut handles = Vec::new();
        for _ in 0..8 {
            let (app, secret, shelter, ana) = (
                w.app.clone(),
                w.vol_a.secret.clone(),
                w.a.clone(),
                ana.clone(),
            );
            handles.push(tokio::spawn(async move {
                app.send(
                    "POST",
                    &format!("/v1/shelters/{shelter}/households"),
                    &secret,
                    Some(household_body(&new_id(), &[&ana])),
                )
                .await
            }));
        }
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.unwrap());
        }
        let created = results
            .iter()
            .filter(|r| r.0 == StatusCode::CREATED)
            .count();
        let conflicts = results
            .iter()
            .filter(|r| error_of(&r.1) == Some("person_in_other_household"))
            .count();
        assert_eq!((created, conflicts), (1, 7), "{results:?}");
        let all = crate::test_support::all_docs(&w.app.state.db, COLLECTION).await;
        assert_eq!(all.len(), 1);
        let winner = results.iter().find(|r| r.0 == StatusCode::CREATED).unwrap();
        assert_eq!(
            stored_person(&w.app, &ana).await.household_id.as_deref(),
            winner.1["household"]["id"].as_str()
        );
    }

    #[tokio::test]
    async fn of_many_patches_racing_for_one_person_exactly_one_adds_it() {
        let w = world().await;
        let caio = person(&w.app, "Caio", Some(&w.a)).await;
        let mut ids = Vec::new();
        for n in 0..6 {
            let p = person(&w.app, &format!("P{n}"), Some(&w.a)).await;
            ids.push(household(&w.app, &w.vol_a, &w.a, &[&p]).await);
        }
        let mut handles = Vec::new();
        for id in ids {
            let (app, secret, caio) = (w.app.clone(), w.vol_a.secret.clone(), caio.clone());
            handles.push(tokio::spawn(async move {
                app.send(
                    "PATCH",
                    &format!("/v1/households/{id}"),
                    &secret,
                    Some(json!({ "add_member_ids": [caio] })),
                )
                .await
            }));
        }
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.unwrap());
        }
        let added = results.iter().filter(|r| r.0 == StatusCode::OK).count();
        let conflicts = results
            .iter()
            .filter(|r| r.0 == StatusCode::CONFLICT)
            .count();
        assert_eq!((added, conflicts), (1, 5), "{results:?}");
        let holders = crate::test_support::all_docs(&w.app.state.db, COLLECTION)
            .await
            .iter()
            .filter(|h| h["member_ids"].to_string().contains(&caio))
            .count();
        assert_eq!(holders, 1);
    }
}
