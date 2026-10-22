use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use super::client::Unico;
use super::domain::{
    ImageInfo, backoff_delay, classify, extract_token, image_info, log_rejected_image,
};
use super::*;
use crate::error::ApiError;
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
        // Observed in production: a 400 with no documented code for a bad or faceless image.
        (
            (400, json!({ "code": 3, "message": "INVALID_REQUEST" })),
            UnicoError::PhotoRejected,
        ),
        ((422, json!({})), UnicoError::PhotoRejected),
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
        (UnicoError::PhotoRejected, 422, "photo_rejected"),
        (UnicoError::Inconclusive, 422, "capture_inconclusive"),
        (UnicoError::Degraded, 503, "unico_degraded"),
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
        let _: ApiError = UnicoError::Rejected(json!({ "code": 3, "message": "BAD_THING" })).into();
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

/// A JPEG header: SOI, an APP0, then SOF0 for `width` x `height`.
fn jpeg(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
    bytes.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    bytes.extend_from_slice(&[0xff, 0xc0, 0x00, 0x0b, 0x08]);
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
    bytes
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = vec![
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R',
    ];
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes
}

#[test]
fn the_metadata_of_an_image_comes_from_its_header() {
    assert_eq!(
        image_info(&jpeg(1280, 960)),
        ImageInfo {
            bytes: jpeg(1280, 960).len(),
            magic: "ffd8ffe0".into(),
            dimensions: Some((1280, 960))
        }
    );
    assert_eq!(image_info(&png(640, 480)).dimensions, Some((640, 480)));
    assert_eq!(image_info(&png(640, 480)).magic, "89504e47");
    // Not an image, a short one, a truncated JPEG: no dimensions, and no panic.
    for junk in [
        &b"hello world"[..],
        b"",
        b"\xff\xd8",
        b"\xff\xd8\xff\xe0\x00",
    ] {
        assert_eq!(image_info(junk).dimensions, None, "{junk:?}");
    }
    assert_eq!(image_info(b"ab").magic, "6162");
    assert_eq!(image_info(b"").magic, "");
}

#[test]
fn a_refused_photo_is_logged_with_the_answer_and_the_metadata_and_never_the_image() {
    let buffer = LogBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buffer.clone())
        .with_ansi(false)
        .finish();
    let image = jpeg(1280, 960);
    let reply = json!({ "code": 3, "message": "INVALID_REQUEST", "padding": "x".repeat(1000) });
    tracing::subscriber::with_default(subscriber, || {
        log_rejected_image(&reply, &image, &UnicoError::PhotoRejected)
    });
    let log = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    assert!(
        log.contains("WARN") && log.contains("INVALID_REQUEST"),
        "{log}"
    );
    assert!(
        log.contains("ffd8ffe0") && log.contains(&format!("image_bytes={}", image.len())),
        "{log}"
    );
    assert!(
        log.contains("image_width=1280") && log.contains("image_height=960"),
        "{log}"
    );
    assert!(log.contains("PhotoRejected"), "{log}");
    // The answer is cut short, and the image (or its base64) is not there.
    assert!(!log.contains(&"x".repeat(400)), "reply not truncated");
    assert!(
        !log.contains(&STANDARD.encode(&image)),
        "base64 of the image in the log"
    );
    assert_eq!(log.lines().count(), 1);
}
