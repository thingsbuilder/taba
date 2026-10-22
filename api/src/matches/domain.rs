//! The rules of mediation (spec 4.8, 11.1): who attends which queue, how a listing is ordered,
//! how a decision applies and what the first message to the family says. Everything here is pure,
//! so each rule is tested without Firestore.

use firestore::FirestoreInstant;
use serde::Deserialize;

use super::{
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
fn whatsapp_message(language: &str) -> &'static str {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Membership;
    use crate::store;
    use crate::volunteers::VolunteerDoc;

    fn volunteer(roles: &[GlobalRole], admin_of: &[(&str, Confirmation)]) -> Volunteer {
        let now = store::now();
        Volunteer {
            id: "v".into(),
            doc: VolunteerDoc {
                name: "V".into(),
                phone: None,
                show_phone: false,
                language: "pt-BR".into(),
                global_roles: roles.to_vec(),
                role_grants: vec![],
                banned: false,
                banned_by: None,
                banned_at: None,
                flagged: false,
                flag_reasons: vec![],
                created_at: now,
                last_seen_at: now,
            },
            memberships: admin_of
                .iter()
                .map(|(shelter, confirmation)| Membership {
                    shelter_id: shelter.to_string(),
                    name: String::new(),
                    role: ShelterRole::Admin,
                    confirmation: *confirmation,
                    event_id: None,
                })
                .collect(),
            session_id: "s".into(),
        }
    }

    fn open_match(kind: MediatorKind, shelter: Option<&str>) -> MatchDoc {
        MatchDoc {
            id: None,
            search_id: "s".into(),
            person_id: "p".into(),
            is_child: kind == MediatorKind::Protection,
            mediator_kind: kind,
            mediator_shelter_id: shelter.map(str::to_string),
            status: MatchStatus::Open,
            decided_by: None,
            decided_by_volunteer: None,
            decided_at: None,
            note: None,
            created_at: store::now(),
        }
    }

    #[test]
    fn a_page_read_with_one_extra_row_is_cut_and_flagged_only_past_the_limit() {
        for (rows, kept, more) in [(99, 99, false), (100, 100, false), (101, 100, true)] {
            let (page, flag) = cap((0..rows).collect::<Vec<_>>(), 100);
            assert_eq!((page.len(), flag), (kept, more), "{rows} rows");
        }
        // The first rows are the ones kept: the page is newest first.
        assert_eq!(cap(vec![1, 2, 3], 2), (vec![1, 2], true));
    }

    /// Every queue a volunteer is sent to by `queues_of` is one they attend, and a match they
    /// attend lives in one of those queues: nothing attended is out of reach of the listing.
    #[test]
    fn the_queues_listed_are_exactly_the_ones_attended() {
        let volunteers = [
            volunteer(&[GlobalRole::CivilDefense], &[]),
            volunteer(&[GlobalRole::Mediator], &[]),
            volunteer(&[GlobalRole::Protection], &[]),
            volunteer(&[GlobalRole::Validator], &[]),
            volunteer(&[], &[("a", Confirmation::Confirmed)]),
            volunteer(&[], &[("a", Confirmation::Unconfirmed)]),
            volunteer(&[], &[]),
        ];
        let matches = [
            open_match(MediatorKind::Protection, None),
            open_match(MediatorKind::Central, None),
            open_match(MediatorKind::Shelter, Some("a")),
            open_match(MediatorKind::Shelter, Some("b")),
        ];
        for v in &volunteers {
            let queues = queues_of(v);
            for m in &matches {
                let reached = queues.iter().any(|(kind, shelter)| {
                    *kind == m.mediator_kind
                        && shelter
                            .as_deref()
                            .is_none_or(|s| m.mediator_shelter_id.as_deref() == Some(s))
                });
                assert_eq!(attends(v, m), reached, "{:?} {:?}", v.memberships, m);
            }
        }
    }

    #[test]
    fn only_who_attends_decides_and_only_once() {
        let admin = volunteer(&[], &[("a", Confirmation::Confirmed)]);
        let other = volunteer(&[], &[("b", Confirmation::Confirmed)]);
        let m = open_match(MediatorKind::Shelter, Some("a"));
        let at = store::now();
        let decide_as = |who: &Volunteer, m: MatchDoc| {
            decide(who, m, Decision::Decline, DecidedBy::Person, None, at)
        };
        assert_eq!(decide_as(&other, m.clone()).unwrap_err().code, "forbidden");
        let decided = decide_as(&admin, m).unwrap();
        assert_eq!(
            (
                decided.status,
                decided.decided_by,
                decided.decided_by_volunteer.as_deref()
            ),
            (MatchStatus::Declined, Some(DecidedBy::Person), Some("v"))
        );
        // Deciding it again, whatever the decision, changes nothing and says so.
        for decision in [Decision::Accept, Decision::Decline, Decision::NotThePerson] {
            let again = decide(
                &admin,
                decided.clone(),
                decision,
                DecidedBy::Operator,
                None,
                at,
            );
            assert_eq!(again.unwrap_err().code, "match_already_decided");
        }
    }

    #[test]
    fn a_note_is_trimmed_and_bounded() {
        assert_eq!(clean_note(None).unwrap(), None);
        assert_eq!(clean_note(Some("  ".into())).unwrap(), None);
        assert_eq!(
            clean_note(Some(" ok ".into())).unwrap().as_deref(),
            Some("ok")
        );
        assert!(clean_note(Some("x".repeat(MAX_NOTE_CHARS))).is_ok());
        assert_eq!(
            clean_note(Some("x".repeat(MAX_NOTE_CHARS + 1)))
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }

    #[test]
    fn the_whatsapp_link_keeps_only_the_digits_and_encodes_the_text() {
        let family = |phone: &str, language: &str| Family {
            name: "J".into(),
            phone: phone.into(),
            relationship: "x".into(),
            language: language.into(),
        };
        let link = whatsapp_link(&family("+55 (51) 99999-0000", "en"));
        let (head, text) = link.split_once("?text=").unwrap();
        assert_eq!(head, "https://wa.me/5551999990000");
        assert!(text.starts_with("Hello%21%20"));
        assert!(
            text.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"%-._~".contains(&b))
        );
        assert_ne!(whatsapp_message("pt-BR"), whatsapp_message("en"));
        assert_ne!(whatsapp_message("es"), whatsapp_message("en"));
        // no place, no shelter, no name in the first message
        for language in ["pt-BR", "en", "es"] {
            let text = whatsapp_message(language).to_lowercase();
            assert!(
                !text.contains("abrigo") && !text.contains("shelter") && !text.contains("refugio")
            );
        }
    }
}
