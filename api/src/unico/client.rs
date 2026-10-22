//! The client: the only code that sends a request to the Unico API. It holds the HTTP client,
//! the concurrency permits and the failure count, and obeys and feeds the degraded mode.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use firestore::FirestoreDb;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use super::domain::{backoff_delay, classify, log_rejected_image, rejected_image};
use super::model::{Journey, ProcessResult, UnicoError};
use crate::config::Secret;
use crate::degraded;

/// Simultaneous calls to Unico in this process (spec 5.5).
const MAX_CONCURRENT_CALLS: usize = 4;
/// Retries after a `429` that is not the monthly limit: waits of 1, 2, 4, 8 s plus 0-1 s of jitter.
const RATE_LIMIT_RETRIES: u32 = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// `PUT`/`POST` image limit of the API-only flow (spec 5.3).
const MAX_IMAGE_BYTES: usize = 800 * 1024;
/// Failed calls in a row (`UnicoError::Unavailable`) that turn the degraded mode on (spec 5.5).
const FAILURES_BEFORE_DEGRADED: u32 = 3;

pub struct Unico {
    http: reqwest::Client,
    base_url: String,
    access_key: Option<Secret>,
    permits: Semaphore,
    /// One second in production (backoff steps; the `5xx` retry waits two). Tests shrink it.
    unit: Duration,
    /// Where `system/status` lives. Without it the client neither obeys nor changes the
    /// degraded mode (unit tests of the retry rules).
    status_db: Option<FirestoreDb>,
    /// `Unavailable` answers since the last one that was not.
    // ponytail: in-process counter, reset by a restart. With N instances each counts alone, so
    // the degraded mode (shared in `system/status`) is entered after up to N times the
    // threshold of failures; keep the count in `system/status` before running more than one.
    failures: AtomicU32,
}

impl Unico {
    pub fn new(base_url: &str, access_key: Option<Secret>) -> Self {
        Self::with_unit(base_url, access_key, Duration::from_secs(1))
    }

    pub fn with_unit(base_url: &str, access_key: Option<Secret>, unit: Duration) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("build the HTTP client"),
            base_url: base_url.trim_end_matches('/').to_string(),
            access_key,
            permits: Semaphore::new(MAX_CONCURRENT_CALLS),
            unit,
            status_db: None,
            failures: AtomicU32::new(0),
        }
    }

    /// Makes every call obey and update the degraded mode kept in `db` (spec 5.5, 5.7).
    pub fn with_status(mut self, db: FirestoreDb) -> Self {
        self.status_db = Some(db);
        self
    }

    /// `humantoken` flow: creates the process and returns the link the person opens.
    pub async fn create_journey(&self, callback_uri: &str) -> Result<Journey, UnicoError> {
        let body = json!({ "flow": "humantoken", "callbackUri": callback_uri });
        let process = self
            .call(reqwest::Method::POST, "/process", Some(&body))
            .await?;
        let text = |field| {
            process
                .get(field)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        match (text("id"), text("link")) {
            (Some(process_id), Some(link)) => Ok(Journey { process_id, link }),
            _ => Err(UnicoError::UnexpectedState(
                "journey without id or link".into(),
            )),
        }
    }

    /// Polling does not create a process and costs nothing (spec 5.2, step 2).
    pub async fn get_process(&self, process_id: &str) -> Result<ProcessResult, UnicoError> {
        let process = self
            .call(
                reqwest::Method::GET,
                &format!("/process/{process_id}"),
                None,
            )
            .await?;
        classify(&process)
    }

    /// `api+humantoken` flow (spec 5.3): a photo in, a token out. The image is refused locally
    /// when it is over 800 KB, so no call is made.
    pub async fn token_from_image(&self, image: &[u8]) -> Result<String, UnicoError> {
        if image.len() > MAX_IMAGE_BYTES {
            return Err(UnicoError::ImageTooLarge);
        }
        let body = json!({ "flow": "api+humantoken", "imageBase64": STANDARD.encode(image) });
        let process = match self
            .call(reqwest::Method::POST, "/process", Some(&body))
            .await
        {
            Err(UnicoError::Rejected(reply)) => {
                let err = rejected_image(&reply);
                log_rejected_image(&reply, image, &err);
                return Err(err);
            }
            other => other?,
        };
        match classify(&process)? {
            ProcessResult::Token(token) => Ok(token),
            ProcessResult::Inconclusive | ProcessResult::Failed => Err(UnicoError::Inconclusive),
            ProcessResult::Pending | ProcessResult::Canceled => Err(UnicoError::UnexpectedState(
                "api+humantoken did not finish".into(),
            )),
        }
    }
}

impl Unico {
    /// One logical call. In degraded mode nothing is sent (spec 5.7); the outcome feeds the
    /// degraded mode (spec 5.5): the monthly limit at once, three `Unavailable` in a row.
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, UnicoError> {
        self.ensure_available().await?;
        let result = self.send(method, path, body).await;
        self.record(&result).await;
        result
    }

    async fn ensure_available(&self) -> Result<(), UnicoError> {
        let Some(db) = &self.status_db else {
            return Ok(());
        };
        match degraded::blocking_reason(db).await {
            Ok(None) => Ok(()),
            Ok(Some(_)) => Err(UnicoError::Degraded),
            Err(err) => {
                // Firestore down takes the whole API down anyway; do not also block on it.
                tracing::error!(
                    code = err.code,
                    "could not read system/status; calling Unico"
                );
                Ok(())
            }
        }
    }

    async fn record(&self, result: &Result<Value, UnicoError>) {
        let Some(db) = &self.status_db else {
            return;
        };
        let recorded = match result {
            Err(UnicoError::Limit) => degraded::enter_limit(db).await,
            Err(UnicoError::Unavailable) => {
                let failures = self.failures.fetch_add(1, Ordering::Relaxed) + 1;
                // The count stays up after the mode trips, so one more failure once the five
                // minutes pass trips it again: the next real call is the test (spec 5.7).
                if failures >= FAILURES_BEFORE_DEGRADED {
                    degraded::enter_unavailable(db).await
                } else {
                    Ok(())
                }
            }
            _ => {
                self.failures.store(0, Ordering::Relaxed);
                Ok(())
            }
        };
        if let Err(err) = recorded {
            tracing::error!(code = err.code, "could not record the degraded mode");
        }
    }

    /// The semaphore, the access key and the retry rules of spec 5.5.
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, UnicoError> {
        let key = self.access_key.as_ref().ok_or(UnicoError::Misconfigured)?;
        let url = format!("{}{path}", self.base_url);
        let mut rate_limit_retries = 0;
        let mut retried_server_error = false;
        loop {
            let reply = {
                let _permit = self
                    .permits
                    .acquire()
                    .await
                    .expect("semaphore is never closed");
                let mut request = self
                    .http
                    .request(method.clone(), &url)
                    .header("x-unico-accesskey", &key.0);
                if let Some(body) = body {
                    request = request.json(body);
                }
                request.send().await
            };
            let (status, text) = match reply {
                Ok(response) => (response.status(), response.text().await.unwrap_or_default()),
                Err(err) => {
                    tracing::warn!(%err, "unico request failed");
                    (StatusCode::BAD_GATEWAY, String::new())
                }
            };
            let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);

            if status.is_success() {
                return Ok(json);
            }
            match status {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    tracing::error!(%status, "unico rejected the access key");
                    return Err(UnicoError::Misconfigured);
                }
                StatusCode::TOO_MANY_REQUESTS => {
                    if text.to_lowercase().contains("monthly process limit") {
                        tracing::error!("unico monthly process limit reached");
                        return Err(UnicoError::Limit);
                    }
                    if rate_limit_retries == RATE_LIMIT_RETRIES {
                        tracing::warn!("unico still answering 429 after every retry");
                        return Err(UnicoError::Busy);
                    }
                    let jitter = rand::random::<f64>();
                    tokio::time::sleep(backoff_delay(rate_limit_retries, jitter, self.unit)).await;
                    rate_limit_retries += 1;
                }
                s if s.is_server_error() => {
                    if retried_server_error {
                        tracing::warn!(%status, "unico unavailable after the retry");
                        return Err(UnicoError::Unavailable);
                    }
                    retried_server_error = true;
                    tokio::time::sleep(self.unit * 2).await;
                }
                _ => {
                    tracing::warn!(%status, "unico rejected the request");
                    return Err(UnicoError::Rejected(json));
                }
            }
        }
    }
}
