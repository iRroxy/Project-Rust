mod conversations;
mod error;
mod state;

use crate::error::{AppError, AppResult};
use ai_core::{SystemInfo, llm::ChatMessage};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderValue, Method, StatusCode, header},
    routing::{delete, get, post},
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

    let reply = state
        .llm
        .chat_completion(&[ChatMessage {
            role: "user",
            content: prompt,
        }])
        .await?;
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
        .route(
            "/api/conversations",
            get(conversations::list_conversations).post(conversations::create_conversation),
        )
        .route(
            "/api/conversations/{id}",
            delete(conversations::delete_conversation),
        )
        .route(
            "/api/conversations/{id}/messages",
            get(conversations::list_messages),
        )
        .route("/api/conversations/{id}/chat", post(conversations::chat))
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
        let db = ai_db::connect_lazy("postgres://test:test@localhost:5432/test").unwrap();
        test_app_with_pool(db)
    }

    fn test_app_with_pool(db: ai_db::PgPool) -> Router {
        let config = AppConfig {
            app_env: "test".to_string(),
            port: 0,
            llm_api_base: "https://api.deepseek.com/v1".to_string(),
            llm_api_key: String::new(),
            llm_model: "deepseek-chat".to_string(),
            database_url: "postgres://test:test@localhost:5432/test".to_string(),
        };
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

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_conversation_and_messages_api(pool: ai_db::PgPool) {
        let conv = ai_db::conversations::create_conversation(&pool, Some("api test"))
            .await
            .unwrap();
        let user = ai_db::message::save_message(&pool, conv.id, "user", "hi", None)
            .await
            .unwrap();
        let assistant =
            ai_db::message::save_message(&pool, conv.id, "assistant", "hello!", Some(3))
                .await
                .unwrap();
        for (id, seconds) in [(user.id, 0), (assistant.id, 1)] {
            sqlx::query(
                "UPDATE messages SET created_at = TIMESTAMPTZ '2026-01-01 00:00:00+00' \
                 + $2::int * INTERVAL '1 second' WHERE id = $1",
            )
            .bind(id)
            .bind(seconds)
            .execute(&pool)
            .await
            .unwrap();
        }
        let app = test_app_with_pool(pool.clone());

        // 列表
        let req = Request::builder()
            .uri("/api/conversations")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json[0]["id"], conv.id.to_string());
        assert_eq!(json[0]["title"], "api test");

        // 消息历史(正序)
        let req = Request::builder()
            .uri(format!("/api/conversations/{}/messages", conv.id))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json[0]["role"], "user");
        assert_eq!(json[1]["content"], "hello!");
        assert_eq!(json[0]["content"], "hi");
        assert_eq!(json[0]["token_count"], serde_json::Value::Null);
        assert_eq!(json[1]["role"], "assistant");
        assert_eq!(json[1]["token_count"], 3);

        // 不存在的会话 -> 404
        let req = Request::builder()
            .uri("/api/conversations/00000000-0000-0000-0000-000000000000/messages")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "NOT_FOUND");

        let req = Request::builder()
            .method(Method::DELETE)
            .uri(format!("/api/conversations/{}", conv.id))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*) FROM messages WHERE conversation_id = $1")
                .bind(conv.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(remaining, 0);

        let req = Request::builder()
            .uri(format!("/api/conversations/{}/messages", conv.id))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_message_history_limits_and_isolation(pool: ai_db::PgPool) {
        let conv = ai_db::conversations::create_conversation(&pool, Some("history"))
            .await
            .unwrap();
        let other = ai_db::conversations::create_conversation(&pool, Some("other"))
            .await
            .unwrap();
        let app = test_app_with_pool(pool.clone());

        let req = Request::builder()
            .uri(format!("/api/conversations/{}/messages", conv.id))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let empty: Vec<ai_db::models::Message> = serde_json::from_slice(&body).unwrap();
        assert!(empty.is_empty());

        sqlx::query(
            "INSERT INTO messages (conversation_id, role, content, created_at) \
             SELECT $1, 'user', i::text, TIMESTAMPTZ '2026-01-01 00:00:00+00' \
             + i * INTERVAL '1 second' FROM generate_series(1, 205) AS i",
        )
        .bind(conv.id)
        .execute(&pool)
        .await
        .unwrap();
        ai_db::message::save_message(&pool, other.id, "user", "private to other", None)
            .await
            .unwrap();

        for (query, first) in [
            ("", 156),
            ("?limit=3", 203),
            ("?limit=0", 205),
            ("?limit=999", 6),
        ] {
            let req = Request::builder()
                .uri(format!("/api/conversations/{}/messages{query}", conv.id))
                .body(Body::empty())
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{query}");
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let messages: Vec<ai_db::models::Message> = serde_json::from_slice(&body).unwrap();
            let contents: Vec<_> = messages.iter().map(|m| m.content.as_str()).collect();
            let expected: Vec<_> = (first..=205).map(|i| i.to_string()).collect();
            assert_eq!(contents, expected, "{query}");
            assert!(messages.iter().all(|m| m.conversation_id == conv.id));
        }
    }
}
