use crate::{
    error::{AppError, AppResult},
    state::AppState,
};
use ai_db::{ChunkRepository, EMBEDDING_DIMENSIONS, MAX_SEARCH_LIMIT, SimilarChunk};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use std::time::Instant;

const DEFAULT_TOP_K: usize = 5;
const DEFAULT_MIN_SIMILARITY: f32 = 0.0;
const MAX_QUERY_BYTES: usize = 8 * 1024;

fn default_top_k() -> usize {
    DEFAULT_TOP_K
}

fn default_min_similarity() -> f32 {
    DEFAULT_MIN_SIMILARITY
}

#[derive(Debug, Deserialize)]
pub struct VectorSearchRequest {
    pub query: String,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    #[serde(default = "default_min_similarity")]
    pub min_similarity: f32,
    #[serde(default)]
    pub document_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VectorSearchResponse {
    pub query: String,
    pub provider: String,
    pub model: String,
    pub dimensions: usize,
    pub top_k: usize,
    pub min_similarity: f32,
    pub total_hits: usize,
    pub embed_time_ms: f64,
    pub search_time_ms: f64,
    pub total_time_ms: f64,
    pub hits: Vec<SimilarChunk>,
}

/// POST /api/search/vector
///
/// Embeds a natural-language query using the configured [`ai_core::EmbeddingProvider`]
/// and performs Top-K cosine similarity retrieval against PostgreSQL `pgvector` HNSW index.
pub async fn vector_search(
    State(state): State<AppState>,
    Json(payload): Json<VectorSearchRequest>,
) -> AppResult<Json<VectorSearchResponse>> {
    let query = payload.query.trim();
    if query.is_empty() {
        return Err(AppError::BadRequest("query cannot be empty".to_string()));
    }
    if query.len() > MAX_QUERY_BYTES {
        return Err(AppError::BadRequest(format!(
            "query exceeds maximum length of {MAX_QUERY_BYTES} bytes"
        )));
    }
    if payload.top_k == 0 || payload.top_k > MAX_SEARCH_LIMIT {
        return Err(AppError::BadRequest(format!(
            "top_k must be between 1 and {MAX_SEARCH_LIMIT}"
        )));
    }
    if !payload.min_similarity.is_finite() || !(-1.0..=1.0).contains(&payload.min_similarity) {
        return Err(AppError::BadRequest(
            "min_similarity must be finite and within [-1.0, 1.0]".to_string(),
        ));
    }

    let provider_dims = state.embeddings.dimensions();
    if provider_dims != EMBEDDING_DIMENSIONS {
        return Err(AppError::BadRequest(format!(
            "configured embedding provider '{}' outputs {provider_dims} dimensions, \
             but document_chunks pgvector index requires {EMBEDDING_DIMENSIONS}",
            state.embeddings.model_name()
        )));
    }

    let total_started = Instant::now();

    let embed_started = Instant::now();
    let query_vector = state.embeddings.embed_one(query).await?;
    let embed_time_ms = embed_started.elapsed().as_secs_f64() * 1000.0;

    let search_started = Instant::now();
    let hits = ChunkRepository::search_similar_chunks_filtered(
        &state.db,
        &query_vector,
        payload.top_k,
        payload.min_similarity,
        payload.document_id.as_deref(),
    )
    .await?;
    let search_time_ms = search_started.elapsed().as_secs_f64() * 1000.0;
    let total_time_ms = total_started.elapsed().as_secs_f64() * 1000.0;

    tracing::debug!(
        provider = state.embeddings.provider_name(),
        model = state.embeddings.model_name(),
        top_k = payload.top_k,
        hits = hits.len(),
        embed_time_ms,
        search_time_ms,
        "Completed vector semantic search"
    );

    Ok(Json(VectorSearchResponse {
        query: query.to_string(),
        provider: state.embeddings.provider_name().to_string(),
        model: state.embeddings.model_name().to_string(),
        dimensions: provider_dims,
        top_k: payload.top_k,
        min_similarity: payload.min_similarity,
        total_hits: hits.len(),
        embed_time_ms,
        search_time_ms,
        total_time_ms,
        hits,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppConfig, EmbeddingConfig};
    use ai_core::{EmbeddingProvider, MockEmbeddingProvider, OpenAiEmbeddingProvider};
    use ai_db::NewDocumentChunk;
    use axum::{
        Router,
        body::Body,
        http::{Method, Request, StatusCode, header},
        routing::post,
    };
    use http_body_util::BodyExt;
    use serde_json::json;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_config() -> AppConfig {
        AppConfig {
            app_env: "test".to_string(),
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            run_migrations: false,
            cors_allow_origins: vec!["http://localhost:3000".to_string()],
            llm_api_base: "https://api.deepseek.com/v1".to_string(),
            llm_api_key: String::new(),
            llm_model: "deepseek-chat".to_string(),
            llm_reasoning_model: None,
            llm_reasoning_api_base: None,
            llm_reasoning_api_key: None,
            embedding: EmbeddingConfig::default(),
            database_url: "postgres://test:test@localhost:5432/test".to_string(),
        }
    }

    fn search_router(state: AppState) -> Router {
        Router::new()
            .route("/api/search/vector", post(vector_search))
            .with_state(state)
    }

    #[tokio::test]
    async fn vector_search_validates_request_fields_and_provider_dimensions() {
        let db = ai_db::connect_lazy("postgres://test:test@localhost:5432/test").unwrap();
        let app = search_router(AppState::new(test_config(), db.clone()));

        for invalid_payload in [
            r#"{"query": "   ", "top_k": 5}"#,
            r#"{"query": "rust rag", "top_k": 0}"#,
            r#"{"query": "rust rag", "top_k": 101}"#,
            r#"{"query": "rust rag", "min_similarity": 1.5}"#,
            r#"{"query": "rust rag", "min_similarity": -1.5}"#,
        ] {
            let req = Request::builder()
                .method(Method::POST)
                .uri("/api/search/vector")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(invalid_payload))
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::BAD_REQUEST,
                "expected 400 for {invalid_payload}"
            );
        }

        // Dimension mismatch (768 vs 1536) fails cleanly with 400 before touching Postgres.
        let wrong_dim_state = AppState::new(test_config(), db.clone())
            .with_embedding_provider(Arc::new(MockEmbeddingProvider::new(768)));
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/search/vector")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"query": "pgvector HNSW", "top_k": 5}"#))
            .unwrap();
        let resp = search_router(wrong_dim_state).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("768")
        );

        // Missing API key maps cleanly to 503 ServiceUnavailable.
        let missing_key_state = AppState::new(test_config(), db).with_embedding_provider(Arc::new(
            OpenAiEmbeddingProvider::new(
                "http://127.0.0.1:9",
                "",
                "text-embedding-3-small",
                EMBEDDING_DIMENSIONS,
            ),
        ));
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/search/vector")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"query": "pgvector HNSW", "top_k": 5}"#))
            .unwrap();
        let resp = search_router(missing_key_state).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn vector_search_endpoint_returns_ranked_similar_chunks(pool: ai_db::PgPool) {
        let provider = Arc::new(MockEmbeddingProvider::new(EMBEDDING_DIMENSIONS));
        let state =
            AppState::new(test_config(), pool.clone()).with_embedding_provider(provider.clone());

        let docs = [
            (
                "docs/pgvector.md",
                0,
                "h-pgvector",
                "PostgreSQL pgvector HNSW cosine similarity index accelerates vector search",
            ),
            (
                "docs/cooking.md",
                0,
                "h-cooking",
                "Homemade sourdough bread baking temperature and hydration recipe",
            ),
        ];

        let mut chunks = Vec::new();
        for (doc_id, idx, hash, text) in docs {
            let embedding = provider.embed_one(text).await.unwrap();
            chunks.push(NewDocumentChunk {
                document_id: doc_id.to_string(),
                chunk_index: idx,
                content: text.to_string(),
                metadata: json!({ "path": doc_id }),
                content_hash: hash.to_string(),
                token_count: 16,
                embedding,
            });
        }
        ChunkRepository::batch_insert_chunks(&pool, &chunks)
            .await
            .unwrap();

        let app = search_router(state);
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/search/vector")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"query": "pgvector HNSW cosine vector search", "top_k": 5, "min_similarity": 0.1}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let parsed: VectorSearchResponse = serde_json::from_slice(&body).unwrap();

        assert_eq!(parsed.provider, "mock");
        assert_eq!(parsed.dimensions, EMBEDDING_DIMENSIONS);
        assert!(parsed.total_hits >= 1);
        assert_eq!(parsed.hits[0].document_id, "docs/pgvector.md");
        assert!(parsed.hits[0].similarity > 0.5);
    }
}
