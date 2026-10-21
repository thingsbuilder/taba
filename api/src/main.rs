mod alerts;
mod audit;
mod auth;
mod cards;
mod children;
mod config;
mod degraded;
mod endorsements;
mod error;
mod events;
mod exits;
mod geo;
mod households;
mod idempotency;
#[cfg(test)]
mod integration_tests;
mod invites;
mod members;
mod people;
#[cfg(test)]
mod permissions_tests;
mod shelters;
mod status;
mod store;
#[cfg(test)]
mod test_support;
mod thumbnail;
mod unico;
mod volunteers;

use std::net::SocketAddr;

use anyhow::{Context, bail};
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{delete, get, patch, post, put};
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
        .route("/v1/shelters", post(shelters::post_shelters))
        .route(
            "/v1/shelters/{id}",
            get(shelters::get_shelter).patch(shelters::patch_shelter),
        )
        .route("/v1/shelters/{id}/close", post(shelters::close_shelter))
        .route("/v1/shelters/{id}/confirm", post(shelters::confirm_shelter))
        .route("/v1/shelters/{id}/revoke", post(shelters::revoke_shelter))
        .route("/v1/events/{id}", patch(events::patch_event))
        .route(
            "/v1/shelters/{id}/endorsements/in-person/code",
            post(endorsements::create_code),
        )
        .route("/v1/endorsements/in-person", post(endorsements::endorse))
        .route(
            "/v1/shelters/{id}/invites",
            post(invites::create_invite).get(invites::list_invites),
        )
        .route(
            "/v1/invites/{code}",
            get(invites::get_invite).delete(invites::cancel_invite),
        )
        .route("/v1/invites/{code}/accept", post(invites::accept_invite))
        .route("/v1/shelters/{id}/members", get(members::list_members))
        .route(
            "/v1/shelters/{id}/members/{volunteer_id}",
            patch(members::promote_member).delete(members::remove_member),
        )
        .route("/v1/shelters/{id}/leave", post(members::leave_shelter))
        .route("/v1/volunteers/{id}/ban", post(members::ban_volunteer))
        // Phase 3
        .route("/v1/shelters/{id}/checkins", post(people::post_checkin))
        .route("/v1/shelters/{id}/people", get(people::list_people))
        .route(
            "/v1/shelters/{id}/people/needs-selfie",
            get(people::needs_selfie),
        )
        .route(
            "/v1/people/{person_id}",
            get(people::get_person).patch(people::patch_person),
        )
        .route(
            "/v1/people/{person_id}/biometrics",
            put(people::put_biometrics),
        )
        .route(
            "/v1/people/{person_id}/guardians",
            post(children::post_guardian),
        )
        .route(
            "/v1/people/{person_id}/thumbnail",
            get(thumbnail::get_thumbnail),
        )
        .route("/v1/shelters/{id}/exits", post(exits::post_exit))
        .route(
            "/v1/shelters/{id}/households",
            post(households::post_household),
        )
        .route(
            "/v1/households/{id}",
            get(households::get_household).patch(households::patch_household),
        )
        .route("/v1/households/{id}/cards", post(cards::post_card))
        .route("/v1/cards/{code}", get(cards::get_card))
        .route("/v1/cards/{code}/cancel", post(cards::cancel_card))
        .route("/v1/alerts", get(alerts::get_alerts))
        .route("/v1/alerts/{id}/resolve", post(alerts::resolve_alert))
        .fallback(error::not_found)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            idempotency::layer,
        ))
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

/// `taba unico resume` (spec 5.7): leaves the degraded mode caused by the monthly limit.
async fn unico_command(config: Config, args: &[String]) -> anyhow::Result<()> {
    let [verb] = args else {
        bail!("usage: taba unico resume");
    };
    if verb != "resume" {
        bail!("unknown unico command {verb:?}: use resume");
    }
    let db = FirestoreDb::new(&config.gcp_project)
        .await
        .context("failed to connect to Firestore")?;
    let changed = degraded::resume(&db)
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

    // Two rustls providers (ring and aws-lc-rs) are in the dependency tree, so rustls cannot
    // pick one by itself and panics on the first TLS connection. Choose before any client exists.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("no rustls crypto provider was installed yet");

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
        Some("unico") => {
            unico_command(
                Config::from_env()?,
                &std::env::args().skip(2).collect::<Vec<_>>(),
            )
            .await
        }
        other => bail!(
            "usage: taba serve | taba role grant|revoke <volunteer_id> <role> | taba unico resume (got {other:?})"
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
    async fn unico_command_rejects_bad_arguments_before_touching_the_database() {
        let config = test_config("http://127.0.0.1:1");
        for bad in [&[][..], &["pause"], &["resume", "now"]] {
            let err = unico_command(config.clone(), &args(bad)).await.unwrap_err();
            assert!(err.to_string().contains("unico"), "{bad:?}: {err}");
        }
    }

    #[tokio::test]
    async fn unico_resume_leaves_the_degraded_mode() {
        let mut config = test_config("http://127.0.0.1:1");
        config.gcp_project = format!("taba-test-{}", uuid::Uuid::new_v4());
        let db = FirestoreDb::new(&config.gcp_project).await.unwrap();
        degraded::enter_limit(&db).await.unwrap();
        assert_eq!(
            degraded::blocking_reason(&db).await.unwrap(),
            Some(status::Reason::Limit)
        );
        unico_command(config, &args(&["resume"])).await.unwrap();
        assert_eq!(degraded::blocking_reason(&db).await.unwrap(), None);
        let doc = degraded::read(&db).await.unwrap().unwrap();
        assert_eq!(doc.unico, status::Unico::Ok);
    }

    #[tokio::test]
    async fn unico_resume_leaves_the_limit_mode_for_real() {
        // The command opens its own connection to the project, as the operator's does.
        let mut config = test_config("http://127.0.0.1:1");
        config.gcp_project = format!("taba-test-{}", uuid::Uuid::new_v4());
        let db = FirestoreDb::new(&config.gcp_project).await.unwrap();
        degraded::enter_limit(&db).await.unwrap();
        assert!(degraded::blocking_reason(&db).await.unwrap().is_some());

        unico_command(config.clone(), &args(&["resume"]))
            .await
            .unwrap();
        assert!(degraded::blocking_reason(&db).await.unwrap().is_none());
        // Running it again with nothing to resume is not an error.
        unico_command(config, &args(&["resume"])).await.unwrap();
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
