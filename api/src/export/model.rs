//! The row the `events.stats` counts read.

use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct Count {
    pub(super) n: i64,
}
