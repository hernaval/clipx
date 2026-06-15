mod auth;
mod db;
mod handlers;
mod models;
mod pubsub;

use std::sync::Arc;
use axum::{routing::{delete, get, post}, Router};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::auth::AuthService;
use crate::db::Database;
use crate::handlers::AppState;
use crate::pubsub::PubSubRelay;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "clipx_server=info,tower_http=info".into()),
        )
        .init();

    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite:clipx-server.db?mode=rwc".into());
    let jwt_secret = std::env::var("JWT_SECRET")
        .unwrap_or_else(|_| "clipx-dev-secret-change-in-production!".into());
    let jwt_ttl_hours: i64 = std::env::var("JWT_TTL_HOURS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let refresh_ttl_days: i64 = std::env::var("REFRESH_TTL_DAYS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let redis_url = std::env::var("REDIS_URL").ok();
    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8443".into());

    let db = Database::new(&db_url).await?;
    let auth = AuthService::new(jwt_secret.as_bytes(), jwt_ttl_hours);
    let pubsub = Arc::new(PubSubRelay::new(redis_url));

    let pubsub_clone = pubsub.clone();
    pubsub_clone.start_redis_subscriber().await?;

    let state = Arc::new(AppState { db, auth, pubsub, refresh_ttl_days, jwt_ttl_hours });

    let app = Router::new()
        .route("/api/v1/clips", post(handlers::upload_clip))
        .route("/api/v1/clips", get(handlers::get_clips))
        .route("/api/v1/pair/request", post(handlers::pair_request))
        .route("/api/v1/pair/confirm", post(handlers::pair_confirm))
        .route("/api/v1/devices", get(handlers::get_devices))
        .route("/api/v1/devices/{id}", delete(handlers::revoke_device))
        .route("/api/v1/audit-logs", get(handlers::get_audit_logs))
        .route("/api/v1/ws", get(handlers::ws_handler))
        .route("/api/v1/sse", get(handlers::sse_handler))
        .route("/api/v1/bootstrap", post(handlers::bootstrap))
        .route("/api/v1/auth/refresh", post(handlers::refresh_token))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    info!("ClipX server starting on {bind_addr}");
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
            info!("Shutting down server...");
        })
        .await?;

    Ok(())
}
