mod config;
mod error;
mod status;

use std::net::SocketAddr;

use anyhow::{Context, bail};
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::get;
use firestore::FirestoreDb;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

use config::Config;

/// Photos are reduced to under 800 KB on the device; 6 MB leaves room for multipart overhead (spec 6.2).
const BODY_LIMIT_BYTES: usize = 6 * 1024 * 1024;
/// Caddy proxies `/v1/*` here (spec 6.4).
const LISTEN_ADDR: &str = "127.0.0.1:8080";

fn router(db: FirestoreDb) -> Router {
    Router::new()
        .route("/v1/status", get(status::get_status))
        .fallback(error::not_found)
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(BODY_LIMIT_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(db)
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let db = FirestoreDb::new(&config.gcp_project)
        .await
        .context("failed to connect to Firestore")?;
    let addr: SocketAddr = LISTEN_ADDR.parse()?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    tracing::info!(%addr, "taba api listening");
    axum::serve(listener, router(db))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// systemd stops the service with SIGTERM; Ctrl-C covers local development.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    tracing::info!("shutting down");
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Development only: loads `.env` when present. On the VM the variables come from systemd.
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=info".into()),
        )
        .init();

    match std::env::args().nth(1).as_deref() {
        Some("serve") => serve(Config::from_env()?).await,
        other => bail!("usage: taba serve (got {other:?})"),
    }
}

#[cfg(test)]
mod test_support {
    use axum::response::Response;
    use firestore::FirestoreDb;
    use http_body_util::BodyExt;

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

    pub async fn body_json(response: Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }
}
