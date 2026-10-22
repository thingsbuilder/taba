//! The rules on a person (spec 3.4, 4.1, 4.12, 7, 9.4): plain functions over `PersonDoc` and
//! the documents that go with it. Nothing here is `async` or reads the database, so every
//! rule runs in a unit test; `persist` reads, calls these and writes what they return.

use std::collections::HashSet;
use std::hash::Hash;

use firestore::FirestoreInstant;
use serde::Deserialize;
use serde_json::{Value, json};

use super::model::{
    ChildStatus, ExitReason, Guardian, Level, Needs, Origin, PersonDoc, Stay, Vulnerabilities,
    invalid,
};
use crate::audit::LegalBasis;
use crate::children::{self, ChildChange};
use crate::error::ApiError;
use crate::households::HouseholdDoc;
use crate::shelters::ShelterDoc;
use crate::store;

const MAX_TEXT_CHARS: usize = 100;
const MIN_BIRTH_YEAR: u16 = 1900;

// ---- data ----

/// The `data` of a check-in or of a `PATCH` (spec 3.4). Unknown fields are refused.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonData {
    pub(super) name: Option<String>,
    pub(super) social_name: Option<String>,
    pub(super) wristband: Option<String>,
    pub(super) birth_year: Option<u16>,
    pub(super) is_child: Option<bool>,
    pub(super) child_status: Option<ChildStatus>,
    pub(super) needs: Option<Needs>,
    pub(super) vulnerabilities: Option<Vulnerabilities>,
}

impl PersonData {
    /// Nothing but `child_status` is set.
    pub(super) fn only_child_status(&self) -> bool {
        self.name.is_none()
            && self.social_name.is_none()
            && self.wristband.is_none()
            && self.birth_year.is_none()
            // `child_status` makes `is_child` true on its own (see `validated`)
            && self.is_child != Some(false)
            && self.needs.is_none()
            && self.vulnerabilities.is_none()
    }
}

fn text(value: Option<String>, field: &str) -> Result<Option<String>, ApiError> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim();
    if value.chars().count() > MAX_TEXT_CHARS {
        return Err(invalid(field));
    }
    Ok((!value.is_empty()).then(|| value.to_string()))
}

fn current_year() -> u16 {
    u16::try_from(store::now().as_second() / 31_556_952 + 1970).unwrap_or(u16::MAX)
}

impl PersonData {
    /// Parses the `data` object; free text in `needs` or `vulnerabilities` is a `422` (criterion 38).
    pub(super) fn parse(value: Value) -> Result<Self, ApiError> {
        let value = if value.is_null() { json!({}) } else { value };
        let data: PersonData = serde_json::from_value(value).map_err(|err| {
            let mut error = invalid("data");
            error.detail = json!({ "field": "data", "message": err.to_string() });
            error
        })?;
        data.validated()
    }

    fn validated(mut self) -> Result<Self, ApiError> {
        self.name = text(self.name, "name")?;
        self.social_name = text(self.social_name, "social_name")?;
        self.wristband = text(self.wristband, "wristband")?;
        if let Some(year) = self.birth_year {
            if !(MIN_BIRTH_YEAR..=current_year()).contains(&year) {
                return Err(invalid("birth_year"));
            }
        }
        if self.child_status.is_some() {
            if self.is_child == Some(false) {
                return Err(invalid("child_status"));
            }
            self.is_child = Some(true);
        }
        if let Some(v) = &mut self.vulnerabilities {
            v.indigenous_people = text(v.indigenous_people.take(), "indigenous_people")?;
            v.migrant_language = text(v.migrant_language.take(), "migrant_language")?;
        }
        Ok(self)
    }

    pub(super) fn identifies(&self) -> bool {
        self.name.is_some() || self.wristband.is_some()
    }

    /// Copies what was sent onto the person; the thumbnail goes in the same write that gives
    /// the person a name or a wristband (spec 4.6, criterion 39). `is_child` and `child_status`
    /// go through `children::change_child_state`, which decides who may change them and returns
    /// the status the separation alert is owed for (`may_alter`: protection or Civil Defense).
    pub(super) fn apply(
        &self,
        person: &mut PersonDoc,
        may_alter: bool,
    ) -> Result<Option<ChildStatus>, ApiError> {
        let child_alert = children::change_child_state(
            person,
            ChildChange {
                is_child: self.is_child,
                child_status: self.child_status,
            },
            may_alter,
        )?;
        if self.name.is_some() {
            person.name = self.name.clone();
        }
        if self.social_name.is_some() {
            person.social_name = self.social_name.clone();
        }
        if self.wristband.is_some() {
            person.wristband = self.wristband.clone();
        }
        if self.birth_year.is_some() {
            person.birth_year = self.birth_year;
        }
        if let Some(needs) = &self.needs {
            person.needs = needs.clone();
        }
        if let Some(vulnerabilities) = &self.vulnerabilities {
            person.vulnerabilities = vulnerabilities.clone();
        }
        drop_thumbnail_if_identified(person);
        Ok(child_alert)
    }
}

pub fn drop_thumbnail_if_identified(person: &mut PersonDoc) {
    if person.name.is_some() || person.wristband.is_some() {
        person.thumbnail_encrypted = None;
    }
}

// ---- stays ----

pub(super) fn open_stay_index(stays: &[Stay]) -> Option<usize> {
    stays.iter().rposition(|s| s.left_at.is_none())
}

/// Opens a stay in `shelter_id` (spec 9.4). An open stay elsewhere is closed with
/// `checkin_elsewhere`; if that one is more recent than `entered_at` (a check-in that was
/// offline), the new stay is recorded already closed and the current shelter does not change.
/// Returns whether the stays changed: a second check-in in the shelter the person is already
/// in changes nothing.
pub fn enter_shelter(
    person: &mut PersonDoc,
    shelter_id: &str,
    event_id: Option<&str>,
    entered_at: FirestoreInstant,
    registered_by: &str,
    now: FirestoreInstant,
) -> bool {
    let entered_at = entered_at.min(now);
    let mut stay = Stay {
        shelter_id: shelter_id.into(),
        event_id: event_id.map(str::to_string),
        entered_at,
        left_at: None,
        exit_reason: None,
        exit_destination: None,
        destination_municipality: None,
        registered_by: registered_by.into(),
    };
    if let Some(event_id) = event_id {
        if !person.event_ids.iter().any(|e| e == event_id) {
            person.event_ids.push(event_id.into());
        }
    }
    match open_stay_index(&person.stays) {
        Some(i) if person.stays[i].shelter_id == shelter_id => return false,
        Some(i) if person.stays[i].entered_at > entered_at => {
            stay.left_at = Some(person.stays[i].entered_at);
            stay.exit_reason = Some(ExitReason::CheckinElsewhere);
            person.stays.push(stay);
            return true;
        }
        Some(i) => {
            person.stays[i].left_at = Some(now);
            person.stays[i].exit_reason = Some(ExitReason::CheckinElsewhere);
        }
        None => {}
    }
    person.stays.push(stay);
    person.current_shelter_id = Some(shelter_id.into());
    true
}

// ---- check-in ----

pub(super) fn new_person(
    id: &str,
    level: Level,
    basis: LegalBasis,
    now: FirestoreInstant,
) -> PersonDoc {
    PersonDoc {
        id: Some(id.into()),
        level,
        biometrics_declined: false,
        former_ids: vec![],
        name: None,
        social_name: None,
        wristband: None,
        birth_year: None,
        is_child: false,
        child_status: None,
        guardians: vec![],
        guardian_ids: vec![],
        needs: Needs::default(),
        vulnerabilities: Vulnerabilities::default(),
        household_id: None,
        current_shelter_id: None,
        stays: vec![],
        event_ids: vec![],
        rescue_ids: vec![],
        thumbnail_encrypted: None,
        origin: Origin::Checkin,
        legal_basis: basis,
        created_at: now,
        updated_at: now,
        client_edited_at: None,
    }
}

/// Everything one check-in carries, resolved before the transaction starts.
#[derive(Clone)]
pub(super) struct Checkin {
    pub person_id: String,
    pub level: Level,
    pub declined: bool,
    pub data: PersonData,
    pub thumbnail: Option<Vec<u8>>,
    pub capture_id: Option<String>,
    pub shelter_id: String,
    pub shelter: ShelterDoc,
    pub event_id: Option<String>,
    pub legal_basis: LegalBasis,
    pub entered_at: FirestoreInstant,
    pub volunteer_id: String,
    /// The volunteer is `protection` or Civil Defense: the only ones who may change the child
    /// state of a person who is already registered.
    pub may_alter_child: bool,
    pub device: Option<String>,
    pub taken_over_from: Option<String>,
}

pub(super) struct Registered {
    pub person: PersonDoc,
    pub is_new: bool,
    /// The separation alert is owed for this status (spec 3.4.1, item 3).
    pub child_alert: Option<ChildStatus>,
}

/// The person after the check-in: created, or raised, edited and given a stay. A person who
/// already exists keeps their child state unless the volunteer may change it, so repeating
/// a check-in changes and raises nothing.
pub(super) fn register(
    existing: Option<PersonDoc>,
    c: &Checkin,
    now: FirestoreInstant,
) -> Result<Registered, ApiError> {
    let is_new = existing.is_none();
    let mut person =
        existing.unwrap_or_else(|| new_person(&c.person_id, c.level, c.legal_basis, now));
    person.level = person.level.raised_to(c.level);
    person.biometrics_declined |= c.declined;
    let child_alert = c.data.apply(&mut person, is_new || c.may_alter_child)?;
    if let Some(sealed) = &c.thumbnail {
        if person.name.is_none() && person.wristband.is_none() {
            person.thumbnail_encrypted = Some(sealed.clone());
        }
    }
    enter_shelter(
        &mut person,
        &c.shelter_id,
        c.event_id.as_deref(),
        c.entered_at,
        &c.volunteer_id,
        now,
    );
    person.updated_at = now;
    Ok(Registered {
        person,
        is_new,
        child_alert,
    })
}

// ---- edit ----

pub(super) enum Edit {
    /// A newer edit is already stored; the person is returned as it is (spec 9.4).
    Stale,
    Applied {
        child_alert: Option<ChildStatus>,
    },
}

/// The last edit wins: an older one is ignored. `may_alter` is the child-state permission of
/// whoever edits.
pub(super) fn edit(
    person: &mut PersonDoc,
    data: &PersonData,
    edited_at: FirestoreInstant,
    may_alter: bool,
    now: FirestoreInstant,
) -> Result<Edit, ApiError> {
    if person.client_edited_at.is_some_and(|last| last > edited_at) {
        return Ok(Edit::Stale);
    }
    let child_alert = data.apply(person, may_alter)?;
    person.client_edited_at = Some(edited_at);
    person.updated_at = now;
    Ok(Edit::Applied { child_alert })
}

// ---- lists ----

/// Cuts rows fetched with `limit + 1` back to a page and says whether there was one more.
pub(super) fn page<T>(mut rows: Vec<T>, limit: usize) -> (Vec<T>, bool) {
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    (rows, has_more)
}

// ---- late biometrics (spec 4.12) ----

/// `old` replaced by `new` in the guardians list; returns whether anything changed.
pub(super) fn repoint_guardians(guardians: &mut [Guardian], old: &str, new: &str) -> bool {
    let mut changed = false;
    for guardian in guardians.iter_mut().filter(|g| g.person_id == old) {
        guardian.person_id = new.into();
        changed = true;
    }
    changed
}

/// `old` replaced by `new` in the members and the head; `new` is not repeated.
pub(super) fn repoint_household(household: &mut HouseholdDoc, old: &str, new: &str) {
    let mut seen = HashSet::new();
    household.member_ids = household
        .member_ids
        .drain(..)
        .map(|member| {
            if member == old {
                new.to_string()
            } else {
                member
            }
        })
        .filter(|member| seen.insert(member.clone()))
        .collect();
    if household.head_id.as_deref() == Some(old) {
        household.head_id = Some(new.into());
    }
}

pub(super) fn without_member(household: &mut HouseholdDoc, id: &str) {
    household.member_ids.retain(|m| m != id);
    if household.head_id.as_deref() == Some(id) {
        household.head_id = None;
    }
}

/// Appends what `into` lacks, once each; a set keeps it linear in the size of both lists.
pub(super) fn union<T: Eq + Hash + Clone>(into: &mut Vec<T>, from: Vec<T>) {
    let mut seen: HashSet<T> = into.iter().cloned().collect();
    for item in from {
        if seen.insert(item.clone()) {
            into.push(item);
        }
    }
}

/// Folds the record `old` (a UUID, `no_biometrics`) into the person `new` that already existed
/// under the token: stays are joined and sorted, only the most recent open one stays open, and
/// what `new` lacks is filled from `old`. The household is decided by the caller. The second
/// value is the separation alert the fusion owes, if it made `new` more protected.
pub(super) fn merge_into(
    mut new: PersonDoc,
    old: PersonDoc,
    old_id: &str,
    level: Level,
) -> (PersonDoc, Option<ChildStatus>) {
    new.level = new.level.raised_to(level);
    let child_alert = children::merge_child_state(&mut new, &old);
    union(&mut new.former_ids, old.former_ids);
    union(&mut new.former_ids, vec![old_id.to_string()]);
    new.name = new.name.or(old.name);
    new.social_name = new.social_name.or(old.social_name);
    new.wristband = new.wristband.or(old.wristband);
    new.birth_year = new.birth_year.or(old.birth_year);
    if new.needs == Needs::default() {
        new.needs = old.needs;
    }
    if new.vulnerabilities == Vulnerabilities::default() {
        new.vulnerabilities = old.vulnerabilities;
    }
    let mut guardians = old.guardians;
    for guardian in new.guardians.drain(..) {
        guardians.retain(|g| g.person_id != guardian.person_id);
        guardians.push(guardian);
    }
    new.guardians = guardians;
    union(&mut new.event_ids, old.event_ids);
    union(&mut new.rescue_ids, old.rescue_ids);
    new.stays.extend(old.stays);
    new.stays.sort_by_key(|s| s.entered_at);
    let last_open = open_stay_index(&new.stays);
    for (i, stay) in new.stays.iter_mut().enumerate() {
        if stay.left_at.is_none() && Some(i) != last_open {
            stay.left_at = Some(store::now());
            stay.exit_reason = Some(ExitReason::CheckinElsewhere);
        }
    }
    new.current_shelter_id = last_open.map(|i| new.stays[i].shelter_id.clone());
    new.thumbnail_encrypted = new.thumbnail_encrypted.or(old.thumbnail_encrypted);
    drop_thumbnail_if_identified(&mut new);
    new.biometrics_declined = false;
    new.created_at = new.created_at.min(old.created_at);
    (new, child_alert)
}

/// What late biometrics writes besides the references: the person under the token, the
/// households that change, and the effects the fusion owes.
pub(super) struct MigrationPlan {
    /// To be stored under the new id; `id` is left empty.
    pub person: PersonDoc,
    pub old_household: Option<(String, HouseholdDoc)>,
    pub new_household: Option<(String, HouseholdDoc)>,
    /// `(old household, household of the token)` when both records were in different ones.
    pub household_conflict: Option<(Option<String>, Option<String>)>,
    pub child_alert: Option<ChildStatus>,
}

/// The record `old` moves to the token's id, or is merged into the one `existing` there. The
/// household of the old record moves to the new id; when the token's record already has another
/// household, that one stays and the admins hear about it.
pub(super) fn plan_migration(
    (old_id, new_id): (&str, &str),
    level: Level,
    old: &PersonDoc,
    existing: Option<PersonDoc>,
    (old_household, new_household): (Option<HouseholdDoc>, Option<HouseholdDoc>),
    now: FirestoreInstant,
) -> MigrationPlan {
    let mut household_conflict = None;
    let mut child_alert = None;
    let mut person = match existing {
        None => {
            let mut person = old.clone();
            person.id = Some(new_id.into());
            person.level = level;
            person.biometrics_declined = false;
            union(&mut person.former_ids, vec![old_id.to_string()]);
            person
        }
        Some(existing) => {
            let same_household = existing.household_id == old.household_id;
            let (mut merged, alert) = merge_into(existing.clone(), old.clone(), old_id, level);
            child_alert = alert;
            if existing.household_id.is_none() {
                merged.household_id = old.household_id.clone();
            } else if !same_household && old.household_id.is_some() {
                household_conflict =
                    Some((old.household_id.clone(), existing.household_id.clone()));
            }
            merged
        }
    };
    person.updated_at = now;
    person.id = None;

    let old_household = match (old.household_id.clone(), old_household) {
        (Some(id), Some(mut household)) => {
            if household_conflict.is_some() {
                without_member(&mut household, old_id);
            } else {
                repoint_household(&mut household, old_id, new_id);
            }
            household.updated_at = now;
            Some((id, household))
        }
        _ => None,
    };
    // Both records were members of different households: the token's one keeps its place.
    let new_household = match (new_household, person.household_id.clone()) {
        (Some(mut household), Some(id)) if !household.member_ids.iter().any(|m| m == new_id) => {
            household.member_ids.push(new_id.to_string());
            household.updated_at = now;
            Some((id, household))
        }
        _ => None,
    };
    MigrationPlan {
        person,
        old_household,
        new_household,
        household_conflict,
        child_alert,
    }
}

/// The token's record already exists: only the level can rise.
pub(super) fn raise_level(person: &mut PersonDoc, level: Level, now: FirestoreInstant) {
    person.level = person.level.raised_to(level);
    person.updated_at = now;
}
