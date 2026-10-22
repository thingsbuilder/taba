//! The rules of mediation (spec 4.8, 11.1): who attends which queue, how a listing is ordered,
//! how a decision applies and what the first message to the family says. Everything here is pure,
//! so each rule is tested without Firestore.

use firestore::FirestoreInstant;
use serde::Deserialize;

use super::model::{
    DecidedBy, MAX_NOTE_CHARS, MatchDoc, MatchStatus, MediatorKind, forbidden, invalid_request,
};
use crate::auth::Volunteer;
use crate::error::ApiError;
use crate::members::ShelterRole;
use crate::people::PersonDoc;
use crate::searches::Family;
use crate::shelters::{Confirmation, ShelterDoc};
use crate::volunteers::GlobalRole;

/// The queue of a match for `person`, who is now in `shelter` if anywhere (spec 4.8): a child goes
/// to `protection` whatever the shelter; a confirmed shelter keeps its own match (and its id);
/// an unconfirmed shelter or none goes to `central`.
pub fn queue_for(
    person: &PersonDoc,
    shelter: Option<&ShelterDoc>,
) -> (MediatorKind, Option<String>) {
    if person.is_child {
        return (MediatorKind::Protection, None);
    }
    match shelter {
        Some(s) if s.confirmation == Confirmation::Confirmed && s.id.is_some() => {
            (MediatorKind::Shelter, s.id.clone())
        }
        _ => (MediatorKind::Central, None),
    }
}

/// Whether `volunteer` attends the queue of `m` (spec 4.8, 2.3): `protection` and the Civil Defense
/// the protection queue; `mediator` and the Civil Defense the central one; the shelter queue goes
/// to the admins of that confirmed shelter, to any `mediator` and to the Civil Defense.
/// A child's match is on the protection queue, so the admins of a shelter never reach it.
pub(super) fn attends(volunteer: &Volunteer, m: &MatchDoc) -> bool {
    let civil_defense = volunteer.has_global(GlobalRole::CivilDefense);
    let mediator = volunteer.has_global(GlobalRole::Mediator);
    match m.mediator_kind {
        MediatorKind::Protection => civil_defense || volunteer.has_global(GlobalRole::Protection),
        MediatorKind::Central => civil_defense || mediator,
        MediatorKind::Shelter => {
            civil_defense
                || mediator
                || m.mediator_shelter_id.as_deref().is_some_and(|shelter| {
                    volunteer.memberships.iter().any(|membership| {
                        membership.shelter_id == shelter
                            && membership.role == ShelterRole::Admin
                            && membership.confirmation == Confirmation::Confirmed
                    })
                })
        }
    }
}

/// `mediator`, `protection` and the Civil Defense see a person's history while attending.
pub(super) fn sees_history(volunteer: &Volunteer) -> bool {
    volunteer.has_global(GlobalRole::Mediator)
        || volunteer.has_global(GlobalRole::Protection)
        || volunteer.has_global(GlobalRole::CivilDefense)
}

/// The queues a volunteer attends: a kind, and one shelter or the whole network.
pub(super) fn queues_of(volunteer: &Volunteer) -> Vec<(MediatorKind, Option<String>)> {
    let civil_defense = volunteer.has_global(GlobalRole::CivilDefense);
    let mut queues = vec![];
    if civil_defense || volunteer.has_global(GlobalRole::Protection) {
        queues.push((MediatorKind::Protection, None));
    }
    if civil_defense || volunteer.has_global(GlobalRole::Mediator) {
        queues.push((MediatorKind::Central, None));
        queues.push((MediatorKind::Shelter, None));
    } else {
        for membership in &volunteer.memberships {
            if membership.role == ShelterRole::Admin
                && membership.confirmation == Confirmation::Confirmed
            {
                queues.push((MediatorKind::Shelter, Some(membership.shelter_id.clone())));
            }
        }
    }
    queues
}

/// Keeps the first `limit` rows of a page read with `limit + 1`; the flag says there was one more.
pub(super) fn cap<T>(mut rows: Vec<T>, limit: u32) -> (Vec<T>, bool) {
    let limit = limit as usize;
    let more = rows.len() > limit;
    rows.truncate(limit);
    (rows, more)
}

/// Open matches first, then the decided ones; each group newest first.
pub(super) fn sort_for_listing(found: &mut [MatchDoc]) {
    found.sort_by_key(|m| {
        (
            m.status != MatchStatus::Open,
            std::cmp::Reverse(m.created_at),
        )
    });
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Decision {
    Accept,
    Decline,
    NotThePerson,
}

impl Decision {
    pub(super) fn is_accept(self) -> bool {
        matches!(self, Decision::Accept)
    }

    pub(super) fn status(self) -> MatchStatus {
        match self {
            Decision::Accept => MatchStatus::Accepted,
            Decision::Decline => MatchStatus::Declined,
            Decision::NotThePerson => MatchStatus::NotThePerson,
        }
    }
}

/// The note of a decision, trimmed; an empty one is none.
pub(super) fn clean_note(note: Option<String>) -> Result<Option<String>, ApiError> {
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    if note
        .as_deref()
        .is_some_and(|n| n.chars().count() > MAX_NOTE_CHARS)
    {
        return Err(invalid_request("note is too long".into()));
    }
    Ok(note)
}

/// `m` as decided by `volunteer`. Only who attends the match decides, and only while it is open:
/// the first decision closes it.
pub(super) fn decide(
    volunteer: &Volunteer,
    mut m: MatchDoc,
    decision: Decision,
    decided_by: DecidedBy,
    note: Option<String>,
    at: FirestoreInstant,
) -> Result<MatchDoc, ApiError> {
    if !attends(volunteer, &m) {
        return Err(forbidden());
    }
    if m.status != MatchStatus::Open {
        return Err(ApiError::match_already_decided());
    }
    m.status = decision.status();
    m.decided_by = Some(decided_by);
    m.decided_by_volunteer = Some(volunteer.id.clone());
    m.decided_at = Some(at);
    m.note = note;
    Ok(m)
}

fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The first message to the family (spec 11.1), in the language they chose. It says nothing
/// about where the person is. An unknown language gets Portuguese.
pub(super) fn whatsapp_message(language: &str) -> &'static str {
    match language {
        "en" => {
            "Hello! This is about the search you registered on Taba. We found the person you are looking for and the contact with you was authorized. Can we talk here?"
        }
        "es" => {
            "¡Hola! Te escribimos por la búsqueda que registraste en Taba. Encontramos a la persona que buscas y se autorizó el contacto contigo. ¿Podemos hablar por aquí?"
        }
        _ => {
            "Olá! Falamos sobre a busca que você registrou no Taba. Encontramos a pessoa que você procura e o contato com você foi autorizado. Podemos conversar por aqui?"
        }
    }
}

pub(super) fn whatsapp_link(family: &Family) -> String {
    let digits: String = family.phone.chars().filter(char::is_ascii_digit).collect();
    format!(
        "https://wa.me/{digits}?text={}",
        percent_encode(whatsapp_message(&family.language))
    )
}
