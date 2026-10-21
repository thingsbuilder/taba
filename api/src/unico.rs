//! Unico Human Token client (spec 5.1-5.6). The only module that talks to the Unico API
//! and the only one that reads `data.humanToken.value` (`extract_token`, spec 5.4).
//!
//! There is no sandbox and every call is billed: automated tests point `UNICO_URL` at a fake.

use std::time::Duration;

use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::config::Secret;
use crate::error::ApiError;

/// Simultaneous calls to Unico in this process (spec 5.5).
const MAX_CONCURRENT_CALLS: usize = 4;
/// Retries after a `429` that is not the monthly limit: waits of 1, 2, 4, 8 s plus 0-1 s of jitter.
const RATE_LIMIT_RETRIES: u32 = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// `PUT`/`POST` image limit of the API-only flow (spec 5.3).
const MAX_IMAGE_BYTES: usize = 800 * 1024;
const NO_FACE_CODE: i64 = 20532;
const RETRY_AFTER_SECONDS: u32 = 30;

#[derive(Debug, PartialEq, Eq)]
pub enum UnicoError {
    /// `429` saying the monthly credits are over. Never retried.
    Limit,
    /// `429` that persisted through every retry.
    Busy,
    /// `5xx`, timeout or connection failure after the retry.
    Unavailable,
    /// `401`/`403`, or no access key configured.
    Misconfigured,
    /// A process state or response shape we do not handle.
    UnexpectedState(String),
    /// Any other `4xx`; carries the response body for the caller to interpret.
    Rejected(Value),
    /// API-only flow: finished without a token, or failed.
    Inconclusive,
    NoFace,
    InvalidImage,
    ImageTooLarge,
}

impl From<UnicoError> for ApiError {
    fn from(err: UnicoError) -> Self {
        // Every way to a 502 unico_unexpected_state is logged here (spec 5.2.3); never a token.
        match &err {
            UnicoError::UnexpectedState(what) => {
                tracing::warn!(what, "unico returned an unexpected state")
            }
            UnicoError::Rejected(body) => {
                let message = body
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("none");
                tracing::warn!(
                    message,
                    "unico rejected the request with an unexpected error"
                );
            }
            _ => {}
        }
        let (status, code) = match err {
            UnicoError::Limit => (StatusCode::SERVICE_UNAVAILABLE, "unico_limit"),
            UnicoError::Busy => (StatusCode::SERVICE_UNAVAILABLE, "unico_busy"),
            UnicoError::Unavailable => (StatusCode::BAD_GATEWAY, "unico_unavailable"),
            UnicoError::Misconfigured => (StatusCode::INTERNAL_SERVER_ERROR, "unico_misconfigured"),
            UnicoError::UnexpectedState(_) | UnicoError::Rejected(_) => {
                (StatusCode::BAD_GATEWAY, "unico_unexpected_state")
            }
            UnicoError::Inconclusive => (StatusCode::UNPROCESSABLE_ENTITY, "capture_inconclusive"),
            UnicoError::NoFace => (StatusCode::UNPROCESSABLE_ENTITY, "no_face"),
            UnicoError::InvalidImage => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_image"),
            UnicoError::ImageTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "image_too_large"),
        };
        let mut error = ApiError::new(status, code);
        if err == UnicoError::Busy {
            error.retry_after = Some(RETRY_AFTER_SECONDS);
        }
        error
    }
}

/// A journey created for a capture (spec 5.2, step 1).
#[derive(Debug, PartialEq, Eq)]
pub struct Journey {
    pub process_id: String,
    pub link: String,
}

/// Where a journey stands (spec 5.2, step 3).
#[derive(Debug, PartialEq, Eq)]
pub enum ProcessResult {
    Pending,
    Token(String),
    Inconclusive,
    Failed,
    Canceled,
}

/// The one place that reads `data.humanToken.value`. Missing, empty and whitespace-only all
/// mean "no token" (spec 5.4).
pub fn extract_token(process: &Value) -> Option<String> {
    let value = process.pointer("/data/humanToken/value")?.as_str()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

pub fn classify(process: &Value) -> Result<ProcessResult, UnicoError> {
    match process.get("state").and_then(Value::as_str) {
        Some("PROCESS_STATE_CREATED") => Ok(ProcessResult::Pending),
        Some("PROCESS_STATE_FINISHED") => {
            Ok(extract_token(process).map_or(ProcessResult::Inconclusive, ProcessResult::Token))
        }
        Some("PROCESS_STATE_FAILED") => Ok(ProcessResult::Failed),
        Some("PROCESS_STATE_CANCELED") => Ok(ProcessResult::Canceled),
        other => Err(UnicoError::UnexpectedState(
            other.unwrap_or("missing").to_string(),
        )),
    }
}

/// Wait before retry number `retry` (0-based): `unit * 2^retry + unit * jitter`, jitter in [0, 1).
pub fn backoff_delay(retry: u32, jitter: f64, unit: Duration) -> Duration {
    unit * 2u32.pow(retry) + unit.mul_f64(jitter)
}

pub struct Unico {
    http: reqwest::Client,
    base_url: String,
    access_key: Option<Secret>,
    permits: Semaphore,
    /// One second in production (backoff steps; the `5xx` retry waits two). Tests shrink it.
    unit: Duration,
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
        }
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
    #[allow(dead_code)] // used from phase 3 (check-in, rescue, search)
    pub async fn token_from_image(&self, image: &[u8]) -> Result<String, UnicoError> {
        if image.len() > MAX_IMAGE_BYTES {
            return Err(UnicoError::ImageTooLarge);
        }
        let body = json!({ "flow": "api+humantoken", "imageBase64": STANDARD.encode(image) });
        let process = match self
            .call(reqwest::Method::POST, "/process", Some(&body))
            .await
        {
            Err(UnicoError::Rejected(reply)) => return Err(rejected_image(&reply)),
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
    /// One logical call: semaphore, access key, and the retry rules of spec 5.5.
    async fn call(
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

/// API-only flow: error `20532` is "no face"; the image errors are "invalid image" (spec 5.3).
#[allow(dead_code)] // used with `token_from_image`
fn rejected_image(reply: &Value) -> UnicoError {
    let message = reply
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if reply.get("code").and_then(Value::as_i64) == Some(NO_FACE_CODE) || message.contains("20532")
    {
        return UnicoError::NoFace;
    }
    match message {
        "INVALID_IMAGE" | "UNSUPPORTED_IMAGE_FORMAT" | "MISSING_IMAGE" => UnicoError::InvalidImage,
        "IMAGE_TOO_LARGE" => UnicoError::ImageTooLarge,
        _ => UnicoError::UnexpectedState(format!("rejected: {message}")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::test_support::{FakeUnico, TEST_ACCESS_KEY, fast_unico};

    fn client(fake: &FakeUnico) -> Unico {
        fast_unico(&fake.url, Some(TEST_ACCESS_KEY))
    }

    fn finished(value: Value) -> Value {
        json!({ "state": "PROCESS_STATE_FINISHED", "data": { "humanToken": { "value": value } } })
    }

    #[test]
    fn extract_token_returns_none_without_a_usable_value() {
        // spec 15, criterion 1
        assert_eq!(extract_token(&json!({ "data": {} })), None);
        assert_eq!(extract_token(&json!({})), None);
        assert_eq!(
            extract_token(&json!({ "data": { "humanToken": {} } })),
            None
        );
        assert_eq!(extract_token(&finished(json!(""))), None);
        assert_eq!(extract_token(&finished(json!("  "))), None);
        assert_eq!(extract_token(&finished(json!(null))), None);
        assert_eq!(extract_token(&finished(json!(42))), None);
        assert_eq!(
            extract_token(&finished(json!("tok/en+1"))),
            Some("tok/en+1".into())
        );
    }

    #[test]
    fn every_process_state_maps_to_a_result() {
        assert_eq!(
            classify(&json!({ "state": "PROCESS_STATE_CREATED" })),
            Ok(ProcessResult::Pending)
        );
        assert_eq!(
            classify(&finished(json!("t"))),
            Ok(ProcessResult::Token("t".into()))
        );
        assert_eq!(
            classify(&finished(json!(""))),
            Ok(ProcessResult::Inconclusive)
        );
        assert_eq!(
            classify(&json!({ "state": "PROCESS_STATE_FINISHED" })),
            Ok(ProcessResult::Inconclusive)
        );
        assert_eq!(
            classify(&json!({ "state": "PROCESS_STATE_FAILED" })),
            Ok(ProcessResult::Failed)
        );
        assert_eq!(
            classify(&json!({ "state": "PROCESS_STATE_CANCELED" })),
            Ok(ProcessResult::Canceled)
        );
        for other in [
            "PROCESS_STATE_UNSPECIFIED",
            "PROCESS_STATE_AWAITING_FOR_DOCUMENT",
        ] {
            assert_eq!(
                classify(&json!({ "state": other })),
                Err(UnicoError::UnexpectedState(other.into()))
            );
        }
        assert!(matches!(
            classify(&json!({})),
            Err(UnicoError::UnexpectedState(_))
        ));
    }

    #[test]
    fn backoff_doubles_from_one_second_and_adds_up_to_one_second_of_jitter() {
        let second = Duration::from_secs(1);
        for (retry, base) in [(0, 1), (1, 2), (2, 4), (3, 8)] {
            assert_eq!(backoff_delay(retry, 0.0, second), Duration::from_secs(base));
            assert_eq!(
                backoff_delay(retry, 0.5, second),
                Duration::from_secs(base) + Duration::from_millis(500)
            );
            assert!(backoff_delay(retry, 0.999, second) < Duration::from_secs(base + 1));
        }
    }

    #[tokio::test]
    async fn create_journey_sends_the_flow_the_callback_and_the_key() {
        let fake = FakeUnico::start().await;
        let journey = client(&fake)
            .create_journey("https://taba.example.org/return?capture=c1")
            .await
            .unwrap();
        assert_eq!(
            journey,
            Journey {
                process_id: "proc-1".into(),
                link: "https://journey.test/proc-1?collect-data=true".into()
            }
        );
        assert_eq!(
            fake.with(|s| s.posts.clone()),
            vec![
                json!({ "flow": "humantoken", "callbackUri": "https://taba.example.org/return?capture=c1" })
            ]
        );
        assert_eq!(
            fake.with(|s| s.access_keys.clone()),
            vec![TEST_ACCESS_KEY.to_string()]
        );
    }

    #[tokio::test]
    async fn journey_without_link_is_an_unexpected_state() {
        let fake = FakeUnico::start().await;
        fake.with(|s| s.post_replies.push_back((200, json!({ "id": "p" }))));
        let err = client(&fake)
            .create_journey("https://x/return")
            .await
            .unwrap_err();
        assert!(matches!(err, UnicoError::UnexpectedState(_)));
    }

    #[tokio::test]
    async fn get_process_reads_the_state_from_the_server() {
        let fake = FakeUnico::start().await;
        fake.finish_with_token("p1", "tok");
        fake.set_state("p2", "PROCESS_STATE_CANCELED");
        let unico = client(&fake);
        assert_eq!(
            unico.get_process("p1").await,
            Ok(ProcessResult::Token("tok".into()))
        );
        assert_eq!(unico.get_process("p2").await, Ok(ProcessResult::Canceled));
        assert_eq!(unico.get_process("p3").await, Ok(ProcessResult::Pending));
        assert_eq!(fake.post_count(), 0, "polling never creates a process");
    }

    #[tokio::test]
    async fn rate_limit_is_retried_four_times_then_becomes_busy() {
        let fake = FakeUnico::start().await;
        fake.with(|s| {
            s.post_replies
                .extend((0..10).map(|_| (429, json!({ "message": "too many requests" }))))
        });
        let err = client(&fake)
            .create_journey("https://x/return")
            .await
            .unwrap_err();
        assert_eq!(err, UnicoError::Busy);
        assert_eq!(fake.post_count(), 5, "the first call plus four retries");
        let api: ApiError = err.into();
        assert_eq!(
            (api.status.as_u16(), api.code, api.retry_after),
            (503, "unico_busy", Some(30))
        );
    }

    #[tokio::test]
    async fn rate_limit_that_clears_succeeds() {
        let fake = FakeUnico::start().await;
        fake.with(|s| {
            s.post_replies
                .extend((0..2).map(|_| (429, json!({ "message": "slow down" }))))
        });
        let journey = client(&fake)
            .create_journey("https://x/return")
            .await
            .unwrap();
        assert_eq!(journey.process_id, "proc-3");
        assert_eq!(fake.post_count(), 3);
    }

    #[tokio::test]
    async fn monthly_limit_is_not_retried() {
        let fake = FakeUnico::start().await;
        fake.with(|s| {
            s.post_replies.push_back((
                429,
                json!({ "message": "Your monthly process limit has been reached" }),
            ))
        });
        let err = client(&fake)
            .create_journey("https://x/return")
            .await
            .unwrap_err();
        assert_eq!(err, UnicoError::Limit);
        assert_eq!(fake.post_count(), 1);
        let api: ApiError = err.into();
        assert_eq!((api.status.as_u16(), api.code), (503, "unico_limit"));
    }

    #[tokio::test]
    async fn server_error_gets_one_retry_then_unavailable() {
        let fake = FakeUnico::start().await;
        fake.with(|s| s.post_replies.extend((0..5).map(|_| (500, json!({})))));
        let err = client(&fake)
            .create_journey("https://x/return")
            .await
            .unwrap_err();
        assert_eq!(err, UnicoError::Unavailable);
        assert_eq!(fake.post_count(), 2);
        let api: ApiError = err.into();
        assert_eq!((api.status.as_u16(), api.code), (502, "unico_unavailable"));
    }

    #[tokio::test]
    async fn server_error_that_clears_after_the_retry_succeeds() {
        let fake = FakeUnico::start().await;
        fake.with(|s| s.post_replies.push_back((503, json!({}))));
        assert!(
            client(&fake)
                .create_journey("https://x/return")
                .await
                .is_ok()
        );
        assert_eq!(fake.post_count(), 2);
    }

    #[tokio::test]
    async fn unreachable_server_counts_as_unavailable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let err = fast_unico(&url, Some(TEST_ACCESS_KEY))
            .create_journey("https://x/return")
            .await
            .unwrap_err();
        assert_eq!(err, UnicoError::Unavailable);
    }

    #[tokio::test]
    async fn rejected_key_is_a_configuration_error_without_retry() {
        for status in [401, 403] {
            let fake = FakeUnico::start().await;
            fake.with(|s| {
                s.post_replies
                    .push_back((status, json!({ "message": "PERMISSION_DENIED" })))
            });
            let err = client(&fake)
                .create_journey("https://x/return")
                .await
                .unwrap_err();
            assert_eq!(err, UnicoError::Misconfigured);
            assert_eq!(fake.post_count(), 1);
            let api: ApiError = err.into();
            assert_eq!(
                (api.status.as_u16(), api.code),
                (500, "unico_misconfigured")
            );
        }
    }

    #[tokio::test]
    async fn missing_key_makes_no_call() {
        let fake = FakeUnico::start().await;
        let err = fast_unico(&fake.url, None)
            .create_journey("https://x/return")
            .await
            .unwrap_err();
        assert_eq!(err, UnicoError::Misconfigured);
        assert_eq!(fake.post_count(), 0);
    }

    #[tokio::test]
    async fn other_client_errors_are_surfaced_as_unexpected() {
        let fake = FakeUnico::start().await;
        fake.with(|s| {
            s.post_replies
                .push_back((400, json!({ "code": 3, "message": "INVALID" })))
        });
        let err = client(&fake)
            .create_journey("https://x/return")
            .await
            .unwrap_err();
        assert!(matches!(err, UnicoError::Rejected(_)));
        let api: ApiError = err.into();
        assert_eq!(
            (api.status.as_u16(), api.code),
            (502, "unico_unexpected_state")
        );
    }

    #[tokio::test]
    async fn at_most_four_calls_run_at_the_same_time() {
        let fake = FakeUnico::start().await;
        fake.with(|s| s.delay = Duration::from_millis(60));
        let unico = Arc::new(client(&fake));
        let calls: Vec<_> = (0..12)
            .map(|i| {
                let unico = unico.clone();
                tokio::spawn(async move { unico.get_process(&format!("p{i}")).await })
            })
            .collect();
        for call in calls {
            assert_eq!(call.await.unwrap(), Ok(ProcessResult::Pending));
        }
        assert_eq!(fake.with(|s| s.max_in_flight), 4);
        assert_eq!(fake.get_count(), 12);
    }

    #[tokio::test]
    async fn image_over_800_kb_is_refused_without_calling_unico() {
        let fake = FakeUnico::start().await;
        let err = client(&fake)
            .token_from_image(&vec![0u8; 800 * 1024 + 1])
            .await
            .unwrap_err();
        assert_eq!(err, UnicoError::ImageTooLarge);
        assert_eq!(fake.post_count(), 0);
        let api: ApiError = err.into();
        assert_eq!((api.status.as_u16(), api.code), (413, "image_too_large"));
    }

    #[tokio::test]
    async fn api_only_flow_sends_the_image_and_returns_the_token() {
        let fake = FakeUnico::start().await;
        fake.with(|s| s.post_replies.push_back((200, finished(json!("tok-api")))));
        let token = client(&fake)
            .token_from_image(&vec![1u8; 800 * 1024])
            .await
            .unwrap();
        assert_eq!(token, "tok-api");
        let sent = fake.with(|s| s.posts[0].clone());
        assert_eq!(sent["flow"], "api+humantoken");
        assert_eq!(
            STANDARD
                .decode(sent["imageBase64"].as_str().unwrap())
                .unwrap()
                .len(),
            800 * 1024
        );
    }

    #[tokio::test]
    async fn api_only_flow_maps_each_failure() {
        let cases = [
            (
                (
                    400,
                    json!({ "code": 20532, "message": "No face detected in image" }),
                ),
                UnicoError::NoFace,
            ),
            (
                (400, json!({ "code": 3, "message": "INVALID_IMAGE" })),
                UnicoError::InvalidImage,
            ),
            (
                (
                    400,
                    json!({ "code": 3, "message": "UNSUPPORTED_IMAGE_FORMAT" }),
                ),
                UnicoError::InvalidImage,
            ),
            (
                (400, json!({ "code": 3, "message": "MISSING_IMAGE" })),
                UnicoError::InvalidImage,
            ),
            (
                (400, json!({ "code": 3, "message": "IMAGE_TOO_LARGE" })),
                UnicoError::ImageTooLarge,
            ),
            ((200, finished(json!(""))), UnicoError::Inconclusive),
            (
                (200, json!({ "state": "PROCESS_STATE_FAILED" })),
                UnicoError::Inconclusive,
            ),
        ];
        for (reply, expected) in cases {
            let fake = FakeUnico::start().await;
            fake.with(|s| s.post_replies.push_back(reply.clone()));
            let err = client(&fake).token_from_image(b"img").await.unwrap_err();
            assert_eq!(err, expected, "{reply:?}");
        }
    }

    #[test]
    fn every_error_has_its_status_and_code() {
        let table = [
            (UnicoError::NoFace, 422, "no_face"),
            (UnicoError::InvalidImage, 422, "invalid_image"),
            (UnicoError::Inconclusive, 422, "capture_inconclusive"),
            (
                UnicoError::UnexpectedState("x".into()),
                502,
                "unico_unexpected_state",
            ),
        ];
        for (err, status, code) in table {
            let api: ApiError = err.into();
            assert_eq!((api.status.as_u16(), api.code), (status, code));
        }
    }

    #[derive(Clone, Default)]
    struct LogBuffer(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for LogBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
        type Writer = LogBuffer;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn unexpected_states_are_logged_with_the_state_and_no_token() {
        let buffer = LogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let _: ApiError = classify(&json!({ "state": "PROCESS_STATE_AWAITING_FOR_DOCUMENT", "data": { "humanToken": { "value": "secret-token" } } }))
                .unwrap_err()
                .into();
            let _: ApiError =
                UnicoError::Rejected(json!({ "code": 3, "message": "BAD_THING" })).into();
            let _: ApiError = UnicoError::Busy.into();
        });
        let log = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        assert!(
            log.contains("WARN") && log.contains("PROCESS_STATE_AWAITING_FOR_DOCUMENT"),
            "{log}"
        );
        assert!(log.contains("BAD_THING"), "{log}");
        assert!(!log.contains("secret-token"));
        assert_eq!(
            log.lines().count(),
            2,
            "only the unexpected cases are logged: {log}"
        );
    }
}
