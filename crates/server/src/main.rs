use ai_core::SystemInfo;
use axum::{Json, Router, routing::get};
use serde::Serialize;
use std::net::SocketAddr;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    system: SystemInfo,
}

async fn health_check() -> Json<HealthResponse> {
    tracing::debug!("Health check endpoint called");
    Json(HealthResponse {
        status: "ok",
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

    let app = Router::new().route("/api/health", get(health_check));

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    tracing::info!("server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind TCP listener");

    axum::serve(listener, app).await.expect("Server error");
}
