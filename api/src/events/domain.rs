//! The rules of an event, with no database and no HTTP: its automatic name (spec 4.3.1) and how a
//! patch is validated and applied (spec 8.2).

use axum::http::StatusCode;
use firestore::FirestoreInstant;
use firestore::jiff::tz::TimeZone;
use serde::Deserialize;
use serde_json::json;

use super::model::{EventDoc, EventStatus, ReferenceContacts};
use crate::error::ApiError;
use crate::shelters::Address;

const MAX_NAME_CHARS: usize = 100;
const MAX_PHONE_CHARS: usize = 30;

/// "City/ST – MM/YYYY"; without an address yet, "Evento – MM/YYYY" (spec 4.3.1).
pub fn auto_name(city: Option<&str>, state: Option<&str>, at: FirestoreInstant) -> String {
    let zoned = at.to_zoned(TimeZone::UTC);
    let when = format!("{:02}/{}", zoned.month(), zoned.year());
    match (city, state) {
        (Some(city), Some(state)) => format!("{city}/{state} – {when}"),
        (Some(city), None) => format!("{city} – {when}"),
        _ => format!("Evento – {when}"),
    }
}

impl EventDoc {
    pub(super) fn new(address: &Address, now: FirestoreInstant) -> Self {
        Self {
            name: auto_name(address.city.as_deref(), address.state.as_deref(), now),
            auto_named: true,
            status: EventStatus::Active,
            created_at: now,
            last_activity_at: now,
            closed_at: None,
            closure_reason: None,
            purge_at: None,
            delivery_purge_at: None,
            purged_at: None,
            stats: None,
            reference_contacts: ReferenceContacts::default(),
        }
    }

    /// The name the event got before any shelter had an address, which a later address replaces.
    pub fn has_placeholder_name(&self) -> bool {
        self.auto_named && self.name == auto_name(None, None, self.created_at)
    }

    /// The name once `address` is known, when it should be renamed (spec 4.3.1).
    pub fn renamed_for(&self, address: &Address) -> Option<String> {
        let city = address.city.as_deref()?;
        self.has_placeholder_name()
            .then(|| auto_name(Some(city), address.state.as_deref(), self.created_at))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventPatch {
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) reference_contacts: Option<ContactsPatch>,
}

/// A key left out keeps the phone; `null` or blank clears it.
#[derive(Debug, Clone, Deserialize)]
pub struct ContactsPatch {
    #[serde(default, deserialize_with = "crate::volunteers::present")]
    civil_defense_phone: Option<Option<String>>,
    #[serde(default, deserialize_with = "crate::volunteers::present")]
    child_protection_phone: Option<Option<String>>,
}

fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_event");
    error.detail = json!({ "field": field });
    error
}

fn phone(value: &Option<String>, field: &str) -> Result<Option<String>, ApiError> {
    let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > MAX_PHONE_CHARS {
        return Err(invalid(field));
    }
    Ok(Some(value.into()))
}

/// Applies the patch to the event; the caller has already checked who may do each part.
pub(super) fn apply(event: &mut EventDoc, patch: &EventPatch) -> Result<(), ApiError> {
    if let Some(name) = &patch.name {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
            return Err(invalid("name"));
        }
        event.name = name.into();
        event.auto_named = false;
    }
    if let Some(contacts) = &patch.reference_contacts {
        if let Some(value) = &contacts.civil_defense_phone {
            event.reference_contacts.civil_defense_phone = phone(value, "civil_defense_phone")?;
        }
        if let Some(value) = &contacts.child_protection_phone {
            event.reference_contacts.child_protection_phone =
                phone(value, "child_protection_phone")?;
        }
    }
    Ok(())
}
