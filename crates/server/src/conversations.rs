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
    response::{
        Sse,
        sse::{Event, KeepAlive},
    },
};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use tokio::sync::mpsc;
use tokio_stream::{Stream, StreamExt, wrappers::ReceiverStream};
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

async fn prepare_chat(state: &AppState, id: Uuid, prompt: &str) -> AppResult<Vec<Message>> {
    let prompt = prompt.trim();
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
    Ok(ai_db::message::get_recent_messages(&state.db, id, DEFAULT_LIMIT).await?)
}

/// POST /api/conversations/{id}/chat
pub async fn chat(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(payload): Json<ConversationChatRequest>,
) -> AppResult<Json<Message>> {
    let history = prepare_chat(&state, id, &payload.prompt).await?;
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

#[derive(Serialize)]
struct StreamDone {
    message_id: Uuid,
}

/// POST /api/conversations/{id}/chat/stream
pub async fn chat_stream(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(payload): Json<ConversationChatRequest>,
) -> AppResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let history = prepare_chat(&state, id, &payload.prompt).await?;
    let messages: Vec<_> = history
        .iter()
        .map(|message| ChatMessage {
            role: &message.role,
            content: &message.content,
        })
        .collect();
    let stream = state.llm.chat_stream(&messages).await?;
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(16);

    tokio::spawn(async move {
        let result: anyhow::Result<()> = async {
            let mut full_response = String::new();
            // Drop the owning upstream stream before awaiting persistence.
            let completed = {
                tokio::pin!(stream);
                loop {
                    let next = tokio::select! {
                        biased;
                        _ = tx.closed() => break false,
                        next = stream.next() => next,
                    };
                    let Some(chunk) = next else {
                        break true;
                    };
                    let chunk = chunk?;
                    full_response.push_str(&chunk);
                    let event = Event::default().event("delta").json_data(&chunk)?;
                    if tx.send(Ok(event)).await.is_err() {
                        break false;
                    }
                }
            };
            if !completed {
                if !full_response.is_empty() && !full_response.ends_with('\n') {
                    full_response.push('\n');
                }
                full_response.push_str("[Interrupted]");
            }

            let saved =
                ai_db::message::save_message(&state.db, id, "assistant", &full_response, None)
                    .await?;
            if !completed {
                return Ok(());
            }
            let done = Event::default().event("done").json_data(StreamDone {
                message_id: saved.id,
            })?;
            let _ = tx.send(Ok(done)).await;
            Ok(())
        }
        .await;

        if let Err(error) = result {
            tracing::error!(conversation_id = %id, error = ?error, "Streaming conversation failed");
            let error = Event::default().event("error").data(
                r#"{"error":{"code":"INTERNAL_ERROR","message":"Unable to complete streaming reply"}}"#,
            );
            let _ = tx.send(Ok(error)).await;
        }
    });

    Ok(Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_app, state::AppConfig};
    use axum::{Router, body::Body, http::Request, routing::post};
    use eventsource_stream::Eventsource;
    use http_body_util::BodyDataStream;
    use std::{sync::Arc, time::Duration};
    use tokio::sync::Notify;
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

    const FIRST_CHUNK: &str =
        "data: {\"choices\":[{\"delta\":{\"content\":\"你\\n好\\r\\n\"}}]}\n\n";
    const LAST_CHUNKS: &str =
        "data: {\"choices\":[{\"delta\":{\"content\":\"，Rust🦀\"}}]}\n\ndata: [DONE]\n\n";

    struct Provider {
        task: tokio::task::JoinHandle<()>,
        closed: Arc<Notify>,
    }

    impl Drop for Provider {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    struct BodyDropped(Arc<Notify>);

    impl Drop for BodyDropped {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }

    async fn streaming_app(
        pool: ai_db::PgPool,
        first: &'static str,
        tail: &'static str,
        gate: Option<Arc<Notify>>,
    ) -> (Router, Provider) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let closed = Arc::new(Notify::new());
        let body_closed = closed.clone();
        let provider = Router::new().route(
            "/chat/completions",
            post(move || {
                let gate = gate.clone();
                let closed = body_closed.clone();
                async move {
                    let chunks = futures_util::stream::unfold(
                        (
                            [first, tail].into_iter().enumerate(),
                            gate,
                            BodyDropped(closed),
                        ),
                        |(mut chunks, gate, dropped)| async move {
                            let (index, chunk) = chunks.next()?;
                            if index == 1
                                && let Some(gate) = &gate
                            {
                                gate.notified().await;
                            }
                            Some((Ok::<_, Infallible>(chunk), (chunks, gate, dropped)))
                        },
                    );
                    (
                        [("content-type", "text/event-stream")],
                        Body::from_stream(chunks),
                    )
                }
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, provider).await.unwrap();
        });
        (
            test_app(pool, format!("http://{address}"), "local-test-only".into()),
            Provider { task, closed },
        )
    }

    fn stream_request(id: Uuid) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(format!("/api/conversations/{id}/chat/stream"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"prompt":"  stream question  "}"#))
            .unwrap()
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn stream_emits_before_completion_and_done_confirms_saved_reply(pool: ai_db::PgPool) {
        let conv = ai_db::conversations::create_conversation(&pool, None)
            .await
            .unwrap();
        let gate = Arc::new(Notify::new());
        let (app, _provider) =
            streaming_app(pool.clone(), FIRST_CHUNK, LAST_CHUNKS, Some(gate.clone())).await;

        tokio::time::timeout(Duration::from_secs(10), async {
            let response = app.oneshot(stream_request(conv.id)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let events = BodyDataStream::new(response.into_body()).eventsource();
            tokio::pin!(events);

            let first = events.next().await.unwrap().unwrap();
            assert_eq!(first.event, "delta");
            assert_eq!(
                serde_json::from_str::<String>(&first.data).unwrap(),
                "你\n好\r\n"
            );
            let pending = ai_db::message::get_recent_messages(&pool, conv.id, 50)
                .await
                .unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].role, "user");
            assert_eq!(pending[0].content, "stream question");

            gate.notify_one();
            let second = events.next().await.unwrap().unwrap();
            assert_eq!(second.event, "delta");
            assert_eq!(
                serde_json::from_str::<String>(&second.data).unwrap(),
                "，Rust🦀"
            );
            let done = events.next().await.unwrap().unwrap();
            assert_eq!(done.event, "done");
            let done: serde_json::Value = serde_json::from_str(&done.data).unwrap();
            let saved = ai_db::message::get_recent_messages(&pool, conv.id, 50)
                .await
                .unwrap();
            assert_eq!(saved.len(), 2);
            let assistant = saved
                .iter()
                .find(|message| message.role == "assistant")
                .unwrap();
            assert_eq!(assistant.content, "你\n好\r\n，Rust🦀");
            assert_eq!(done["message_id"], assistant.id.to_string());
            assert!(events.next().await.is_none());
        })
        .await
        .expect("stream must emit before the upstream finishes");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn stream_failure_does_not_persist_partial_reply(pool: ai_db::PgPool) {
        for tail in ["", "data: not-json\n\n"] {
            let conv = ai_db::conversations::create_conversation(&pool, None)
                .await
                .unwrap();
            let (app, _provider) = streaming_app(pool.clone(), FIRST_CHUNK, tail, None).await;
            tokio::time::timeout(Duration::from_secs(10), async {
                let response = app.oneshot(stream_request(conv.id)).await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let events = BodyDataStream::new(response.into_body()).eventsource();
                tokio::pin!(events);
                let first = events.next().await.unwrap().unwrap();
                assert_eq!(first.event, "delta");
                let error = events.next().await.unwrap().unwrap();
                assert_eq!(error.event, "error");
                let error: serde_json::Value = serde_json::from_str(&error.data).unwrap();
                assert_eq!(error["error"]["code"], "INTERNAL_ERROR");
                assert!(events.next().await.is_none());
                let saved = ai_db::message::get_recent_messages(&pool, conv.id, 50)
                    .await
                    .unwrap();
                assert_eq!(saved.len(), 1);
                assert_eq!(saved[0].role, "user");
            })
            .await
            .expect("upstream failure must terminate the SSE response");
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn stream_persistence_failure_never_reports_done(pool: ai_db::PgPool) {
        let conv = ai_db::conversations::create_conversation(&pool, None)
            .await
            .unwrap();
        let gate = Arc::new(Notify::new());
        let (app, _provider) =
            streaming_app(pool.clone(), FIRST_CHUNK, LAST_CHUNKS, Some(gate.clone())).await;

        tokio::time::timeout(Duration::from_secs(10), async {
            let response = app.oneshot(stream_request(conv.id)).await.unwrap();
            let events = BodyDataStream::new(response.into_body()).eventsource();
            tokio::pin!(events);
            assert_eq!(events.next().await.unwrap().unwrap().event, "delta");
            assert!(
                ai_db::conversations::delete_conversation(&pool, conv.id)
                    .await
                    .unwrap()
            );
            gate.notify_one();
            assert_eq!(events.next().await.unwrap().unwrap().event, "delta");
            let error = events.next().await.unwrap().unwrap();
            assert_eq!(error.event, "error");
            assert!(events.next().await.is_none());
        })
        .await
        .expect("failed persistence must terminate without a done event");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn disconnect_drops_idle_upstream_before_persisting_interrupted_reply(
        pool: ai_db::PgPool,
    ) {
        for (first, expected, read_first) in [
            (FIRST_CHUNK, "你\n好\r\n[Interrupted]", true),
            (
                "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
                "[Interrupted]",
                false,
            ),
        ] {
            let conv = ai_db::conversations::create_conversation(&pool, None)
                .await
                .unwrap();
            let gate = Arc::new(Notify::new());
            let (app, provider) = streaming_app(pool.clone(), first, LAST_CHUNKS, Some(gate)).await;

            tokio::time::timeout(Duration::from_secs(10), async {
                let held_connections = {
                    let response = app.oneshot(stream_request(conv.id)).await.unwrap();
                    assert_eq!(response.status(), StatusCode::OK);
                    let events = BodyDataStream::new(response.into_body()).eventsource();
                    tokio::pin!(events);
                    if read_first {
                        let event = events.next().await.unwrap().unwrap();
                        assert_eq!(event.event, "delta");
                        assert_eq!(
                            serde_json::from_str::<String>(&event.data).unwrap(),
                            "你\n好\r\n"
                        );
                    }

                    // Block persistence to prove upstream is dropped first.
                    let mut held =
                        Vec::with_capacity(pool.options().get_max_connections() as usize);
                    for _ in 0..pool.options().get_max_connections() {
                        held.push(pool.acquire().await.unwrap());
                    }
                    held
                };

                tokio::time::timeout(Duration::from_secs(5), provider.closed.notified())
                    .await
                    .expect("disconnect must close the idle upstream before database access");
                drop(held_connections);

                loop {
                    let history = ai_db::message::get_recent_messages(&pool, conv.id, 50)
                        .await
                        .unwrap();
                    if let Some(reply) = history.iter().find(|message| message.role == "assistant")
                    {
                        assert_eq!(history.len(), 2);
                        assert_eq!(reply.content, expected);
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("disconnect must cancel and persist without another upstream token");
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn disconnect_after_upstream_completion_keeps_full_reply(pool: ai_db::PgPool) {
        let conv = ai_db::conversations::create_conversation(&pool, None)
            .await
            .unwrap();
        let gate = Arc::new(Notify::new());
        let (app, _provider) =
            streaming_app(pool.clone(), FIRST_CHUNK, LAST_CHUNKS, Some(gate.clone())).await;

        tokio::time::timeout(Duration::from_secs(10), async {
            let transaction = {
                let response = app.oneshot(stream_request(conv.id)).await.unwrap();
                let events = BodyDataStream::new(response.into_body()).eventsource();
                tokio::pin!(events);
                assert_eq!(events.next().await.unwrap().unwrap().event, "delta");

                let mut transaction = pool.begin().await.unwrap();
                sqlx::query("LOCK TABLE messages IN SHARE MODE")
                    .execute(&mut *transaction)
                    .await
                    .unwrap();
                gate.notify_one();

                // Wait until [DONE] has been consumed and the final INSERT is blocked.
                loop {
                    let waiting: bool = sqlx::query_scalar(
                        "SELECT EXISTS (SELECT 1 FROM pg_locks \
                         WHERE relation = 'messages'::regclass AND NOT granted \
                         AND database = (SELECT oid FROM pg_database \
                         WHERE datname = current_database()))",
                    )
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                    if waiting {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                transaction
            };
            transaction.commit().await.unwrap();

            loop {
                let history = ai_db::message::get_recent_messages(&pool, conv.id, 50)
                    .await
                    .unwrap();
                if let Some(reply) = history.iter().find(|message| message.role == "assistant") {
                    assert_eq!(history.len(), 2);
                    assert_eq!(reply.content, "你\n好\r\n，Rust🦀");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a completed upstream reply must remain complete after the client disconnects");
    }
}
