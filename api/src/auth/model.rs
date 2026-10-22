//! The stored documents of a login (capture, session), the authenticated `Volunteer`, and the
//! `AppState` every route shares (spec 3.1, 5.2, 6.2, 6.3.2, 8.1).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, bail};
use axum::extract::FromRef;
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use firestore::{FirestoreDb, FirestoreInstant};
use serde::{Deserialize, Serialize};

use super::domain::derive_id;
use crate::config::Config;
use crate::error::ApiError;
use crate::members::ShelterRole;
use crate::shelters::Confirmation;
use crate::unico::Unico;
use crate::volunteers::{GlobalRole, VolunteerDoc};

pub(super) const CAPTURES: &str = "captures";
pub const SESSIONS: &str = "sessions";

#[derive(Clone)]
pub struct AppState {
    pub db: FirestoreDb,
    pub config: Arc<Config>,
    pub unico: Arc<Unico>,
    pub(super) hmac_key: Arc<[u8]>,
    pub(super) thumbnail_key: Arc<[u8]>,
    pub(super) domain: Arc<str>,
    /// Serializes shelter creation (spec 6.2.4).
    pub shelter_lock: Arc<tokio::sync::Mutex<()>>,
    /// When each event's `last_activity_at` was last written (spec 4.3.3).
    pub event_activity: Arc<Mutex<HashMap<String, Instant>>>,
}

impl FromRef<AppState> for FirestoreDb {
    fn from_ref(state: &AppState) -> Self {
        state.db.clone()
    }
}

impl AppState {
    /// Fails at startup when what login needs is not configured (spec 6.2.2).
    pub fn new(db: FirestoreDb, config: Config, unico: Unico) -> anyhow::Result<Self> {
        let Some(domain) = config.domain.clone() else {
            bail!("DOMAIN is required to serve");
        };
        let key = config
            .hmac_key
            .as_ref()
            .context("HMAC_KEY is required to serve")?;
        let hmac_key = STANDARD
            .decode(key.0.trim())
            .context("HMAC_KEY is not valid base64")?;
        if hmac_key.len() != 32 {
            bail!("HMAC_KEY must be 32 bytes in base64");
        }
        let thumbnail_key = STANDARD
            .decode(
                config
                    .thumbnail_key
                    .as_ref()
                    .context("THUMBNAIL_KEY is required to serve")?
                    .0
                    .trim(),
            )
            .context("THUMBNAIL_KEY is not valid base64")?;
        if thumbnail_key.len() != 32 {
            bail!("THUMBNAIL_KEY must be 32 bytes in base64");
        }
        let status_db = db.clone();
        Ok(Self {
            db,
            config: Arc::new(config),
            // Every call obeys and feeds the degraded mode (spec 5.5, 5.7).
            unico: Arc::new(unico.with_status(status_db)),
            hmac_key: hmac_key.into(),
            thumbnail_key: thumbnail_key.into(),
            domain: domain.into(),
            shelter_lock: Arc::default(),
            event_activity: Arc::default(),
        })
    }

    /// The AES-256-GCM key of the thumbnails (spec 4.6); used by `thumbnail.rs`.
    pub fn thumbnail_key(&self) -> &[u8] {
        &self.thumbnail_key
    }

    /// The `people` id of a Human Token: its HMAC (spec 6.3.2); the token is not kept.
    pub fn person_id_for_token(&self, token: &str) -> String {
        derive_id(&self.hmac_key, token)
    }

    pub fn domain(&self) -> &str {
        &self.domain
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Login,
    Invite,
    Checkin,
    Revalidation,
    Biometrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    Created,
    Ok,
    Inconclusive,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureDoc {
    pub process_id: String,
    pub purpose: Purpose,
    pub device_id: String,
    pub status: CaptureStatus,
    /// HMAC of the token; the token itself is never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person_id: Option<String>,
    /// Who asked for the capture (session purposes only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volunteer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shelter_id: Option<String>,
    /// The person a revalidation must turn out to be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_person_id: Option<String>,
    pub consumed: bool,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub expires_at: FirestoreInstant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDoc {
    pub volunteer_id: String,
    pub device_id: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub renewed_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub expires_at: FirestoreInstant,
}

/// Shelter membership as `GET /v1/me` reports it (spec 8.1).
#[derive(Debug, Clone, Serialize)]
pub struct Membership {
    pub shelter_id: String,
    pub name: String,
    pub role: ShelterRole,
    pub confirmation: Confirmation,
    #[serde(skip)]
    pub event_id: Option<String>,
}

/// The authenticated volunteer: profile, global roles (inside `doc`) and active memberships.
#[derive(Debug, Clone)]
pub struct Volunteer {
    pub id: String,
    pub doc: VolunteerDoc,
    pub memberships: Vec<Membership>,
    pub session_id: String,
}

impl Volunteer {
    pub fn has_global(&self, role: GlobalRole) -> bool {
        self.doc.global_roles.contains(&role)
    }

    /// `validator` and `civil_defense` confirm and revoke shelters (spec 2.1).
    pub fn is_validator_or_civil_defense(&self) -> bool {
        self.has_global(GlobalRole::Validator) || self.has_global(GlobalRole::CivilDefense)
    }

    pub fn role_in(&self, shelter_id: &str) -> Option<ShelterRole> {
        self.memberships
            .iter()
            .find(|m| m.shelter_id == shelter_id)
            .map(|m| m.role)
    }

    pub fn is_admin_of(&self, shelter_id: &str) -> bool {
        self.role_in(shelter_id) == Some(ShelterRole::Admin)
    }
}

pub(super) fn unauthorized() -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized")
}
