//! Shared test helpers: an isolated Firestore emulator project, a fake Unico server and an
//! `AppState` wired to both. Automated tests never reach the real Unico.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use firestore::FirestoreDb;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::auth::AppState;
use crate::config::{Config, Secret};
use crate::router;
use crate::unico::Unico;

pub const TEST_DOMAIN: &str = "taba.example.org";
pub const TEST_ACCESS_KEY: &str = "test-access-key";

/// Each test gets its own project id, so data is isolated inside the shared emulator.
pub async fn test_db() -> FirestoreDb {
    assert!(
        std::env::var("FIRESTORE_EMULATOR_HOST").is_ok(),
        "tests need the Firestore emulator: export FIRESTORE_EMULATOR_HOST=127.0.0.1:<port>"
    );
    FirestoreDb::new(format!("taba-test-{}", uuid::Uuid::new_v4()))
        .await
        .expect("connect to the Firestore emulator")
}

pub async fn body_json(response: Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[derive(Default)]
pub struct FakeState {
    /// Replies for `POST /process`, used in order; when empty a new journey is created.
    pub post_replies: VecDeque<(u16, Value)>,
    /// Replies for `GET /process/{id}` that take priority over `processes`.
    pub get_replies: VecDeque<(u16, Value)>,
    /// Process bodies by id, for `GET /process/{id}`.
    pub processes: HashMap<String, Value>,
    pub posts: Vec<Value>,
    pub gets: Vec<String>,
    pub access_keys: Vec<String>,
    /// Each request takes this long to answer.
    pub delay: Duration,
    pub in_flight: usize,
    pub max_in_flight: usize,
}

/// A stand-in for the Unico API on a local port.
#[derive(Clone)]
pub struct FakeUnico {
    pub url: String,
    pub state: Arc<Mutex<FakeState>>,
}

impl FakeUnico {
    pub async fn start() -> Self {
        let state = Arc::new(Mutex::new(FakeState::default()));
        let app = Router::new()
            .route("/process", post(fake_create))
            .route("/process/{id}", get(fake_get))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { url, state }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut FakeState) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }

    pub fn post_count(&self) -> usize {
        self.with(|s| s.posts.len())
    }

    pub fn get_count(&self) -> usize {
        self.with(|s| s.gets.len())
    }

    /// Registers the answer of `GET /process/{id}` for the journey created next.
    pub fn finish_with_token(&self, process_id: &str, token: &str) {
        self.with(|s| {
            s.processes.insert(
                process_id.into(),
                json!({ "id": process_id, "state": "PROCESS_STATE_FINISHED", "data": { "humanToken": { "value": token } } }),
            )
        });
    }

    pub fn set_state(&self, process_id: &str, state: &str) {
        self.with(|s| {
            s.processes.insert(
                process_id.into(),
                json!({ "id": process_id, "state": state }),
            )
        });
    }
}

async fn enter(state: &Arc<Mutex<FakeState>>, key: Option<String>) -> Duration {
    let mut s = state.lock().unwrap();
    s.access_keys.extend(key);
    s.in_flight += 1;
    s.max_in_flight = s.max_in_flight.max(s.in_flight);
    s.delay
}

fn reply(status: u16, body: Value) -> Response {
    (StatusCode::from_u16(status).unwrap(), axum::Json(body)).into_response()
}

fn key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-unico-accesskey")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

async fn fake_create(
    State(state): State<Arc<Mutex<FakeState>>>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let delay = enter(&state, key(&headers)).await;
    tokio::time::sleep(delay).await;
    let mut s = state.lock().unwrap();
    s.in_flight -= 1;
    s.posts.push(body);
    if let Some((status, body)) = s.post_replies.pop_front() {
        return reply(status, body);
    }
    let n = s.posts.len();
    let id = format!("proc-{n}");
    reply(
        200,
        json!({ "id": id, "link": format!("https://journey.test/{id}?collect-data=true"), "state": "PROCESS_STATE_CREATED" }),
    )
}

async fn fake_get(
    State(state): State<Arc<Mutex<FakeState>>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let delay = enter(&state, key(&headers)).await;
    tokio::time::sleep(delay).await;
    let mut s = state.lock().unwrap();
    s.in_flight -= 1;
    s.gets.push(id.clone());
    if let Some((status, body)) = s.get_replies.pop_front() {
        return reply(status, body);
    }
    match s.processes.get(&id) {
        Some(process) => reply(200, process.clone()),
        None => reply(200, json!({ "id": id, "state": "PROCESS_STATE_CREATED" })),
    }
}

/// Every document of a collection as plain JSON, in no particular order.
pub async fn all_docs(db: &FirestoreDb, collection: &str) -> Vec<Value> {
    db.fluent()
        .select()
        .from(collection)
        .obj()
        .query()
        .await
        .unwrap()
}

pub async fn audit_actions(db: &FirestoreDb) -> Vec<String> {
    let mut actions: Vec<String> = all_docs(db, "audit_log")
        .await
        .iter()
        .map(|e| e["action"].as_str().unwrap().to_string())
        .collect();
    actions.sort();
    actions
}

pub fn test_config(unico_url: &str) -> Config {
    let hmac_key = STANDARD.encode([7u8; 32]);
    let vars: HashMap<&str, String> = HashMap::from([
        ("GCP_PROJECT", "test-project".to_string()),
        ("DOMAIN", TEST_DOMAIN.to_string()),
        ("HMAC_KEY", hmac_key),
        ("UNICO_URL", unico_url.to_string()),
        ("HUMAN_TOKEN_ACCESS_KEY", TEST_ACCESS_KEY.to_string()),
    ]);
    Config::from_lookup(|name| vars.get(name).cloned()).unwrap()
}

/// A client whose backoff steps last one millisecond instead of one second.
pub fn fast_unico(url: &str, key: Option<&str>) -> Unico {
    Unico::with_unit(url, key.map(|k| Secret(k.into())), Duration::from_millis(1))
}

pub struct TestApp {
    pub state: AppState,
    pub unico: FakeUnico,
}

impl TestApp {
    pub async fn new() -> Self {
        Self::with_db(test_db().await).await
    }

    pub async fn with_db(db: FirestoreDb) -> Self {
        let unico = FakeUnico::start().await;
        let config = test_config(&unico.url);
        let client = fast_unico(&unico.url, Some(TEST_ACCESS_KEY));
        let state = AppState::new(db, config, client).unwrap();
        Self { state, unico }
    }

    /// Sends one request through the real router. `headers` are added as given.
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = match body {
            Some(body) => builder
                .header("content-type", "application/json")
                .body(Body::from(body.to_string())),
            None => builder.body(Body::empty()),
        }
        .unwrap();
        let response = router(self.state.clone()).oneshot(request).await.unwrap();
        let (status, response_headers) = (response.status(), response.headers().clone());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, response_headers, json)
    }

    pub async fn post(&self, path: &str, device: &str, body: Value) -> (StatusCode, Value) {
        let (status, _, json) = self
            .request("POST", path, &[("x-device-id", device)], Some(body))
            .await;
        (status, json)
    }

    pub async fn get_authed(&self, path: &str, session: &str) -> (StatusCode, Value) {
        let auth = format!("Bearer {session}");
        let (status, _, json) = self
            .request("GET", path, &[("authorization", auth.as_str())], None)
            .await;
        (status, json)
    }

    /// Full login for tests that only need a session: capture, journey done with `token`, session.
    /// Returns `(session_secret, volunteer_id)`.
    pub async fn login(&self, device: &str, token: &str, name: &str) -> (String, String) {
        let (status, capture) = self
            .post("/v1/captures", device, json!({ "purpose": "login" }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{capture}");
        let capture_id = capture["capture_id"].as_str().unwrap().to_string();
        let process_id = self.unico.with(|s| format!("proc-{}", s.posts.len()));
        self.unico.finish_with_token(&process_id, token);
        let profile = json!({ "name": name, "show_phone": false, "language": "pt-BR" });
        let (status, body) = self
            .post(
                "/v1/sessions",
                device,
                json!({ "capture_id": capture_id, "profile": profile }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        (
            body["session"].as_str().unwrap().to_string(),
            body["volunteer"]["id"].as_str().unwrap().to_string(),
        )
    }
}
