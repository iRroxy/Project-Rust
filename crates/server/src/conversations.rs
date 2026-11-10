use crate::{
    error::{AppError, AppResult},
    state::AppState,
};
use ai_core::llm::ChatMessage;
use ai_db::models::{Conversation, Message};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use uuid::Uuid;

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

#[derive(Deserialize)]
pub struct CreateConversationRequest {
    pub title: Option<String>,
}

/// POST /api/conversations
pub async fn create_conversation(
    State(state): State<AppState>,
    Json(payload): Json<CreateConversationRequest>,
) -> AppResult<(StatusCode, Json<Conversation>)> {
    let title = payload
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let conversation = ai_db::conversations::create_conversation(&state.db, title).await?;
    Ok((StatusCode::CREATED, Json(conversation)))
}

/// GET /api/conversations
pub async fn list_conversations(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<Conversation>>> {
    let conversations = ai_db::conversations::list_conversations(&state.db).await?;
    Ok(Json(conversations))
}

/// DELETE /api/conversations/{id}
pub async fn delete_conversation(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    if ai_db::conversations::delete_conversation(&state.db, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound(format!("conversation {id} not found")))
    }
}

#[derive(Deserialize)]
pub struct MessagesQuery {
    pub limit: Option<i64>,
}

/// GET /api/conversations/{id}/messages?limit=50
pub async fn list_messages(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<MessagesQuery>,
) -> AppResult<Json<Vec<Message>>> {
    // 会话不存在要返回 404,而不是 200 + 空数组
    if ai_db::conversations::get_conversation(&state.db, id)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound(format!("conversation {id} not found")));
    }

    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let messages = ai_db::message::get_recent_messages(&state.db, id, limit).await?;
    Ok(Json(messages))
}

#[derive(Deserialize)]
pub struct ConversationChatRequest {
    pub prompt: String,
}

/// POST /api/conversations/{id}/chat
pub async fn chat(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(payload): Json<ConversationChatRequest>,
) -> AppResult<Json<Message>> {
    let prompt = payload.prompt.trim();
    if prompt.is_empty() {
        return Err(AppError::BadRequest("prompt cannot be empty".to_string()));
    }
    if ai_db::conversations::get_conversation(&state.db, id)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound(format!("conversation {id} not found")));
    }

    ai_db::message::save_message(&state.db, id, "user", prompt, None).await?;
    let history = ai_db::message::get_recent_messages(&state.db, id, DEFAULT_LIMIT).await?;
    let messages: Vec<_> = history
        .iter()
        .map(|message| ChatMessage {
            role: &message.role,
            content: &message.content,
        })
        .collect();
    let reply = state.llm.chat_completion(&messages).await?;
    let saved = ai_db::message::save_message(&state.db, id, "assistant", &reply, None).await?;
    Ok(Json(saved))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_app, state::AppConfig};
    use axum::{Router, body::Body, http::Request, routing::post};
    use tower::ServiceExt;

    fn test_app(pool: ai_db::PgPool, api_base: String, api_key: String) -> Router {
        let config = AppConfig {
            app_env: "test".to_string(),
            port: 0,
            llm_api_base: api_base,
            llm_api_key: api_key,
            llm_model: "test".to_string(),
            database_url: String::new(),
        };
        build_app(AppState::new(config, pool))
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn chat_rejects_invalid_input_without_writes(pool: ai_db::PgPool) {
        let conv = ai_db::conversations::create_conversation(&pool, Some("invalid input"))
            .await
            .unwrap();
        let app = test_app(pool.clone(), String::new(), String::new());
        for (id, prompt, expected) in [
            (conv.id, "   ", StatusCode::BAD_REQUEST),
            (Uuid::nil(), "hello", StatusCode::NOT_FOUND),
        ] {
            let req = Request::builder()
                .method("POST")
                .uri(format!("/api/conversations/{id}/chat"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "prompt": prompt }).to_string(),
                ))
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), expected);
        }
        assert!(
            ai_db::message::get_recent_messages(&pool, conv.id, 50)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn chat_preserves_user_on_provider_failure(pool: ai_db::PgPool) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let provider = Router::new().route(
            "/chat/completions",
            post(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        );
        let provider_task = tokio::spawn(async move {
            axum::serve(listener, provider).await.unwrap();
        });

        let conv = ai_db::conversations::create_conversation(&pool, Some("provider failure"))
            .await
            .unwrap();
        let app = test_app(
            pool.clone(),
            format!("http://{address}"),
            "local-test-only".to_string(),
        );
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/conversations/{}/chat", conv.id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"prompt":"  keep this question  "}"#))
            .unwrap();
        let response = app.oneshot(req).await;
        provider_task.abort();
        let resp = response.unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let messages = ai_db::message::get_recent_messages(&pool, conv.id, 50)
            .await
            .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "keep this question");
    }
}
