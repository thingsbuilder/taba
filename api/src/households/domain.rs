//! The rules on a household (spec 3.5.1, 8.5): what a request may carry and who may act on it.
//! Plain functions; nothing here is `async` or reads the database.

use std::collections::HashSet;

use super::model::{CreateHousehold, NewHousehold, Pet, invalid};
use crate::auth::Volunteer;
use crate::error::ApiError;
use crate::people::PersonDoc;

pub(super) const MAX_MEMBERS: usize = 50;
const MAX_PETS: usize = 10;
const MAX_SPECIES_CHARS: usize = 40;
const MAX_PET_COUNT: u32 = 99;
const MAX_ID_CHARS: usize = 128;

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

pub(super) fn valid_ids(ids: &[String], field: &str) -> Result<(), ApiError> {
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

pub(super) fn valid_pets(pets: &[Pet]) -> Result<Vec<Pet>, ApiError> {
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

pub(super) fn validate(request: CreateHousehold) -> Result<NewHousehold, ApiError> {
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
