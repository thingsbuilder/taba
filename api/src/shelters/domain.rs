//! The rules of a shelter, with no database and no HTTP: what a shelter stores, how a request to
//! create one is validated, and its transitions (open, close, confirm, revoke). `mod.rs` reads and
//! writes around them; `endorsements.rs` confirms through the same `confirm`.

use std::collections::HashSet;

use axum::http::StatusCode;
use firestore::FirestoreInstant;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::ApiError;
use crate::geo;

pub const COLLECTION: &str = "shelters";
/// Largest `accuracy` accepted when creating a shelter or endorsing one (spec 4.2).
pub const MAX_ACCURACY_M: f64 = 100.0;
const MAX_NAME_CHARS: usize = 100;
const MAX_CAPACITY: u32 = 100_000;
pub(super) const MAX_REASON_CHARS: usize = 500;
const MAX_ADDRESS_TEXT_CHARS: usize = 500;
const MAX_ADDRESS_PART_CHARS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShelterStatus {
    Open,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confirmation {
    Unconfirmed,
    Confirmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationType {
    Validator,
    InPerson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndorsementKind {
    InPerson,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// True until Nominatim answers (spec 6.1.6).
    #[serde(default)]
    pub pending: bool,
}

/// The address as the app sends it; blank parts count as absent.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AddressInput {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

pub(super) fn invalid(field: &str) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_shelter");
    error.detail = json!({ "field": field });
    error
}

fn part(value: &Option<String>, max: usize) -> Result<Option<String>, ApiError> {
    let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > max {
        return Err(invalid("address"));
    }
    Ok(Some(value.into()))
}

impl Address {
    /// What a shelter has before its address is known.
    pub(super) fn pending() -> Self {
        Self {
            pending: true,
            ..Self::default()
        }
    }

    /// `None` when the input holds no address at all.
    pub(super) fn from_input(input: &AddressInput) -> Result<Option<Self>, ApiError> {
        let address = Self {
            text: part(&input.text, MAX_ADDRESS_TEXT_CHARS)?,
            city: part(&input.city, MAX_ADDRESS_PART_CHARS)?,
            state: part(&input.state, MAX_ADDRESS_PART_CHARS)?,
            pending: false,
        };
        let empty = address.text.is_none() && address.city.is_none() && address.state.is_none();
        Ok((!empty).then_some(address))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Endorsement {
    pub kind: EndorsementKind,
    pub volunteer_id: String,
    pub endorser_shelter_id: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub at: FirestoreInstant,
    pub distance_m: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Revocation {
    pub by: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub at: FirestoreInstant,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShelterDoc {
    #[serde(alias = "_firestore_id", default, skip_serializing)]
    pub id: Option<String>,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: f64,
    pub geohash: String,
    pub address: Address,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<u32>,
    pub created_by: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub client_created_at: FirestoreInstant,
    #[serde(with = "firestore::serialize_as_timestamp")]
    pub created_at: FirestoreInstant,
    pub status: ShelterStatus,
    pub confirmation: Confirmation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation_type: Option<ConfirmationType>,
    pub can_endorse: bool,
    #[serde(default)]
    pub endorsements: Vec<Endorsement>,
    #[serde(default)]
    pub revocations: Vec<Revocation>,
    #[serde(default)]
    pub flagged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_site_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflicts_with: Option<String>,
    /// What the shelter gives and the rule of each item (spec 3.5.2); set by `PUT /v1/shelters/{id}/items`.
    #[serde(default, skip_serializing_if = "crate::deliveries::Items::is_empty")]
    pub items: crate::deliveries::Items,
}

/// The shelter a new one is too close to, and why the creator opened it anyway (spec 3.2.7).
pub(super) struct Neighbour {
    pub shelter_id: String,
    pub reason: String,
}

impl ShelterDoc {
    /// A shelter just created: open, unconfirmed, flagged when it was opened next to another one.
    pub(super) fn open(
        new: &NewShelter,
        creator_id: &str,
        event_id: &str,
        neighbour: Option<Neighbour>,
        now: FirestoreInstant,
    ) -> Self {
        Self {
            id: None,
            name: new.name.clone(),
            lat: new.lat,
            lon: new.lon,
            accuracy_m: new.accuracy_m,
            geohash: geo::encode(new.lat, new.lon, geo::STORED_PRECISION),
            address: new.address.clone(),
            event_id: Some(event_id.to_string()),
            capacity: new.capacity,
            created_by: creator_id.into(),
            client_created_at: new.client_created_at,
            created_at: now,
            status: ShelterStatus::Open,
            confirmation: Confirmation::Unconfirmed,
            confirmation_type: None,
            can_endorse: false,
            endorsements: vec![],
            revocations: vec![],
            flagged: neighbour.is_some(),
            other_site_reason: neighbour.as_ref().map(|n| n.reason.clone()),
            conflicts_with: neighbour.map(|n| n.shelter_id),
            items: Default::default(),
        }
    }

    /// Closes the shelter; `false` means it already was, so there is nothing to write.
    pub fn close(&mut self) -> bool {
        let changed = self.status != ShelterStatus::Closed;
        self.status = ShelterStatus::Closed;
        changed
    }

    /// Confirms the shelter and lets it endorse others (spec 2.2). Both routes that confirm go
    /// through here: a validator (`update`) and an in-person endorsement. `false` means it already
    /// was confirmed, and nothing changed.
    pub fn confirm(&mut self, kind: ConfirmationType) -> bool {
        if self.confirmation == Confirmation::Confirmed {
            return false;
        }
        self.confirmation = Confirmation::Confirmed;
        self.confirmation_type = Some(kind);
        self.can_endorse = true;
        true
    }

    /// Confirms the shelter on the word of an endorser who stood next to it (spec 3.9.3).
    pub fn endorse_in_person(&mut self, endorsement: Endorsement) {
        self.confirm(ConfirmationType::InPerson);
        self.endorsements.push(endorsement);
    }

    /// Takes the confirmation back (spec 3.9). Returns the endorsements the confirmation rested on,
    /// one per volunteer, whose endorsers are to be flagged; older ones were flagged by an earlier
    /// revocation. `None` means the shelter was not confirmed, so nothing changed and nobody is
    /// flagged: a repeated revocation has no effect.
    pub(super) fn revoke(
        &mut self,
        by: &str,
        reason: &str,
        at: FirestoreInstant,
    ) -> Option<Vec<Endorsement>> {
        if self.confirmation == Confirmation::Unconfirmed {
            return None;
        }
        let since = self.revocations.last().map(|r| r.at);
        let mut seen = HashSet::new();
        let to_flag = self
            .endorsements
            .iter()
            .filter(|e| since.is_none_or(|since| e.at > since))
            .filter(|e| seen.insert(e.volunteer_id.as_str()))
            .cloned()
            .collect();
        self.confirmation = Confirmation::Unconfirmed;
        self.confirmation_type = None;
        self.can_endorse = false;
        self.revocations.push(Revocation {
            by: by.to_string(),
            at,
            reason: reason.to_string(),
        });
        Some(to_flag)
    }
}

// ---- validation ----

#[derive(Debug, Deserialize)]
pub struct CreateShelter {
    id: String,
    name: String,
    lat: f64,
    lon: f64,
    accuracy: f64,
    #[serde(default)]
    address: Option<AddressInput>,
    #[serde(default)]
    capacity: Option<u32>,
    #[serde(default)]
    other_site_reason: Option<String>,
    client_created_at: String,
}

#[derive(Debug, Clone)]
pub(super) struct NewShelter {
    pub id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: f64,
    pub address: Address,
    pub capacity: Option<u32>,
    pub other_site_reason: Option<String>,
    pub client_created_at: FirestoreInstant,
}

pub(super) fn valid_name(name: &str) -> Result<String, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(invalid("name"));
    }
    Ok(name.into())
}

pub(super) fn valid_capacity(capacity: u32) -> Result<u32, ApiError> {
    if capacity == 0 || capacity > MAX_CAPACITY {
        return Err(invalid("capacity"));
    }
    Ok(capacity)
}

pub fn valid_position(lat: f64, lon: f64, accuracy: f64) -> Result<(), ApiError> {
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(invalid("position"));
    }
    if !accuracy.is_finite() || accuracy < 0.0 {
        return Err(invalid("accuracy"));
    }
    if accuracy > MAX_ACCURACY_M {
        return Err(insufficient_accuracy(accuracy));
    }
    Ok(())
}

pub fn insufficient_accuracy(accuracy: f64) -> ApiError {
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "insufficient_accuracy");
    error.detail = json!({ "accuracy": accuracy, "max": MAX_ACCURACY_M });
    error
}

pub(super) fn validate(request: CreateShelter) -> Result<NewShelter, ApiError> {
    uuid::Uuid::parse_str(&request.id).map_err(|_| invalid("id"))?;
    let name = valid_name(&request.name)?;
    let capacity = request.capacity.map(valid_capacity).transpose()?;
    let client_created_at = request
        .client_created_at
        .parse()
        .map_err(|_| invalid("client_created_at"))?;
    let other_site_reason = match request.other_site_reason.as_deref().map(str::trim) {
        None => None,
        Some(reason) if reason.is_empty() || reason.chars().count() > MAX_REASON_CHARS => {
            return Err(invalid("other_site_reason"));
        }
        Some(reason) => Some(reason.to_string()),
    };
    let address = match &request.address {
        Some(input) => Address::from_input(input)?.unwrap_or_else(Address::pending),
        None => Address::pending(),
    };
    valid_position(request.lat, request.lon, request.accuracy)?;
    Ok(NewShelter {
        id: request.id,
        name,
        lat: request.lat,
        lon: request.lon,
        accuracy_m: request.accuracy,
        address,
        capacity,
        other_site_reason,
        client_created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;

    fn at(micros: i64) -> FirestoreInstant {
        FirestoreInstant::from_microsecond(1_800_000_000_000_000 + micros).unwrap()
    }

    fn new_shelter(neighbour: Option<Neighbour>) -> ShelterDoc {
        let new = NewShelter {
            id: "s1".into(),
            name: "Abrigo".into(),
            lat: -30.0,
            lon: -51.0,
            accuracy_m: 10.0,
            address: Address::pending(),
            capacity: Some(10),
            other_site_reason: None,
            client_created_at: store::now(),
        };
        ShelterDoc::open(&new, "creator", "e1", neighbour, at(0))
    }

    fn endorsement(volunteer: &str, when: FirestoreInstant) -> Endorsement {
        Endorsement {
            kind: EndorsementKind::InPerson,
            volunteer_id: volunteer.into(),
            endorser_shelter_id: "other".into(),
            at: when,
            distance_m: 5.0,
        }
    }

    fn volunteers_of(flagged: Option<Vec<Endorsement>>) -> Vec<String> {
        flagged
            .unwrap()
            .into_iter()
            .map(|e| e.volunteer_id)
            .collect()
    }

    #[test]
    fn a_new_shelter_is_open_and_unconfirmed_and_flagged_only_next_to_another() {
        let plain = new_shelter(None);
        assert_eq!(plain.status, ShelterStatus::Open);
        assert_eq!(plain.confirmation, Confirmation::Unconfirmed);
        assert!(!plain.can_endorse);
        assert!(
            !plain.flagged && plain.conflicts_with.is_none() && plain.other_site_reason.is_none()
        );

        let near = new_shelter(Some(Neighbour {
            shelter_id: "s0".into(),
            reason: "other site".into(),
        }));
        assert!(near.flagged);
        assert_eq!(near.conflicts_with.as_deref(), Some("s0"));
        assert_eq!(near.other_site_reason.as_deref(), Some("other site"));
        assert_eq!(near.confirmation, Confirmation::Unconfirmed);
    }

    /// A repeated item does not repeat its effect: closing again changes nothing.
    #[test]
    fn closing_twice_changes_once() {
        let mut shelter = new_shelter(None);
        assert!(shelter.close());
        assert!(!shelter.close());
        assert_eq!(shelter.status, ShelterStatus::Closed);
    }

    /// Both routes that confirm go through `confirm`, and whichever came first stays: a second
    /// confirmation neither changes the type nor reports a change.
    #[test]
    fn confirming_twice_changes_once_and_keeps_the_first_type() {
        let mut shelter = new_shelter(None);
        assert!(shelter.confirm(ConfirmationType::Validator));
        assert!(shelter.can_endorse);
        assert!(!shelter.confirm(ConfirmationType::InPerson));
        assert_eq!(shelter.confirmation_type, Some(ConfirmationType::Validator));

        let mut shelter = new_shelter(None);
        shelter.endorse_in_person(endorsement("v1", at(1)));
        assert_eq!(shelter.confirmation, Confirmation::Confirmed);
        assert_eq!(shelter.confirmation_type, Some(ConfirmationType::InPerson));
        assert!(shelter.can_endorse);
        assert_eq!(shelter.endorsements.len(), 1);
    }

    /// A repeated revocation has no effect: nobody is flagged twice and no second revocation is
    /// recorded.
    #[test]
    fn revoking_twice_revokes_and_flags_once() {
        let mut shelter = new_shelter(None);
        shelter.endorse_in_person(endorsement("v1", at(1)));
        shelter.endorse_in_person(endorsement("v1", at(2)));
        shelter.endorse_in_person(endorsement("v2", at(3)));

        let flagged = shelter.revoke("validator", "reason", at(10));
        assert_eq!(volunteers_of(flagged), ["v1", "v2"], "one per volunteer");
        assert_eq!(shelter.confirmation, Confirmation::Unconfirmed);
        assert_eq!(shelter.confirmation_type, None);
        assert!(!shelter.can_endorse);
        assert_eq!(shelter.revocations.len(), 1);

        assert!(shelter.revoke("validator", "again", at(11)).is_none());
        assert_eq!(shelter.revocations.len(), 1);
        assert_eq!(shelter.revocations[0].reason, "reason");
    }

    /// A revocation flags the endorsers its confirmation rested on, not those an earlier one
    /// already flagged.
    #[test]
    fn a_second_revocation_flags_only_the_endorsers_since_the_first() {
        let mut shelter = new_shelter(None);
        shelter.endorse_in_person(endorsement("v1", at(1)));
        shelter.revoke("validator", "first", at(10)).unwrap();
        shelter.endorse_in_person(endorsement("v2", at(20)));
        let flagged = shelter.revoke("validator", "second", at(30));
        assert_eq!(volunteers_of(flagged), ["v2"]);
        assert_eq!(shelter.revocations.len(), 2);
        assert_eq!(shelter.endorsements.len(), 2, "the history is kept");
    }

    #[test]
    fn revoking_a_shelter_a_validator_confirmed_flags_no_one() {
        let mut shelter = new_shelter(None);
        shelter.confirm(ConfirmationType::Validator);
        let flagged = shelter.revoke("validator", "reason", at(10));
        assert!(volunteers_of(flagged).is_empty());
        assert_eq!(shelter.confirmation, Confirmation::Unconfirmed);
    }
}
