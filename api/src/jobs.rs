//! The hourly task of `serve` (spec 6.2.6 items 1, 2, 4): closes events, raises `search_expiring`,
//! deletes thumbnails, expires captures, invites and endorsement codes. Stage 0: it runs every
//! hour and does nothing.
#![allow(dead_code)] // stage 0 scaffold: the owner of this module removes this line

use std::time::Duration;

use crate::auth::AppState;
use crate::error::ApiError;

const INTERVAL: Duration = Duration::from_secs(3600);

/// Starts the loop; `serve` calls it once.
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(INTERVAL);
        loop {
            tick.tick().await;
            if let Err(err) = run_once(&state).await {
                tracing::error!(code = err.code, "hourly task failed");
            }
        }
    });
}

/// One pass of the task; tests call it directly.
pub async fn run_once(_state: &AppState) -> Result<(), ApiError> {
    Ok(())
}
