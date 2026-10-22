//! The export of a shelter (spec 8.8, Q22): `GET /v1/shelters/{id}/export.csv`, for an admin of a
//! `confirmed` shelter: its people, households and deliveries, no thumbnail, no `person_id`, no
//! token and no card code, audited. Also the `stats` an event keeps when it is discarded.
//!
//! One CSV with three sections. Each section opens with a `# name` line and a column header, and
//! ends with a blank line. Households are labelled `N1`, `N2`, ... for this export only, so a row
//! of `people` can point at its household without an id. `# warning` lines say a section was cut.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use firestore::{FirestoreDb, FirestoreInstant, path};
use jiff::tz::TimeZone;
use serde::Deserialize;

use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::deliveries::{self, DeliveryDoc};
use crate::error::ApiError;
use crate::events::EventStats;
use crate::households::{self, HouseholdDoc};
use crate::people::{self, PersonDoc};
use crate::shelters::{self, Confirmation, ShelterDoc};
use crate::store::{self, fs_error};

/// Rows of one section; one more is read to know the section was cut.
const MAX_ROWS: u32 = 5000;

// ---- CSV ----

/// A cell: a leading `= + - @`, tab or CR would run as a formula in a spreadsheet, so it gets a
/// `'` in front; quotes, commas and line breaks are escaped as RFC 4180 says.
fn cell(value: &str) -> String {
    let guarded = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_string()
    };
    if guarded.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", guarded.replace('"', "\"\""))
    } else {
        guarded
    }
}

fn row(out: &mut String, cells: &[&str]) {
    let line: Vec<String> = cells.iter().map(|c| cell(c)).collect();
    out.push_str(&line.join(","));
    out.push_str("\r\n");
}

fn section(out: &mut String, name: &str, columns: &[&str]) {
    out.push_str(&format!("# {name}\r\n"));
    row(out, columns);
}

fn warn_if_cut(out: &mut String, name: &str, cut: bool) {
    if cut {
        row(
            out,
            &["# warning", "truncated", name, &MAX_ROWS.to_string()],
        );
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

// ---- the data ----

/// `0-11`, `12-17`, `18-59`, `60+`; empty when the birth year is unknown.
fn age_band(birth_year: Option<u16>, year: i16) -> &'static str {
    let Some(born) = birth_year else { return "" };
    match i32::from(year) - i32::from(born) {
        ..=11 => "0-11",
        12..=17 => "12-17",
        18..=59 => "18-59",
        _ => "60+",
    }
}

fn needs_of(person: &PersonDoc) -> String {
    let n = &person.needs;
    [
        (n.continuous_medication, "continuous_medication"),
        (n.dietary_restriction, "dietary_restriction"),
        (n.reduced_mobility, "reduced_mobility"),
        (n.health_follow_up, "health_follow_up"),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .map(|(_, name)| *name)
    .collect::<Vec<_>>()
    .join("|")
}

/// The serde name of a unit enum (`verified`, `in_review`, ...).
fn tag<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn display_name(person: &PersonDoc) -> String {
    person.name.clone().unwrap_or_default()
}

async fn people_in(db: &FirestoreDb, shelter_id: &str) -> Result<Vec<PersonDoc>, ApiError> {
    db.fluent()
        .select()
        .from(people::COLLECTION)
        .filter(|q| q.for_all([q.field(path!(PersonDoc::current_shelter_id)).eq(shelter_id)]))
        .limit(MAX_ROWS + 1)
        .obj()
        .query()
        .await
        .map_err(fs_error)
}

async fn deliveries_of(db: &FirestoreDb, shelter_id: &str) -> Result<Vec<DeliveryDoc>, ApiError> {
    db.fluent()
        .select()
        .from(deliveries::COLLECTION)
        .filter(|q| q.for_all([q.field(path!(DeliveryDoc::shelter_id)).eq(shelter_id)]))
        .limit(MAX_ROWS + 1)
        .obj()
        .query()
        .await
        .map_err(fs_error)
}

fn at(instant: FirestoreInstant) -> String {
    instant.to_string()
}

/// The stay of `person` in `shelter_id` that is still open: when they entered.
fn entered_at(person: &PersonDoc, shelter_id: &str) -> String {
    person
        .stays
        .iter()
        .rev()
        .find(|s| s.shelter_id == shelter_id && s.left_at.is_none())
        .map(|s| at(s.entered_at))
        .unwrap_or_default()
}

/// The whole CSV of a shelter at `now`.
async fn build(
    db: &FirestoreDb,
    shelter_id: &str,
    now: FirestoreInstant,
) -> Result<String, ApiError> {
    let mut people = people_in(db, shelter_id).await?;
    let people_cut = people.len() > MAX_ROWS as usize;
    people.truncate(MAX_ROWS as usize);
    // A stable order, so two exports of the same data are the same file.
    people.sort_by_key(|p| (p.created_at, p.wristband.clone(), p.name.clone()));

    let mut labels: Vec<String> = Vec::new(); // household id by position: label N{i+1}
    for p in &people {
        if let Some(h) = &p.household_id {
            if !labels.contains(h) {
                labels.push(h.clone());
            }
        }
    }
    let label_of = |id: &str| {
        labels
            .iter()
            .position(|h| h == id)
            .map(|i| format!("N{}", i + 1))
            .unwrap_or_default()
    };
    // ponytail: one read per household; a batch read if a shelter ever has thousands of them.
    let mut households_docs: Vec<HouseholdDoc> = Vec::new();
    for id in &labels {
        if let Some(h) = store::get::<HouseholdDoc>(db, households::COLLECTION, id).await? {
            households_docs.push(HouseholdDoc {
                id: Some(id.clone()),
                ..h
            });
        }
    }

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
        let role = match (
            &p.household_id,
            households_docs.iter().find(|h| h.id == p.household_id),
        ) {
            (Some(_), Some(h)) if h.head_id.is_some() && h.head_id == p.id => "head",
            (Some(_), _) => "member",
            _ => "",
        };
        row(
            &mut out,
            &[
                &display_name(p),
                p.social_name.as_deref().unwrap_or_default(),
                p.wristband.as_deref().unwrap_or_default(),
                &tag(&p.level),
                age_band(p.birth_year, year),
                &needs_of(p),
                &household,
                role,
                &p.child_status.map(|s| tag(&s)).unwrap_or_default(),
                &entered_at(p, shelter_id),
                &at(p.created_at),
                &at(p.updated_at),
            ],
        );
    }
    warn_if_cut(&mut out, "people", people_cut);
    out.push_str("\r\n");

    section(
        &mut out,
        "households",
        &["household", "members", "animals", "has_card"],
    );
    for (i, id) in labels.iter().enumerate() {
        let Some(h) = households_docs.iter().find(|h| h.id.as_deref() == Some(id)) else {
            continue;
        };
        // Only the members who are in this shelter; the others are another shelter's data.
        let members: Vec<String> = people
            .iter()
            .filter(|p| p.household_id.as_deref() == Some(id))
            .map(|p| match (&p.wristband, &p.name) {
                (Some(w), Some(n)) => format!("{n} ({w})"),
                (Some(w), None) => w.clone(),
                (None, Some(n)) => n.clone(),
                (None, None) => String::new(),
            })
            .collect();
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

    let mut given = deliveries_of(db, shelter_id).await?;
    let deliveries_cut = given.len() > MAX_ROWS as usize;
    given.truncate(MAX_ROWS as usize);
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
                &tag(&d.status),
                &tag(&d.verification),
            ],
        );
    }
    warn_if_cut(&mut out, "deliveries", deliveries_cut);
    Ok(out)
}

/// `GET /v1/shelters/{id}/export.csv`: `403` for anyone but an admin of a `confirmed` shelter.
/// Reading, so `events::ensure_open` does not apply: a closed event can still be exported.
pub async fn export_csv(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(shelter_id): Path<String>,
) -> Result<Response, ApiError> {
    let forbidden = || ApiError::new(StatusCode::FORBIDDEN, "forbidden");
    if !volunteer.is_admin_of(&shelter_id) {
        return Err(forbidden());
    }
    let shelter: ShelterDoc = store::get(&state.db, shelters::COLLECTION, &shelter_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "shelter_not_found"))?;
    if shelter.confirmation != Confirmation::Confirmed {
        return Err(forbidden());
    }
    let now = store::now();
    let body = build(&state.db, &shelter_id, now).await?;
    let entry = AuditEntry::new("export.shelter", "shelter", &shelter_id)
        .by(&volunteer.id)
        .in_shelter(&shelter_id)
        .basis(LegalBasis::PublicPolicy);
    store::transact(&state.db, |db, tx| {
        let entry = entry.clone();
        Box::pin(async move { audit::add(&db, tx, &entry) })
    })
    .await?;
    let day = now.to_zoned(TimeZone::UTC).date();
    let disposition = format!("attachment; filename=\"taba-shelter-{shelter_id}-{day}.csv\"");
    Ok((
        [
            (CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (CACHE_CONTROL, "no-store".to_string()),
            (CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
        .into_response())
}

// ---- events.stats ----

#[derive(Deserialize)]
struct Count {
    n: i64,
}

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
    Ok(counted
        .first()
        .map_or(0, |c| u32::try_from(c.n).unwrap_or(0)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deliveries::{DeliveryStatus, ItemScope, Verification};
    use crate::exits::fixtures::{person, put};
    use crate::households::Pet;
    use crate::people::{Level, Needs};
    use crate::rescues::RescueDoc;
    use crate::test_support::{Actor, ORIGIN, TestApp, all_docs, new_id, north_of_origin};
    use crate::volunteers::GlobalRole;
    use axum::body::Body;
    use axum::http::{HeaderMap, Request};
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as STANDARD_B64;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const CARD: &str = "ZK7Q2M9X";

    /// The raw answer: the CSV is not JSON.
    async fn get_csv(
        app: &TestApp,
        path: &str,
        session: Option<&str>,
    ) -> (StatusCode, HeaderMap, String) {
        let mut request = Request::builder().method("GET").uri(path);
        if let Some(session) = session {
            request = request.header("authorization", format!("Bearer {session}"));
        }
        let response = crate::router(app.state.clone())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let (status, headers) = (response.status(), response.headers().clone());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
    }

    fn url(shelter: &str) -> String {
        format!("/v1/shelters/{shelter}/export.csv")
    }

    async fn delivery(app: &TestApp, shelter: &str, event: Option<&str>, item: &str, person: &str) {
        let now = store::now();
        let doc = DeliveryDoc {
            shelter_id: shelter.into(),
            shelter_name: "Shelter".into(),
            event_id: event.map(str::to_string),
            person_id: Some(person.into()),
            household_id: Some("hh-secret".into()),
            household_pseudonym: None,
            card_code: Some(CARD.into()),
            item: item.into(),
            scope: ItemScope::PerPerson,
            quantity: 2,
            delivered_by: "vol".into(),
            delivered_at: now,
            received_at: now,
            verification: Verification::Card,
            approved_by: None,
            status: DeliveryStatus::Ok,
            reasons: vec![],
            offline_claimed: false,
            delivered_at_clamped: false,
            reviewed_by: None,
            review_note: None,
            pseudonymized: false,
        };
        store::replace(&app.state.db, deliveries::COLLECTION, &new_id(), &doc)
            .await
            .unwrap();
    }

    async fn household(app: &TestApp, id: &str, shelter: &str, head: &str, members: &[&str]) {
        let now = store::now();
        let doc = HouseholdDoc {
            id: None,
            member_ids: members.iter().map(|m| m.to_string()).collect(),
            head_id: Some(head.into()),
            pets: vec![Pet {
                species: "dog".into(),
                count: 2,
            }],
            card_code: Some(CARD.into()),
            created_by: "vol".into(),
            created_shelter_id: shelter.into(),
            client_created_at: now,
            created_at: now,
            updated_at: now,
        };
        store::replace(&app.state.db, households::COLLECTION, id, &doc)
            .await
            .unwrap();
    }

    struct World {
        app: TestApp,
        admin: Actor,
        shelter: String,
        other: String,
        ids: Vec<String>,
    }

    /// A confirmed shelter with three people (two in a household with a card, a pet and a
    /// thumbnail), two deliveries; another shelter with a person and a delivery of its own.
    async fn world() -> World {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.confirmed_shelter(&admin, "Gym", ORIGIN).await;
        let boss = app.actor("Boss").await;
        let other = app
            .confirmed_shelter(&boss, "School", north_of_origin(5000.0))
            .await;
        let (a, b, c, z) = (new_id(), new_id(), new_id(), new_id());

        let mut ana = person(Level::Verified, Some(&shelter));
        ana.name = Some("=HYPERLINK(\"http://x\")".into());
        ana.wristband = Some("W1".into());
        ana.household_id = Some("hh-1".into());
        ana.thumbnail_encrypted = Some(b"ENCRYPTED-THUMB-BYTES".to_vec());
        ana.needs = Needs {
            continuous_medication: true,
            reduced_mobility: true,
            ..Needs::default()
        };
        let mut joao = person(Level::NoBiometrics, Some(&shelter));
        joao.name = Some("Joao, \"Jo\"\nSilva".into());
        joao.social_name = Some("@Jo".into());
        joao.household_id = Some("hh-1".into());
        joao.birth_year = Some(2020);
        let mut eva = person(Level::Assisted, Some(&shelter));
        eva.name = None;
        eva.wristband = Some("W3".into());
        let mut zed = person(Level::Verified, Some(&other));
        zed.name = Some("Zed Elsewhere".into());
        for (id, p) in [(&a, &ana), (&b, &joao), (&c, &eva), (&z, &zed)] {
            put(&app, id, p).await;
        }
        household(&app, "hh-1", &shelter, &a, &[&a, &b]).await;
        delivery(&app, &shelter, None, "blanket", &a).await;
        delivery(&app, &shelter, None, "=soap", &c).await;
        delivery(&app, &other, None, "elsewhere-item", &z).await;
        World {
            app,
            admin,
            shelter,
            other,
            ids: vec![a, b, c, z],
        }
    }

    #[test]
    fn cells_are_guarded_against_formulas_and_escaped() {
        assert_eq!(cell("plain"), "plain");
        for start in ["=1+1", "+1", "-1", "@SUM(A1)", "\tx", "\rx"] {
            assert!(cell(start).trim_matches('"').starts_with('\''), "{start:?}");
        }
        assert_eq!(cell("a,b"), "\"a,b\"");
        assert_eq!(cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(cell("two\nlines"), "\"two\nlines\"");
        assert_eq!(cell("=a,b"), "\"'=a,b\"");
        assert_eq!(cell(""), "");
    }

    #[test]
    fn age_bands_follow_the_birth_year() {
        for (born, band) in [
            (2026, "0-11"),
            (2015, "0-11"),
            (2014, "12-17"),
            (2009, "12-17"),
            (2008, "18-59"),
            (1967, "18-59"),
            (1966, "60+"),
        ] {
            assert_eq!(age_band(Some(born), 2026), band, "{born}");
        }
        assert_eq!(age_band(None, 2026), "");
    }

    #[tokio::test]
    async fn only_an_admin_of_a_confirmed_shelter_exports() {
        let w = world().await;
        let app = &w.app;
        let (status, headers, body) = get_csv(app, &url(&w.shelter), Some(&w.admin.secret)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.starts_with("# people\r\n"));
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(headers["content-type"], "text/csv; charset=utf-8");
        let disposition = headers["content-disposition"].to_str().unwrap();
        assert!(disposition.starts_with("attachment; filename=\"taba-shelter-"));
        assert!(disposition.ends_with(".csv\""));

        let (status, _, _) = get_csv(app, &url(&w.shelter), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // An ordinary volunteer of the shelter.
        let code = app.invite(&w.admin, &w.shelter, "qr", "volunteer").await;
        let volunteer = app.join(&code, "Vera").await;
        let (status, _, body) = get_csv(app, &url(&w.shelter), Some(&volunteer.secret)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

        // Global roles without a link to the shelter, and an admin of another shelter.
        for (name, role) in [
            ("Val", GlobalRole::Validator),
            ("Dee", GlobalRole::CivilDefense),
            ("Med", GlobalRole::Mediator),
            ("Pro", GlobalRole::Protection),
        ] {
            let who = app.actor(name).await;
            app.grant(&who, role).await;
            let (status, _, _) = get_csv(app, &url(&w.shelter), Some(&who.secret)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{name}");
        }
        let boss = app.actor("Boss").await;
        let (status, _, _) = get_csv(app, &url(&w.shelter), Some(&boss.secret)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = get_csv(app, &url(&w.other), Some(&w.admin.secret)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = get_csv(app, &url("nope"), Some(&w.admin.secret)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_unconfirmed_shelter_does_not_export() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let shelter = app.create_shelter(&admin, "Gym", ORIGIN).await;
        let (status, _, _) = get_csv(&app, &url(&shelter), Some(&admin.secret)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            !crate::test_support::audit_actions(&app.state.db)
                .await
                .contains(&"export.shelter".to_string())
        );
    }

    #[tokio::test]
    async fn the_csv_has_the_shelter_data_and_no_identifier() {
        let w = world().await;
        let (_, _, body) = get_csv(&w.app, &url(&w.shelter), Some(&w.admin.secret)).await;

        for section in ["# people", "# households", "# deliveries"] {
            assert_eq!(body.matches(section).count(), 1, "{section}");
        }
        for expected in [
            "W1",
            "W3",
            "verified",
            "no_biometrics",
            "assisted",
            "blanket",
            "dog x2",
        ] {
            assert!(body.contains(expected), "{expected}\n{body}");
        }
        assert!(body.contains("continuous_medication|reduced_mobility"));
        assert!(body.contains("0-11"));
        // Formula guard and escaping.
        assert!(body.contains("\"'=HYPERLINK(\"\"http://x\"\")\""), "{body}");
        assert!(body.contains("'@Jo"));
        assert!(body.contains("\"Joao, \"\"Jo\"\"\nSilva\""));
        assert!(body.contains("'=soap"));
        // The household has a card, but not its code.
        assert!(body.contains(",yes\r\n"));
        assert!(body.contains("N1"));
        // Nothing of the other shelter.
        for foreign in ["Zed", "elsewhere-item"] {
            assert!(!body.contains(foreign), "{foreign}");
        }
        // No person id, card code, household id, token or thumbnail, by pattern.
        for id in &w.ids {
            assert!(!body.contains(id.as_str()), "person id {id}");
        }
        assert!(!body.contains(CARD));
        assert!(!body.contains("hh-1") && !body.contains("hh-secret"));
        assert!(!body.contains("tok-"));
        assert!(!body.contains("ENCRYPTED-THUMB"));
        assert!(!body.contains(&STANDARD_B64.encode(b"ENCRYPTED-THUMB-BYTES")));
        let uuid_like = body
            .split(|c: char| !(c.is_ascii_hexdigit() || c == '-'))
            .any(|w| w.len() == 36 && w.matches('-').count() == 4);
        assert!(!uuid_like, "a UUID in the export");
        let long_base64 = body
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='))
            .any(|w| w.len() >= 40);
        assert!(!long_base64, "a long base64 blob in the export");
        let people_header = body.lines().nth(1).unwrap();
        assert!(!people_header.contains("id") && !people_header.contains("thumbnail"));
    }

    #[tokio::test]
    async fn the_export_is_audited_without_its_content() {
        let w = world().await;
        let (status, _, _) = get_csv(&w.app, &url(&w.shelter), Some(&w.admin.secret)).await;
        assert_eq!(status, StatusCode::OK);
        let entries: Vec<_> = all_docs(&w.app.state.db, "audit_log")
            .await
            .into_iter()
            .filter(|e| e["action"] == "export.shelter")
            .collect();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry["volunteer_id"], w.admin.id);
        assert_eq!(entry["shelter_id"], w.shelter);
        assert_eq!(entry["target_kind"], "shelter");
        assert_eq!(entry["target_id"], w.shelter);
        assert_eq!(entry["legal_basis"], "public_policy");
        let text = entry.to_string();
        for secret in ["W1", "Joao", "blanket"] {
            assert!(!text.contains(secret));
        }
    }

    #[tokio::test]
    async fn event_stats_count_the_event_without_personal_data() {
        let app = TestApp::new().await;
        let admin = app.actor("Admin").await;
        let s1 = app.confirmed_shelter(&admin, "Gym", ORIGIN).await;
        let boss = app.actor("Boss").await;
        let s2 = app
            .confirmed_shelter(&boss, "School", north_of_origin(5000.0))
            .await;
        let first: ShelterDoc = store::get(&app.state.db, shelters::COLLECTION, &s1)
            .await
            .unwrap()
            .unwrap();
        let event = first.event_id.clone().expect("a shelter has an event");
        let second: ShelterDoc = store::get(&app.state.db, shelters::COLLECTION, &s2)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.event_id.as_deref(), Some(event.as_str()));

        for (i, shelter_id) in [&s1, &s1, &s2].into_iter().enumerate() {
            let mut p = person(Level::Assisted, Some(shelter_id));
            p.event_ids = vec![event.clone()];
            put(&app, &format!("p{i}"), &p).await;
        }
        let mut outsider = person(Level::Assisted, Some(&s1));
        outsider.event_ids = vec!["another-event".into()];
        put(&app, "outsider", &outsider).await;
        household(&app, "h1", &s1, "p0", &["p0", "p1"]).await;
        household(&app, "h2", &s2, "p2", &["p2"]).await;
        household(&app, "h-other", "some-other-shelter", "x", &["x"]).await;
        for item in ["soap", "blanket", "soap"] {
            delivery(&app, &s1, Some(&event), item, "p0").await;
        }
        delivery(&app, &s1, Some("another-event"), "soap", "p0").await;
        let now = store::now();
        let rescue = RescueDoc {
            person_id: "p0".into(),
            volunteer_id: "vol".into(),
            device_id: None,
            lat: ORIGIN.0,
            lon: ORIGIN.1,
            accuracy_m: 10.0,
            landing_point: None,
            captured_at: now,
            received_at: now,
            event_id: Some(event.clone()),
            destination_shelter_id: None,
            note: None,
        };
        store::replace(
            &app.state.db,
            crate::rescues::COLLECTION,
            &new_id(),
            &rescue,
        )
        .await
        .unwrap();

        let stats = event_stats(&app.state.db, &event).await.unwrap();
        assert_eq!(
            stats,
            EventStats {
                shelters: 2,
                unique_people: 3,
                households: 2,
                rescues: 1,
                deliveries: 3
            }
        );
        assert_eq!(
            event_stats(&app.state.db, "empty-event").await.unwrap(),
            EventStats::default()
        );
    }
}
