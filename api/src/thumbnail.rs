//! Thumbnail of a person with no name or wristband (spec 4.6): AES-256-GCM with `THUMBNAIL_KEY`
//! and `GET /v1/people/{id}/thumbnail`.

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::audit::{self, AuditEntry};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::people::{self, PersonDoc};
use crate::store;

const NONCE_BYTES: usize = 12;
/// The app sends 128x128 JPEG at quality 0.6 (5-10 KB); this leaves room without accepting photos.
const MAX_THUMBNAIL_BYTES: usize = 32 * 1024;

fn invalid_thumbnail() -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_thumbnail")
}

/// The JPEG the app sent as `thumbnail_base64`.
pub fn decode_upload(base64: &str) -> Result<Vec<u8>, ApiError> {
    let bytes = STANDARD
        .decode(base64.trim())
        .map_err(|_| invalid_thumbnail())?;
    if bytes.is_empty() || bytes.len() > MAX_THUMBNAIL_BYTES || !bytes.starts_with(&[0xFF, 0xD8]) {
        return Err(invalid_thumbnail());
    }
    Ok(bytes)
}

fn cipher(key: &[u8]) -> Result<Aes256Gcm, ApiError> {
    Aes256Gcm::new_from_slice(key).map_err(|_| ApiError::internal())
}

/// `nonce (12 random bytes) || ciphertext+tag`; a new nonce for every call.
pub fn encrypt(key: &[u8], plain: &[u8]) -> Result<Vec<u8>, ApiError> {
    let mut nonce = [0u8; NONCE_BYTES];
    rand::fill(&mut nonce);
    let sealed = cipher(key)?
        .encrypt(&Nonce::from(nonce), plain)
        .map_err(|_| ApiError::internal())?;
    let mut out = nonce.to_vec();
    out.extend(sealed);
    Ok(out)
}

pub fn decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>, ApiError> {
    let (nonce, sealed) = data
        .split_at_checked(NONCE_BYTES)
        .ok_or_else(ApiError::internal)?;
    let nonce: [u8; NONCE_BYTES] = nonce.try_into().map_err(|_| ApiError::internal())?;
    cipher(key)?
        .decrypt(&Nonce::from(nonce), sealed)
        .map_err(|_| ApiError::internal())
}

/// `Option<Vec<u8>>` as a Firestore bytes value; the default `Vec<u8>` encoding is an array
/// of integers.
pub mod optional_bytes {
    use serde::de::{self, Visitor};
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(bytes) => s.serialize_bytes(bytes),
            None => s.serialize_none(),
        }
    }

    struct Bytes;

    impl<'de> Visitor<'de> for Bytes {
        type Value = Option<Vec<u8>>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a bytes value")
        }

        fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
            Ok(Some(v.to_vec()))
        }

        fn visit_byte_buf<E: de::Error>(self, v: Vec<u8>) -> Result<Self::Value, E> {
            Ok(Some(v))
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            d.deserialize_bytes(Bytes)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        d.deserialize_option(Bytes)
    }
}

/// `GET /v1/people/{id}/thumbnail` (spec 4.6): active members of the person's current shelter;
/// every access is audited. The people who see it in a match or a rescue come with phases 4 and 5.
pub async fn get_thumbnail(
    State(state): State<AppState>,
    volunteer: Volunteer,
    Path(person_id): Path<String>,
) -> Result<Response, ApiError> {
    let person: PersonDoc = store::get(&state.db, people::COLLECTION, &person_id)
        .await?
        .ok_or_else(people::person_not_found)?;
    let shelter_id = match &person.current_shelter_id {
        Some(shelter) if volunteer.role_in(shelter).is_some() => shelter.clone(),
        _ => return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden")),
    };
    let encrypted = person
        .thumbnail_encrypted
        .as_deref()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "thumbnail_not_found"))?;
    let jpeg = decrypt(state.thumbnail_key(), encrypted)?;
    let entry = AuditEntry::new("thumbnail.view", "person", &person_id)
        .by(&volunteer.id)
        .in_shelter(&shelter_id)
        .basis(person.legal_basis);
    store::transact(&state.db, |db, tx| {
        let entry = entry.clone();
        Box::pin(async move { audit::add(&db, tx, &entry) })
    })
    .await?;
    Ok((
        [(CONTENT_TYPE, "image/jpeg"), (CACHE_CONTROL, "no-store")],
        jpeg,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [9u8; 32];

    #[test]
    fn decrypts_what_it_encrypted() {
        let sealed = encrypt(&KEY, b"\xFF\xD8jpeg").unwrap();
        assert_eq!(decrypt(&KEY, &sealed).unwrap(), b"\xFF\xD8jpeg");
        assert!(!sealed.windows(4).any(|w| w == b"jpeg"));
    }

    #[test]
    fn every_encryption_uses_a_new_nonce() {
        let a = encrypt(&KEY, b"same").unwrap();
        let b = encrypt(&KEY, b"same").unwrap();
        assert_ne!(a[..NONCE_BYTES], b[..NONCE_BYTES]);
        assert_ne!(a, b);
        assert_eq!(a.len(), NONCE_BYTES + 4 + 16);
    }

    #[test]
    fn wrong_key_and_tampering_do_not_decrypt() {
        let mut sealed = encrypt(&KEY, b"data").unwrap();
        assert!(decrypt(&[1u8; 32], &sealed).is_err());
        *sealed.last_mut().unwrap() ^= 1;
        assert!(decrypt(&KEY, &sealed).is_err());
        assert!(decrypt(&KEY, &[0u8; 5]).is_err());
        assert!(encrypt(&[0u8; 5], b"x").is_err());
    }

    #[test]
    fn upload_must_be_a_small_jpeg_in_base64() {
        let jpeg = STANDARD.encode([0xFF, 0xD8, 0xFF, 0xE0, 1, 2]);
        assert_eq!(decode_upload(&jpeg).unwrap().len(), 6);
        for bad in [
            "not base64!".to_string(),
            String::new(),
            STANDARD.encode(b"png-or-text"),
            STANDARD.encode(vec![0xFF; MAX_THUMBNAIL_BYTES + 1]),
        ] {
            assert_eq!(decode_upload(&bad).unwrap_err().code, "invalid_thumbnail");
        }
    }
}
