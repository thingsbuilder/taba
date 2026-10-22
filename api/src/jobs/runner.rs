//! The loop that ticks every hour and runs the steps, each with its own deadline.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::persist::{
    close_events, delete_expired, delete_thumbnails, expire_family_photos, notify_expiring_searches,
};
use crate::auth::AppState;
use crate::error::ApiError;

const INTERVAL: Duration = Duration::from_secs(3600);
/// What one step may take. The discard holds a 20 minute lease, so a step cut off here leaves
/// it to lapse by itself soon after.
const STEP_DEADLINE: Duration = Duration::from_secs(15 * 60);
/// How long `shutdown` waits for a pass in the middle of a step.
pub(super) const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// The loop of the hourly task, which `serve` stops before it exits.
pub struct Jobs {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

/// Starts the loop; `serve` calls it once.
pub fn spawn(state: AppState) -> Jobs {
    let (stop, mut stopped) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(INTERVAL);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = stopped.changed() => break,
            }
            // A pass cut off by the stop is safe: every write is a transaction.
            tokio::select! {
                _ = run_once(&state) => {}
                _ = stopped.changed() => {
                    tracing::info!("hourly task interrupted by shutdown");
                    break;
                }
            }
        }
    });
    Jobs { stop, task }
}

impl Jobs {
    /// Asks the loop to stop and waits for it, up to `SHUTDOWN_GRACE`.
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        match tokio::time::timeout(SHUTDOWN_GRACE, self.task).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::error!(%error, "hourly task ended abnormally"),
            Err(_) => tracing::warn!("hourly task did not stop in time"),
        }
    }
}

/// A step that did not finish well.
#[derive(Debug)]
pub struct StepFailure {
    pub step: &'static str,
    pub error: ApiError,
}

pub(super) type StepFuture<'a> = Pin<Box<dyn Future<Output = Result<(), ApiError>> + Send + 'a>>;

/// Runs the steps one after the other, each within `deadline`. A step that fails or runs out of
/// time does not stop the others; every failure is logged and all of them are returned.
pub(super) async fn run_steps(
    steps: Vec<(&'static str, StepFuture<'_>)>,
    deadline: Duration,
) -> Vec<StepFailure> {
    let mut failures = Vec::new();
    for (step, work) in steps {
        let started = tokio::time::Instant::now();
        let outcome = match tokio::time::timeout(deadline, work).await {
            Ok(result) => result,
            Err(_) => Err(ApiError::new(
                axum::http::StatusCode::GATEWAY_TIMEOUT,
                "job_step_timeout",
            )),
        };
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match outcome {
            Ok(()) => tracing::info!(step, elapsed_ms, "hourly step done"),
            Err(error) => {
                tracing::error!(step, code = error.code, elapsed_ms, "hourly step failed");
                failures.push(StepFailure { step, error });
            }
        }
    }
    failures
}

/// One pass of the task; tests call it directly. A step that fails does not stop the others;
/// every failure is logged and the first is returned.
pub async fn run_once(state: &AppState) -> Result<(), ApiError> {
    let steps: Vec<(&'static str, StepFuture<'_>)> = vec![
        ("close_events", Box::pin(close_events(state))),
        (
            "notify_expiring_searches",
            Box::pin(notify_expiring_searches(state)),
        ),
        (
            "expire_family_photos",
            Box::pin(expire_family_photos(state)),
        ),
        ("delete_thumbnails", Box::pin(delete_thumbnails(state))),
        ("delete_expired", Box::pin(delete_expired(state))),
        (
            "purge",
            Box::pin(async { crate::purge::run(state).await.map(|_| ()) }),
        ),
    ];
    let mut failures = run_steps(steps, STEP_DEADLINE).await;
    if failures.len() > 1 {
        let steps: Vec<_> = failures.iter().map(|f| f.step).collect();
        tracing::error!(?steps, "several steps of the hourly task failed");
    }
    match failures.is_empty() {
        true => Ok(()),
        false => Err(failures.swap_remove(0).error),
    }
}
