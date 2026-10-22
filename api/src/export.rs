//! The export of a shelter (spec 8.8, Q22): `GET /v1/shelters/{id}/export.csv`, for an admin of a
//! `confirmed` shelter: its people, households and deliveries, no thumbnail, no `person_id`, no
//! token and no card code, audited. Also the `stats` an event keeps when it is discarded.
//!
//! One CSV with three sections. Each section opens with a `# name` line and a column header, and
//! ends with a blank line. Households are labelled `N1`, `N2`, ... for this export only, so a row
//! of `people` can point at its household without an id. `# warning` lines say a section was cut.
//!
//! Layers, one file each: `model` (the row the count reads), `persist` (the reads that feed the
//! CSV and `event_stats`), `http` (the route) and `view` (the CSV text). There is no `domain`: the
//! only rules are how a cell is written, and they live with the text they shape in `view`.

mod http;
mod model;
mod persist;
mod view;

#[cfg(test)]
mod tests;

pub use http::export_csv;
pub use persist::event_stats;
