//! The rules of the family link: the code, the search id derived from it and the per-process
//! rate limit of the public routes.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// Submits per hour from one device (`X-Device-Id`) on one IP, and from one IP alone.
pub(super) const MAX_PER_DEVICE_HOUR: usize = 20;
pub(super) const MAX_PER_IP_HOUR: usize = 100;
pub(super) const LIMIT_WINDOW: Duration = Duration::from_secs(3600);
pub(super) fn new_code() -> String {
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
/// The search the submit makes: always the same for one link, so a retry writes the same document.
pub(super) fn search_id_of(code: &str) -> String {
    let digest = Sha256::digest(code.as_bytes());
    let bytes: [u8; 16] = digest[..16].try_into().expect("sha256 has 32 bytes");
    uuid::Uuid::from_bytes(bytes).to_string()
}
/// A sliding window per key, in this process's memory.
// ponytail: the counters live in memory, at most 10 000 keys (past it the idle ones are dropped),
// and reset on restart. With N instances the limit is N times higher; the per-link attempts in
// Firestore still cap the damage. Count in Firestore with a TTL document per key and hour before
// running more than one instance.
pub(super) fn allow(key: &str, max: usize) -> bool {
    static HITS: OnceLock<Mutex<HashMap<String, Vec<Instant>>>> = OnceLock::new();
    let mut hits = HITS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let now = Instant::now();
    if hits.len() > 10_000 {
        hits.retain(|_, at| at.iter().any(|t| now.duration_since(*t) < LIMIT_WINDOW));
    }
    let at = hits.entry(key.to_string()).or_default();
    at.retain(|t| now.duration_since(*t) < LIMIT_WINDOW);
    if at.len() >= max {
        return false;
    }
    at.push(now);
    true
}
