//! Boot smoke test: runs the real binary the way the VM does (no Firestore emulator, a service
//! account for credentials) and checks it gets past the TLS setup without panicking.
//!
//! Incident: v0.2.0 panicked at start-up in production because rustls had two crypto providers
//! (ring and aws-lc-rs) in the dependency tree and none installed. The unit tests never opened a
//! TLS connection, so they could not see it.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const WATCH: Duration = Duration::from_secs(5);
/// The address `taba serve` listens on (Caddy proxies `/v1/*` here).
const LISTEN: &str = "127.0.0.1:8080";

/// One raw HTTP/1.1 request; returns the status line and the rest of the response.
fn request(method: &str, path: &str) -> Option<(String, String)> {
    let mut stream = TcpStream::connect(LISTEN).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: taba\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let (head, body) = response.split_once("\r\n\r\n")?;
    Some((head.lines().next()?.to_string(), body.to_string()))
}

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
        .env("THUMBNAIL_KEY", format!("{}=", "A".repeat(43)))
        .env("PILOT_RESPONSIBLE", "Pilot Team")
        .env("UNICO_URL", "http://127.0.0.1:1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Either it keeps running (listening) or it exits with an ordinary error; never a panic.
    // When it listens, the routes of every phase must answer: building the route table
    // panics on a conflicting path, and that happens only once the server starts.
    let start = Instant::now();
    let mut answered = Vec::new();
    while start.elapsed() < WATCH && child.try_wait().unwrap().is_none() {
        if answered.is_empty() && TcpStream::connect(LISTEN).is_ok() {
            // No session is sent, so the extractor refuses before Firestore is needed.
            for (method, path) in [
                ("GET", "/v1/nope"),
                ("POST", "/v1/shelters"),
                ("PATCH", "/v1/events/e1"),
                ("POST", "/v1/endorsements/in-person"),
                ("GET", "/v1/alerts"),
                ("DELETE", "/v1/shelters/s1/members/v1"),
                // Phase 3 routes, registered since stage 0.
                ("POST", "/v1/shelters/s1/checkins"),
                ("PUT", "/v1/people/p1/biometrics"),
                ("GET", "/v1/people/p1/thumbnail"),
                ("POST", "/v1/shelters/s1/exits"),
                ("GET", "/v1/cards/ABCD2345"),
                ("POST", "/v1/households/h1/cards"),
            ] {
                answered.push((method, path, request(method, path)));
            }
        }
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
    eprintln!("boot: {} routes answered", answered.len());
    // It either listened and answered, or the port was taken (a local API already running).
    assert!(
        !answered.is_empty() || stderr.contains("failed to bind"),
        "the binary neither listened nor reported a busy port:\n{stderr}"
    );
    for (method, path, response) in answered {
        let (status, body) = response.unwrap_or_else(|| panic!("no answer to {method} {path}"));
        let (code, error) = if path == "/v1/nope" {
            ("404", "not_found")
        } else {
            ("401", "unauthorized")
        };
        assert!(status.contains(code), "{method} {path}: {status}");
        assert!(body.contains(error), "{method} {path}: {body}");
    }
}

/// `serve` without the thumbnail key must stop with an ordinary error that names it, not panic
/// and not start listening (spec 6.2.2).
#[test]
fn serve_refuses_to_start_without_the_thumbnail_key() {
    let dir = std::env::temp_dir().join(format!("taba-boot-key-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let credentials = dir.join("service-account.json");
    std::fs::write(
        &credentials,
        r#"{"type":"service_account","project_id":"p","private_key_id":"x","private_key":"not a key","client_email":"a@b.c","client_id":"1","token_uri":"https://oauth2.googleapis.com/token"}"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_taba"))
        .arg("serve")
        .env_remove("FIRESTORE_EMULATOR_HOST")
        .env_remove("THUMBNAIL_KEY")
        .env("GOOGLE_APPLICATION_CREDENTIALS", &credentials)
        .env("GCP_PROJECT", "boot-test")
        .env("DOMAIN", "taba.example.org")
        .env("HMAC_KEY", format!("{}=", "A".repeat(43)))
        .env("UNICO_URL", "http://127.0.0.1:1")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "it started without the key");
    assert!(!stderr.contains("panicked"), "panicked:\n{stderr}");
    assert!(
        stderr.contains("THUMBNAIL_KEY"),
        "does not name the key:\n{stderr}"
    );
}
