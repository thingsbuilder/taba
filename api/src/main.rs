mod audit;
mod auth;
mod config;
mod error;
mod members;
mod status;
mod store;
#[cfg(test)]
mod test_support;
mod unico;
mod volunteers;

use std::net::SocketAddr;

use anyhow::{Context, bail};
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post};
use firestore::FirestoreDb;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

use auth::AppState;
use config::Config;
use members::Change;
use unico::Unico;

/// Photos are reduced to under 800 KB on the device; 6 MB leaves room for multipart overhead (spec 6.2).
const BODY_LIMIT_BYTES: usize = 6 * 1024 * 1024;
/// Caddy proxies `/v1/*` here (spec 6.4).
const LISTEN_ADDR: &str = "127.0.0.1:8080";

fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/status", get(status::get_status))
        .route("/v1/captures", post(auth::post_captures))
        .route("/v1/sessions", post(auth::post_sessions))
        .route("/v1/sessions/current", delete(auth::delete_current_session))
        .route(
            "/v1/me",
            get(volunteers::get_me).patch(volunteers::patch_me),
        )
        .fallback(error::not_found)
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(BODY_LIMIT_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let db = FirestoreDb::new(&config.gcp_project)
        .await
        .context("failed to connect to Firestore")?;
    let unico = Unico::new(&config.unico_url, config.human_token_access_key.clone());
    if config.human_token_access_key.is_none() {
        tracing::warn!("HUMAN_TOKEN_ACCESS_KEY is not set: every Unico call will fail");
    }
    let state = AppState::new(db, config, unico)?;
    let addr: SocketAddr = LISTEN_ADDR.parse()?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    tracing::info!(%addr, "taba api listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// `taba role grant|revoke <volunteer_id> <validator|civil_defense>` (spec 2.1).
async fn role(config: Config, args: &[String]) -> anyhow::Result<()> {
    let [verb, volunteer_id, role] = args else {
        bail!("usage: taba role grant|revoke <volunteer_id> validator|civil_defense");
    };
    let change = match verb.as_str() {
        "grant" => Change::Grant,
        "revoke" => Change::Revoke,
        _ => bail!("unknown role command {verb:?}: use grant or revoke"),
    };
    let Some(role) = members::parse_cli_role(role) else {
        bail!("the command line only handles validator and civil_defense (got {role:?})");
    };
    let db = FirestoreDb::new(&config.gcp_project)
        .await
        .context("failed to connect to Firestore")?;
    let changed = members::change_role(&db, volunteer_id, role, change)
        .await
        .map_err(|err| anyhow::anyhow!("{} {}", err.status.as_u16(), err.code))?;
    println!("{}", if changed { "done" } else { "nothing to change" });
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
        Some("role") => {
            role(
                Config::from_env()?,
                &std::env::args().skip(2).collect::<Vec<_>>(),
            )
            .await
        }
        other => bail!(
            "usage: taba serve | taba role grant|revoke <volunteer_id> <role> (got {other:?})"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_config;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn role_command_rejects_bad_arguments_before_touching_the_database() {
        let config = test_config("http://127.0.0.1:1");
        for bad in [
            &["grant", "id"][..],
            &["grant", "id", "validator", "extra"],
            &["promote", "id", "validator"],
            &["grant", "id", "mediator"],
            &["revoke", "id", "protection"],
        ] {
            assert!(role(config.clone(), &args(bad)).await.is_err(), "{bad:?}");
        }
    }
}
