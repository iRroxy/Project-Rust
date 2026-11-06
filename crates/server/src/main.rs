mod error;
mod state;

use crate::error::{AppError, AppResult};
use ai_core::SystemInfo;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderValue, Method, StatusCode, header},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, time::Duration};
use tower_http::{cors::CorsLayer, timeout::TimeoutLayer, trace::TraceLayer};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::state::{AppConfig, AppState};

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    environment: String,
    system: SystemInfo,
}

async fn health_check(State(state): State<AppState>) -> AppResult<Json<HealthResponse>> {
    tracing::debug!("Health check endpoint called");
    Ok(Json(HealthResponse {
        status: "ok",
        environment: state.config.app_env.clone(),
        system: SystemInfo::current(),
    }))
}

#[derive(Deserialize)]
struct EchoRequest {
    message: String,
}

#[derive(Serialize)]
struct EchoResponse {
    reply: String,
    length: usize,
}

async fn echo(Json(payload): Json<EchoRequest>) -> AppResult<Json<EchoResponse>> {
    let trimmed = payload.message.trim();
    if trimmed.is_empty() {
        return Err(error::AppError::BadRequest(
            "message cannot be empty".to_string(),
        ));
    }

    Ok(Json(EchoResponse {
        reply: format!("Echo: {}", trimmed),
        length: trimmed.chars().count(),
    }))
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

    let cors = CorsLayer::new()
        .allow_origin("http://localhost:3000".parse::<HeaderValue>().unwrap())
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]);

    let app = Router::new()
        .route("/api/health", get(health_check))
        .route("/api/echo", post(echo))
        .layer(TraceLayer::new_for_http())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(30),
        ))
        .layer(cors)
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!("server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind TCP listener");

    axum::serve(listener, app).await.expect("Server error");
}
