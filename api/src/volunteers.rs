//! The `volunteers` collection (spec 7) and `GET`/`PATCH /v1/me` (spec 8.1).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use firestore::errors::BackoffError;
use firestore::{FirestoreInstant, paths};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

use crate::audit::{self, AuditEntry};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::store::fs_error;

pub const COLLECTION: &str = "volunteers";
const MAX_NAME_CHARS: usize = 100;
const MAX_PHONE_CHARS: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GlobalRole {
    Validator,
    CivilDefense,
    Mediator,
    Protection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleGrant {
    pub role: GlobalRole,
    pub by: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub at: FirestoreInstant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolunteerDoc {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    pub show_phone: bool,
    pub language: String,
    #[serde(default)]
    pub global_roles: Vec<GlobalRole>,
    #[serde(default)]
    pub role_grants: Vec<RoleGrant>,
    #[serde(default)]
    pub banned: bool,
    #[serde(default)]
    pub flagged: bool,
    #[serde(default)]
    pub flag_reasons: Vec<String>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub last_seen_at: FirestoreInstant,
}

/// Profile as the app sends it (spec 8.1).
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileInput {
    pub name: String,
    #[serde(default)]
    pub phone: Option<String>,
    pub show_phone: bool,
    pub language: String,
}

fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_profile");
    error.detail = json!({ "field": field });
    error
}

pub fn valid_name(name: &str) -> Result<String, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(invalid("name"));
    }
    Ok(name.to_string())
}

/// Blank means "no phone".
pub fn valid_phone(phone: &str) -> Result<Option<String>, ApiError> {
    let phone = phone.trim();
    if phone.chars().count() > MAX_PHONE_CHARS {
        return Err(invalid("phone"));
    }
    Ok((!phone.is_empty()).then(|| phone.to_string()))
}

pub fn valid_language(language: &str) -> Result<String, ApiError> {
    match language {
        "pt-BR" | "en" => Ok(language.to_string()),
        _ => Err(invalid("language")),
    }
}

impl VolunteerDoc {
    pub fn from_profile(profile: &ProfileInput) -> Result<Self, ApiError> {
        let now = crate::store::now();
        Ok(Self {
            name: valid_name(&profile.name)?,
            phone: profile
                .phone
                .as_deref()
                .map(valid_phone)
                .transpose()?
                .flatten(),
            show_phone: profile.show_phone,
            language: valid_language(&profile.language)?,
            global_roles: vec![],
            role_grants: vec![],
            banned: false,
            flagged: false,
            flag_reasons: vec![],
            created_at: now,
            last_seen_at: now,
        })
    }
}

/// What the app may see of a profile.
pub fn view(id: &str, doc: &VolunteerDoc) -> Value {
    json!({
        "id": id,
        "name": doc.name,
        "phone": doc.phone,
        "show_phone": doc.show_phone,
        "language": doc.language,
    })
}

/// `memberships` stays empty until phase 2 creates shelters.
pub fn me_body(volunteer: &Volunteer) -> Value {
    json!({
        "volunteer": view(&volunteer.id, &volunteer.doc),
        "global_roles": volunteer.doc.global_roles,
        "memberships": volunteer.memberships,
    })
}

pub async fn get_me(volunteer: Volunteer) -> Json<Value> {
    Json(me_body(&volunteer))
}

/// Distinguishes an absent field from an explicit `null` (which clears the phone).
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

#[derive(Debug, Deserialize)]
pub struct ProfilePatch {
    name: Option<String>,
    #[serde(default, deserialize_with = "present")]
    phone: Option<Option<String>>,
    show_phone: Option<bool>,
    language: Option<String>,
}

pub async fn patch_me(
    State(state): State<AppState>,
    mut volunteer: Volunteer,
    Json(patch): Json<ProfilePatch>,
) -> Result<Json<Value>, ApiError> {
    let mut doc = volunteer.doc.clone();
    if let Some(name) = &patch.name {
        doc.name = valid_name(name)?;
    }
    if let Some(phone) = &patch.phone {
        doc.phone = phone.as_deref().map(valid_phone).transpose()?.flatten();
    }
    if let Some(show_phone) = patch.show_phone {
        doc.show_phone = show_phone;
    }
    if let Some(language) = &patch.language {
        doc.language = valid_language(language)?;
    }

    let entry = AuditEntry::new("volunteer.update", "volunteer", &volunteer.id).by(&volunteer.id);
    let id = volunteer.id.clone();
    state
        .db
        .run_transaction(|db, tx| {
            let (id, doc, entry) = (id.clone(), doc.clone(), entry.clone());
            Box::pin(async move {
                let mut write = || -> Result<(), ApiError> {
                    // Only the profile fields: a role granted meanwhile must survive.
                    db.fluent()
                        .update()
                        .fields(paths!(VolunteerDoc::{name, phone, show_phone, language}))
                        .in_col(COLLECTION)
                        .document_id(&id)
                        .object(&doc)
                        .add_to_transaction(tx)
                        .map_err(fs_error)?;
                    audit::add(&db, tx, &entry)
                };
                write().map_err(BackoffError::permanent)
            })
        })
        .await
        .map_err(fs_error)?;
    volunteer.doc = doc;
    Ok(Json(me_body(&volunteer)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::members::{Change, change_role};
    use crate::test_support::{TestApp, audit_actions};

    async fn patch(app: &TestApp, secret: &str, body: Value) -> (StatusCode, Value) {
        let auth = format!("Bearer {secret}");
        let (status, _, json) = app
            .request(
                "PATCH",
                "/v1/me",
                &[("authorization", auth.as_str())],
                Some(body),
            )
            .await;
        (status, json)
    }

    #[test]
    fn field_validation() {
        assert_eq!(valid_name("  Ana  ").unwrap(), "Ana");
        assert!(valid_name("").is_err() && valid_name("   ").is_err());
        assert!(valid_name(&"x".repeat(100)).is_ok() && valid_name(&"x".repeat(101)).is_err());
        assert_eq!(
            valid_phone(" +55 11 99999-0000 ").unwrap().as_deref(),
            Some("+55 11 99999-0000")
        );
        assert_eq!(valid_phone("  ").unwrap(), None);
        assert!(valid_phone(&"1".repeat(31)).is_err());
        assert!(valid_language("pt-BR").is_ok() && valid_language("en").is_ok());
        assert!(valid_language("es").is_err() && valid_language("").is_err());
    }

    #[tokio::test]
    async fn me_returns_the_profile_roles_and_memberships() {
        let app = TestApp::new().await;
        let (secret, id) = app.login("dev", "tok", "Ana").await;
        let (status, body) = app.get_authed("/v1/me", &secret).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({
                "volunteer": { "id": id, "name": "Ana", "phone": null, "show_phone": false, "language": "pt-BR" },
                "global_roles": [],
                "memberships": [],
            })
        );
    }

    #[tokio::test]
    async fn patch_changes_only_the_fields_sent() {
        let app = TestApp::new().await;
        let (secret, _) = app.login("dev", "tok", "Ana").await;
        let (status, body) = patch(
            &app,
            &secret,
            json!({ "name": "Ana Souza", "phone": "123", "show_phone": true }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["volunteer"]["name"], "Ana Souza");
        let (_, me) = app.get_authed("/v1/me", &secret).await;
        assert_eq!(
            (
                &me["volunteer"]["name"],
                &me["volunteer"]["phone"],
                &me["volunteer"]["show_phone"],
                &me["volunteer"]["language"]
            ),
            (
                &json!("Ana Souza"),
                &json!("123"),
                &json!(true),
                &json!("pt-BR")
            )
        );

        // An absent phone keeps it; an explicit null clears it.
        patch(&app, &secret, json!({ "language": "en" })).await;
        let (_, me) = app.get_authed("/v1/me", &secret).await;
        assert_eq!(
            (&me["volunteer"]["phone"], &me["volunteer"]["language"]),
            (&json!("123"), &json!("en"))
        );
        patch(&app, &secret, json!({ "phone": null })).await;
        let (_, me) = app.get_authed("/v1/me", &secret).await;
        assert_eq!(me["volunteer"]["phone"], Value::Null);
    }

    #[tokio::test]
    async fn patch_rejects_invalid_values_and_writes_nothing() {
        let app = TestApp::new().await;
        let (secret, _) = app.login("dev", "tok", "Ana").await;
        let before = audit_actions(&app.state.db).await.len();
        for body in [
            json!({ "name": "" }),
            json!({ "language": "fr" }),
            json!({ "phone": "1".repeat(31) }),
        ] {
            let (status, json) = patch(&app, &secret, body.clone()).await;
            assert_eq!(
                (status, json["error"].as_str()),
                (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_profile")),
                "{body}"
            );
        }
        assert_eq!(
            app.get_authed("/v1/me", &secret).await.1["volunteer"]["name"],
            "Ana"
        );
        assert_eq!(audit_actions(&app.state.db).await.len(), before);
    }

    #[tokio::test]
    async fn patch_needs_a_session_and_is_audited() {
        let app = TestApp::new().await;
        let (status, _, _) = app
            .request("PATCH", "/v1/me", &[], Some(json!({ "name": "X" })))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (secret, _) = app.login("dev", "tok", "Ana").await;
        patch(&app, &secret, json!({ "name": "Bia" })).await;
        assert!(
            audit_actions(&app.state.db)
                .await
                .contains(&"volunteer.update".to_string())
        );
    }

    #[tokio::test]
    async fn patch_keeps_roles_granted_meanwhile() {
        let app = TestApp::new().await;
        let (secret, id) = app.login("dev", "tok", "Ana").await;
        change_role(&app.state.db, &id, GlobalRole::Validator, Change::Grant)
            .await
            .unwrap();
        patch(&app, &secret, json!({ "name": "Bia" })).await;
        let (_, me) = app.get_authed("/v1/me", &secret).await;
        assert_eq!(
            (&me["global_roles"], &me["volunteer"]["name"]),
            (&json!(["validator"]), &json!("Bia"))
        );
    }
}
