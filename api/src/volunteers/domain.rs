//! The rules on a volunteer (spec 7, 8.1): profile validation, granting and revoking a role,
//! and applying a `PATCH /v1/me`. Nothing here is `async` or reads the database.

use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde::{Deserialize, Deserializer};
use serde_json::json;

use super::model::{GlobalRole, ProfileInput, RoleGrant, VolunteerDoc};
use crate::error::ApiError;

pub(super) const MAX_NAME_CHARS: usize = 100;
pub(super) const MAX_PHONE_CHARS: usize = 30;

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
            banned_by: None,
            banned_at: None,
            flagged: false,
            flag_reasons: vec![],
            created_at: now,
            last_seen_at: now,
        })
    }
}

impl VolunteerDoc {
    /// Grants (`grant`) or revokes `role`, recording who granted it. `false` means the volunteer
    /// already was in that state and nothing changed: repeating a change adds no second grant.
    pub fn set_role(
        &mut self,
        role: GlobalRole,
        grant: bool,
        by: &str,
        at: FirestoreInstant,
    ) -> bool {
        if self.global_roles.contains(&role) == grant {
            return false;
        }
        if grant {
            self.global_roles.push(role);
            self.role_grants.push(RoleGrant {
                role,
                by: by.to_string(),
                at,
            });
        } else {
            self.global_roles.retain(|r| *r != role);
            self.role_grants.retain(|g| g.role != role);
        }
        true
    }
}

/// Distinguishes an absent field from an explicit `null` (which clears the value).
pub fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Debug, Deserialize)]
pub struct ProfilePatch {
    name: Option<String>,
    #[serde(default, deserialize_with = "present")]
    phone: Option<Option<String>>,
    show_phone: Option<bool>,
    language: Option<String>,
}

impl ProfilePatch {
    /// Changes only the fields the patch names. On an invalid field `doc` may be half changed, so
    /// callers patch a copy.
    pub(super) fn apply(&self, doc: &mut VolunteerDoc) -> Result<(), ApiError> {
        if let Some(name) = &self.name {
            doc.name = valid_name(name)?;
        }
        if let Some(phone) = &self.phone {
            doc.phone = phone.as_deref().map(valid_phone).transpose()?.flatten();
        }
        if let Some(show_phone) = self.show_phone {
            doc.show_phone = show_phone;
        }
        if let Some(language) = &self.language {
            doc.language = valid_language(language)?;
        }
        Ok(())
    }
}
