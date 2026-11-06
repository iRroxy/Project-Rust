mod state;

use ai_core::SystemInfo;
use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;
use std::net::SocketAddr;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::state::{AppConfig, AppState};

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    environment: String,
    system: SystemInfo,
}

async fn health_check(State(state): State<AppState>) -> Json<HealthResponse> {
    tracing::debug!("Health check endpoint called");
    Json(HealthResponse {
        status: "ok",
        environment: state.config.app_env.clone(),
        system: SystemInfo::current(),
    })
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,ai_server=debug".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = AppConfig::from_env();
    let port = config.port;
    let state = AppState::new(config);

    let app = Router::new()
        .route("/api/health", get(health_check))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!("server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind TCP listener");

    axum::serve(listener, app).await.expect("Server error");
}
