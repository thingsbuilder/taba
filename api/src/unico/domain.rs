//! The rules of the Unico client (spec 5.1 to 5.5) as plain functions: how an answer is read,
//! how a failure maps to an API error, the backoff and how a refused image is described.
//! Nothing here is `async` or calls Unico.

use std::time::Duration;

use axum::http::StatusCode;
use serde_json::Value;

use super::model::{ProcessResult, UnicoError};
use crate::error::ApiError;

pub(super) const NO_FACE_CODE: i64 = 20532;
const RETRY_AFTER_SECONDS: u32 = 30;

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
            UnicoError::PhotoRejected => (StatusCode::UNPROCESSABLE_ENTITY, "photo_rejected"),
            UnicoError::InvalidImage => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_image"),
            UnicoError::ImageTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "image_too_large"),
            UnicoError::Degraded => return ApiError::unico_degraded(),
        };
        let mut error = ApiError::new(status, code);
        if err == UnicoError::Busy {
            error.retry_after = Some(RETRY_AFTER_SECONDS);
        }
        error
    }
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

/// API-only flow: error `20532` is "no face"; the image errors are "invalid image" (spec 5.3).
pub(super) fn rejected_image(reply: &Value) -> UnicoError {
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
        // A 4xx about the image that is none of the above is still about the image: the same
        // image will be refused again, so it is a refusal, not a failure of Unico (a 502).
        _ => UnicoError::PhotoRejected,
    }
}

/// Longest part of Unico's answer written to the log.
const LOGGED_REPLY_CHARS: usize = 300;

/// What the image was, without the image: size, the first four bytes and, for a JPEG or a PNG, the
/// dimensions of its header. Enough to tell a black frame or a wrong format from a refusal of the face.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ImageInfo {
    pub(super) bytes: usize,
    pub(super) magic: String,
    pub(super) dimensions: Option<(u32, u32)>,
}

pub(super) fn image_info(image: &[u8]) -> ImageInfo {
    ImageInfo {
        bytes: image.len(),
        magic: image.iter().take(4).map(|b| format!("{b:02x}")).collect(),
        dimensions: png_size(image).or_else(|| jpeg_size(image)),
    }
}

fn png_size(image: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if image.len() < 24 || image[..8] != SIGNATURE {
        return None;
    }
    let read =
        |at: usize| u32::from_be_bytes([image[at], image[at + 1], image[at + 2], image[at + 3]]);
    Some((read(16), read(20)))
}

/// The size in the first start-of-frame marker (`SOF0`..`SOF15`, but not `DHT`, `JPG` or `DAC`).
fn jpeg_size(image: &[u8]) -> Option<(u32, u32)> {
    if image.len() < 4 || image[0] != 0xff || image[1] != 0xd8 {
        return None;
    }
    let mut at = 2;
    while at + 4 <= image.len() {
        if image[at] != 0xff {
            return None;
        }
        let marker = image[at + 1];
        if marker == 0xff {
            at += 1; // fill byte
            continue;
        }
        if (0xd0..=0xd9).contains(&marker) || marker == 0x01 {
            at += 2; // markers without a length
            continue;
        }
        let length = usize::from(u16::from_be_bytes([image[at + 2], image[at + 3]]));
        let is_frame = (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker);
        if is_frame {
            let frame = image.get(at + 4..at + 9)?;
            let height = u32::from(u16::from_be_bytes([frame[1], frame[2]]));
            let width = u32::from(u16::from_be_bytes([frame[3], frame[4]]));
            return Some((width, height));
        }
        at += 2 + length;
    }
    None
}

/// Writes down why Unico refused a photo: its answer (cut short) and the metadata of the image,
/// never the image nor its base64.
pub(super) fn log_rejected_image(reply: &Value, image: &[u8], mapped: &UnicoError) {
    let text = reply.to_string();
    let shown: String = text.chars().take(LOGGED_REPLY_CHARS).collect();
    let info = image_info(image);
    tracing::warn!(
        reply = %shown,
        image_bytes = info.bytes,
        image_magic = %info.magic,
        image_width = info.dimensions.map(|d| d.0),
        image_height = info.dimensions.map(|d| d.1),
        ?mapped,
        "unico refused the photo"
    );
}
