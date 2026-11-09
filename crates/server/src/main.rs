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
        return Err(AppError::BadRequest("message cannot be empty".to_string()));
    }

    Ok(Json(EchoResponse {
        reply: format!("Echo: {}", trimmed),
        length: trimmed.chars().count(),
    }))
}

#[derive(Deserialize)]
struct ChatRequest {
    prompt: String,
}

#[derive(Serialize)]
struct ChatResponse {
    reply: String,
}

async fn chat(
    State(state): State<AppState>,
    Json(payload): Json<ChatRequest>,
) -> AppResult<Json<ChatResponse>> {
    let prompt = payload.prompt.trim();
    if prompt.is_empty() {
        return Err(AppError::BadRequest("prompt cannot be empty".to_string()));
    }

    let reply = state.llm.chat_completion(prompt).await?;
    Ok(Json(ChatResponse { reply }))
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
    let db = ai_db::connect(&config.database_url)
        .await
        .expect("Failed to connect to PostgreSQL");
    tracing::info!("database pool ready");
    let state = AppState::new(config, db);

    let app = build_app(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!("server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind TCP listener");

    axum::serve(listener, app).await.expect("Server error");
}

pub fn build_app(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin("http://localhost:3000".parse::<HeaderValue>().unwrap())
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]);

    Router::new()
        .route("/api/health", get(health_check))
        .route("/api/echo", post(echo))
        .route("/api/chat", post(chat))
        .layer(TraceLayer::new_for_http())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(30),
        ))
        .layer(cors)
        .with_state(state)
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_app() -> Router {
        let config = AppConfig {
            app_env: "test".to_string(),
            port: 0,
            llm_api_base: "https://api.deepseek.com/v1".to_string(),
            llm_api_key: String::new(),
            llm_model: "deepseek-chat".to_string(),
            database_url: "postgres://test:test@localhost:5432/test".to_string(),
        };
        let db = ai_db::connect_lazy(&config.database_url).unwrap();
        build_app(AppState::new(config, db))
    }

    #[tokio::test]
    async fn test_health_check() {
        let app = test_app();
        let req = Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_echo_and_bad_request() {
        let app = test_app();
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/echo")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"message": "Hello week 1"}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(json["reply"], "Echo: Hello week 1");
        assert_eq!(json["length"], 12);

        let bad_req = Request::builder()
            .method(Method::POST)
            .uri("/api/echo")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"message": "   "}"#))
            .unwrap();
        let bad_resp = app.oneshot(bad_req).await.unwrap();
        assert_eq!(bad_resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_chat_mock_response() {
        let app = test_app();
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/chat")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"prompt": "Hi Rust"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
