//! The reads that feed the CSV of a shelter and the final numbers of an event.

use std::collections::HashMap;

use firestore::{FirestoreDb, FirestoreInstant, path};
use jiff::tz::TimeZone;
use tokio::task::JoinSet;

use super::model::Count;
use super::view::{
    age_band, at, child_status_tag, delivery_status_tag, display_name, entered_at, level_tag,
    needs_of, row, section, verification_tag, warn_if_cut, yes_no,
};
use crate::deliveries::{self, DeliveryDoc};
use crate::error::ApiError;
use crate::events::EventStats;
use crate::households::{self, HouseholdDoc};
use crate::people::{self, PersonDoc};
use crate::shelters::{self, ShelterDoc};
use crate::store::{self, fs_error};

/// Rows of one section; one more is read to know the section was cut, and a `# warning` row says so.
pub(super) const MAX_ROWS: u32 = 5000;
/// Household documents read at once.
const HOUSEHOLD_READS: usize = 16;

async fn people_in(
    db: &FirestoreDb,
    shelter_id: &str,
    max_rows: u32,
) -> Result<Vec<PersonDoc>, ApiError> {
    db.fluent()
        .select()
        .from(people::COLLECTION)
        .filter(|q| q.for_all([q.field(path!(PersonDoc::current_shelter_id)).eq(shelter_id)]))
        .limit(max_rows + 1)
        .obj()
        .query()
        .await
        .map_err(fs_error)
}

async fn deliveries_of(
    db: &FirestoreDb,
    shelter_id: &str,
    max_rows: u32,
) -> Result<Vec<DeliveryDoc>, ApiError> {
    db.fluent()
        .select()
        .from(deliveries::COLLECTION)
        .filter(|q| q.for_all([q.field(path!(DeliveryDoc::shelter_id)).eq(shelter_id)]))
        .limit(max_rows + 1)
        .obj()
        .query()
        .await
        .map_err(fs_error)
}

/// The household documents by id, read `HOUSEHOLD_READS` at a time.
async fn read_households(
    db: &FirestoreDb,
    ids: &[String],
) -> Result<HashMap<String, HouseholdDoc>, ApiError> {
    let mut found = HashMap::new();
    for chunk in ids.chunks(HOUSEHOLD_READS) {
        let mut tasks = JoinSet::new();
        for id in chunk {
            let (db, id) = (db.clone(), id.clone());
            tasks.spawn(async move {
                let doc = store::get::<HouseholdDoc>(&db, households::COLLECTION, &id).await?;
                Ok::<_, ApiError>((id, doc))
            });
        }
        while let Some(done) = tasks.join_next().await {
            let (id, doc) = done.map_err(|_| ApiError::internal())??;
            if let Some(doc) = doc {
                found.insert(
                    id.clone(),
                    HouseholdDoc {
                        id: Some(id),
                        ..doc
                    },
                );
            }
        }
    }
    Ok(found)
}

/// The whole CSV of a shelter at `now`; each section has at most `max_rows` rows.
pub(super) async fn build(
    db: &FirestoreDb,
    shelter_id: &str,
    now: FirestoreInstant,
    max_rows: u32,
) -> Result<String, ApiError> {
    let mut people = people_in(db, shelter_id, max_rows).await?;
    let people_cut = people.len() > max_rows as usize;
    people.truncate(max_rows as usize);
    // A stable order, so two exports of the same data are the same file.
    people.sort_by_key(|p| (p.created_at, p.wristband.clone(), p.name.clone()));

    // Household labels in the order the people first show them: `N{position + 1}`.
    let mut labels: Vec<String> = Vec::new();
    let mut label_index: HashMap<String, usize> = HashMap::new();
    for h in people.iter().filter_map(|p| p.household_id.as_ref()) {
        if !label_index.contains_key(h) {
            label_index.insert(h.clone(), labels.len());
            labels.push(h.clone());
        }
    }
    let label_of = |id: &str| {
        label_index
            .get(id)
            .map(|i| format!("N{}", i + 1))
            .unwrap_or_default()
    };
    let households_docs = read_households(db, &labels).await?;

    let year = now.to_zoned(TimeZone::UTC).year();
    let mut out = String::new();

    section(
        &mut out,
        "people",
        &[
            "name",
            "social_name",
            "wristband",
            "level",
            "age_band",
            "needs",
            "household",
            "household_role",
            "child_status",
            "entered_at",
            "registered_at",
            "updated_at",
        ],
    );
    for p in &people {
        let household = p.household_id.as_deref().map(label_of).unwrap_or_default();
        let role = match p.household_id.as_deref() {
            Some(id) => match households_docs.get(id) {
                Some(h) if h.head_id.is_some() && h.head_id == p.id => "head",
                _ => "member",
            },
            None => "",
        };
        row(
            &mut out,
            &[
                &display_name(p),
                p.social_name.as_deref().unwrap_or_default(),
                p.wristband.as_deref().unwrap_or_default(),
                level_tag(p.level),
                age_band(p.birth_year, year),
                &needs_of(p),
                &household,
                role,
                p.child_status.map(child_status_tag).unwrap_or_default(),
                &entered_at(p, shelter_id),
                &at(p.created_at),
                &at(p.updated_at),
            ],
        );
    }
    warn_if_cut(&mut out, "people", people_cut, max_rows);
    out.push_str("\r\n");

    section(
        &mut out,
        "households",
        &["household", "members", "animals", "has_card"],
    );
    // Only the members who are in this shelter; the others are another shelter's data.
    let mut members_of: HashMap<&str, Vec<String>> = HashMap::new();
    for p in &people {
        if let Some(id) = p.household_id.as_deref() {
            members_of
                .entry(id)
                .or_default()
                .push(match (&p.wristband, &p.name) {
                    (Some(w), Some(n)) => format!("{n} ({w})"),
                    (Some(w), None) => w.clone(),
                    (None, Some(n)) => n.clone(),
                    (None, None) => String::new(),
                });
        }
    }
    for (i, id) in labels.iter().enumerate() {
        let Some(h) = households_docs.get(id) else {
            continue;
        };
        let members = members_of
            .get(id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let animals: Vec<String> = h
            .pets
            .iter()
            .map(|p| format!("{} x{}", p.species, p.count))
            .collect();
        row(
            &mut out,
            &[
                &format!("N{}", i + 1),
                &members.join("; "),
                &animals.join("; "),
                yes_no(h.card_code.is_some()),
            ],
        );
    }
    out.push_str("\r\n");

    let mut given = deliveries_of(db, shelter_id, max_rows).await?;
    let deliveries_cut = given.len() > max_rows as usize;
    given.truncate(max_rows as usize);
    given.sort_by_key(|d| (d.delivered_at, d.item.clone()));
    section(
        &mut out,
        "deliveries",
        &["item", "quantity", "delivered_at", "status", "verification"],
    );
    for d in &given {
        row(
            &mut out,
            &[
                &d.item,
                &d.quantity.to_string(),
                &at(d.delivered_at),
                delivery_status_tag(d.status),
                verification_tag(d.verification),
            ],
        );
    }
    warn_if_cut(&mut out, "deliveries", deliveries_cut, max_rows);
    Ok(out)
}

// ---- events.stats ----

async fn count(
    db: &FirestoreDb,
    collection: &str,
    field: &str,
    value: &str,
    array: bool,
) -> Result<u32, ApiError> {
    let counted: Vec<Count> = db
        .fluent()
        .select()
        .from(collection)
        .filter(|q| {
            if array {
                q.for_all([q.field(field).array_contains(value)])
            } else {
                q.for_all([q.field(field).eq(value)])
            }
        })
        .aggregate(|a| a.fields([a.field("n").count()]))
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    counted
        .first()
        .map_or(Ok(0), |c| crate::dashboard::count_value(c.n))
}

/// The final numbers of an event, for `events.stats` before its data is discarded (spec 7):
/// counts only, nothing about a person. Households are the ones created in a shelter of the event.
#[allow(dead_code)] // the discard (purge.rs) calls it
pub async fn event_stats(db: &FirestoreDb, event_id: &str) -> Result<EventStats, ApiError> {
    let shelters: Vec<ShelterDoc> = db
        .fluent()
        .select()
        .from(shelters::COLLECTION)
        .filter(|q| q.for_all([q.field(path!(ShelterDoc::event_id)).eq(event_id)]))
        .obj()
        .query()
        .await
        .map_err(fs_error)?;
    let mut households = 0;
    for s in &shelters {
        let id = s.id.as_deref().unwrap_or_default();
        households += count(db, households::COLLECTION, "created_shelter_id", id, false).await?;
    }
    Ok(EventStats {
        shelters: u32::try_from(shelters.len()).unwrap_or(u32::MAX),
        unique_people: count(db, people::COLLECTION, "event_ids", event_id, true).await?,
        households,
        rescues: count(db, crate::rescues::COLLECTION, "event_id", event_id, false).await?,
        deliveries: count(db, deliveries::COLLECTION, "event_id", event_id, false).await?,
    })
}
