//! Boot smoke test: runs the real binary the way the VM does (no Firestore emulator, a service
//! account for credentials) and checks it gets past the TLS setup without panicking.
//!
//! Incident: v0.2.0 panicked at start-up in production because rustls had two crypto providers
//! (ring and aws-lc-rs) in the dependency tree and none installed. The unit tests never opened a
//! TLS connection, so they could not see it.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const WATCH: Duration = Duration::from_secs(5);

#[test]
fn serve_starts_without_a_rustls_panic() {
    let dir = std::env::temp_dir().join(format!("taba-boot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // The credentials file is only parsed at start-up; no key is needed for the TLS setup.
    let credentials = dir.join("service-account.json");
    std::fs::write(
        &credentials,
        r#"{"type":"service_account","project_id":"p","private_key_id":"x","private_key":"not a key","client_email":"a@b.c","client_id":"1","token_uri":"https://oauth2.googleapis.com/token"}"#,
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_taba"))
        .arg("serve")
        .env_remove("FIRESTORE_EMULATOR_HOST")
        .env("GOOGLE_APPLICATION_CREDENTIALS", &credentials)
        .env("GCP_PROJECT", "boot-test")
        .env("DOMAIN", "taba.example.org")
        .env("HMAC_KEY", format!("{}=", "A".repeat(43)))
        .env("UNICO_URL", "http://127.0.0.1:1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Either it keeps running (listening) or it exits with an ordinary error; never a panic.
    let start = Instant::now();
    while start.elapsed() < WATCH && child.try_wait().unwrap().is_none() {
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        !stderr.contains("panicked"),
        "the binary panicked at start-up:\n{stderr}"
    );
}
