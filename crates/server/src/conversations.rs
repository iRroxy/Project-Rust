use crate::{
    error::{AppError, AppResult},
    state::AppState,
};
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
