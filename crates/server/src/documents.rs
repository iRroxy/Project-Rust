use std::{sync::OnceLock, time::Instant};
use tokio::sync::Semaphore;

use ai_db::{ChunkRepository, EMBEDDING_DIMENSIONS, NewDocumentChunk, StoredDocumentChunk};
use ai_parser::{
    DiffCalculator, DocumentChunk, DocumentChunker, DocumentChunkerConfig, DocumentFingerprint,
    ParseError, ParsedDocument, RepositoryScanner, TokenizerEncoding, hash_text,
};
use axum::{Json, extract::State};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_BATCH_FILES: usize = 100;
const MIN_CHUNK_SIZE_TOKENS: usize = 32;
const MAX_CHUNK_SIZE_TOKENS: usize = 4096;
const MAX_CONCURRENT_PARSE_JOBS: usize = 8;

static PARSE_SEMAPHORE: OnceLock<Semaphore> = OnceLock::new();

fn parse_semaphore() -> &'static Semaphore {
    PARSE_SEMAPHORE.get_or_init(|| Semaphore::new(MAX_CONCURRENT_PARSE_JOBS))
}

#[derive(Debug, Deserialize)]
pub struct ParseDocumentRequest {
    pub filename: String,
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ParseDocumentResponse {
    pub document: ParsedDocument,
    pub fingerprint: DocumentFingerprint,
    pub parse_time_ms: f64,
}

#[derive(Debug, Deserialize)]
pub struct ParseBatchRequest {
    pub files: Vec<ParseDocumentRequest>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ParseBatchItem {
    pub document: ParsedDocument,
    pub fingerprint: DocumentFingerprint,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ParseBatchFailure {
    pub filename: String,
    pub error: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ParseBatchResponse {
    pub parsed: Vec<ParseBatchItem>,
    pub failed: Vec<ParseBatchFailure>,
    pub total_bytes: usize,
    pub elapsed_ms: f64,
}

fn default_max_tokens() -> usize {
    256
}

fn default_overlap_tokens() -> usize {
    32
}

fn default_min_chunk_tokens() -> usize {
    32
}

fn default_include_headers() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct ChunkDocumentRequest {
    pub filename: String,
    pub content: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,
    #[serde(default = "default_overlap_tokens")]
    pub overlap_tokens: usize,
    #[serde(default = "default_min_chunk_tokens")]
    pub min_chunk_tokens: usize,
    #[serde(default)]
    pub encoding: TokenizerEncoding,
    #[serde(default = "default_include_headers")]
    pub include_context_headers: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChunkDocumentResponse {
    pub chunks: Vec<DocumentChunk>,
    pub total_chunks: usize,
    pub total_tokens: usize,
    pub avg_tokens: f64,
    pub chunk_time_ms: f64,
}

#[derive(Debug, Deserialize)]
pub struct IngestDocumentRequest {
    pub filename: String,
    pub content: String,
    #[serde(default)]
    pub document_id: Option<String>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,
    #[serde(default = "default_overlap_tokens")]
    pub overlap_tokens: usize,
    #[serde(default = "default_min_chunk_tokens")]
    pub min_chunk_tokens: usize,
    #[serde(default)]
    pub encoding: TokenizerEncoding,
    #[serde(default = "default_include_headers")]
    pub include_context_headers: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IngestDocumentResponse {
    pub document_id: String,
    pub filename: String,
    pub provider: String,
    pub model: String,
    pub dimensions: usize,
    pub total_chunks: usize,
    pub deduplicated_count: usize,
    pub upserted_rows: usize,
    pub total_tokens: usize,
    pub chunk_time_ms: f64,
    pub embed_time_ms: f64,
    pub upsert_time_ms: f64,
    pub total_time_ms: f64,
    pub chunks: Vec<StoredDocumentChunk>,
}

fn map_parse_error(err: ParseError) -> AppError {
    match err {
        ParseError::UnsupportedLanguage(ext) => AppError::BadRequest(format!(
            "unsupported file extension '{ext}'. Supported: .md, .mdx, .txt, .rs, .ts, .tsx, .py"
        )),
        other => AppError::Internal(other.into()),
    }
}

/// POST /api/documents/parse - Parse a single Markdown, text, or source code document.
pub async fn parse_document(
    Json(payload): Json<ParseDocumentRequest>,
) -> AppResult<Json<ParseDocumentResponse>> {
    let filename = payload.filename.trim().to_string();
    if filename.is_empty() {
        return Err(AppError::BadRequest("filename cannot be empty".to_string()));
    }
    if payload.content.is_empty() {
        return Err(AppError::BadRequest("content cannot be empty".to_string()));
    }
    if payload.content.len() > MAX_DOCUMENT_BYTES {
        return Err(AppError::BadRequest(format!(
            "content exceeds maximum size of {} bytes",
            MAX_DOCUMENT_BYTES
        )));
    }

    let _permit = parse_semaphore().try_acquire().map_err(|_| {
        AppError::ServiceUnavailable("document parser is busy; please retry shortly".to_string())
    })?;

    let content = payload.content;
    let (document, fingerprint, parse_time_ms) = tokio::task::spawn_blocking(move || {
        let start = Instant::now();
        let document = RepositoryScanner::parse_content(&filename, &content)?;
        let fingerprint = DiffCalculator::fingerprint_document(&document);
        let parse_time_ms = start.elapsed().as_secs_f64() * 1000.0;
        Ok::<_, ParseError>((document, fingerprint, parse_time_ms))
    })
    .await
    .map_err(|err| AppError::Internal(err.into()))?
    .map_err(map_parse_error)?;

    Ok(Json(ParseDocumentResponse {
        document,
        fingerprint,
        parse_time_ms,
    }))
}

/// POST /api/documents/parse-batch - Parse multiple documents in parallel using Rayon.
pub async fn parse_documents_batch(
    Json(payload): Json<ParseBatchRequest>,
) -> AppResult<Json<ParseBatchResponse>> {
    if payload.files.is_empty() {
        return Err(AppError::BadRequest(
            "files list cannot be empty".to_string(),
        ));
    }
    if payload.files.len() > MAX_BATCH_FILES {
        return Err(AppError::BadRequest(format!(
            "batch exceeds maximum of {MAX_BATCH_FILES} files"
        )));
    }

    let _permit = parse_semaphore().try_acquire().map_err(|_| {
        AppError::ServiceUnavailable("document parser is busy; please retry shortly".to_string())
    })?;

    let files = payload.files;
    let response = tokio::task::spawn_blocking(move || {
        let start = Instant::now();

        let results: Vec<Result<ParseBatchItem, ParseBatchFailure>> = files
            .into_par_iter()
            .map(|file| {
                let name = file.filename.trim().to_string();
                if name.is_empty() {
                    return Err(ParseBatchFailure {
                        filename: file.filename,
                        error: "filename cannot be empty".to_string(),
                    });
                }
                if file.content.len() > MAX_DOCUMENT_BYTES {
                    return Err(ParseBatchFailure {
                        filename: name,
                        error: format!("exceeds maximum size of {MAX_DOCUMENT_BYTES} bytes"),
                    });
                }
                match RepositoryScanner::parse_content(&name, &file.content) {
                    Ok(document) => {
                        let fingerprint = DiffCalculator::fingerprint_document(&document);
                        Ok(ParseBatchItem {
                            document,
                            fingerprint,
                        })
                    }
                    Err(ParseError::UnsupportedLanguage(ext)) => Err(ParseBatchFailure {
                        filename: name,
                        error: format!("unsupported file extension '{ext}'. Supported: .md, .mdx, .txt, .rs, .ts, .tsx, .py"),
                    }),
                    Err(err) => {
                        tracing::warn!(filename = %name, error = %err, "Batch parse failed for file");
                        Err(ParseBatchFailure {
                            filename: name,
                            error: "failed to parse document".to_string(),
                        })
                    }
                }
            })
            .collect();

        let mut parsed = Vec::new();
        let mut failed = Vec::new();
        let mut total_bytes = 0usize;

        for item in results {
            match item {
                Ok(ok) => {
                    total_bytes += ok.document.size_bytes;
                    parsed.push(ok);
                }
                Err(err) => failed.push(err),
            }
        }

        ParseBatchResponse {
            parsed,
            failed,
            total_bytes,
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        }
    })
    .await
    .map_err(|err| AppError::Internal(err.into()))?;

    Ok(Json(response))
}

/// POST /api/documents/chunk - Run semantic/AST-aware chunking with live tunable parameters.
pub async fn chunk_document(
    Json(payload): Json<ChunkDocumentRequest>,
) -> AppResult<Json<ChunkDocumentResponse>> {
    let filename = payload.filename.trim().to_string();
    if filename.is_empty() {
        return Err(AppError::BadRequest("filename cannot be empty".to_string()));
    }
    if payload.content.is_empty() {
        return Err(AppError::BadRequest("content cannot be empty".to_string()));
    }
    if payload.content.len() > MAX_DOCUMENT_BYTES {
        return Err(AppError::BadRequest(format!(
            "content exceeds maximum size of {} bytes",
            MAX_DOCUMENT_BYTES
        )));
    }
    if payload.max_tokens < MIN_CHUNK_SIZE_TOKENS || payload.max_tokens > MAX_CHUNK_SIZE_TOKENS {
        return Err(AppError::BadRequest(format!(
            "max_tokens must be between {MIN_CHUNK_SIZE_TOKENS} and {MAX_CHUNK_SIZE_TOKENS}"
        )));
    }
    if payload.overlap_tokens >= payload.max_tokens {
        return Err(AppError::BadRequest(
            "overlap_tokens must be strictly less than max_tokens".to_string(),
        ));
    }

    let config = DocumentChunkerConfig {
        max_tokens: payload.max_tokens,
        overlap_tokens: payload.overlap_tokens,
        min_chunk_tokens: payload.min_chunk_tokens,
        encoding: payload.encoding,
        include_context_headers: payload.include_context_headers,
    };
    let _permit = parse_semaphore().try_acquire().map_err(|_| {
        AppError::ServiceUnavailable("document chunker is busy; please retry shortly".to_string())
    })?;

    let content = payload.content;

    let response = tokio::task::spawn_blocking(move || {
        let start = Instant::now();
        let file_id = hash_text(&filename)[..24].to_string();
        let chunker = DocumentChunker::new(config);
        let chunks = chunker.chunk_file(&file_id, &filename, &content)?;
        let total_chunks = chunks.len();
        let total_tokens: usize = chunks.iter().map(|c| c.token_count).sum();
        let avg_tokens = if total_chunks == 0 {
            0.0
        } else {
            total_tokens as f64 / total_chunks as f64
        };
        let chunk_time_ms = start.elapsed().as_secs_f64() * 1000.0;
        Ok::<_, ParseError>(ChunkDocumentResponse {
            chunks,
            total_chunks,
            total_tokens,
            avg_tokens,
            chunk_time_ms,
        })
    })
    .await
    .map_err(|err| AppError::Internal(err.into()))?
    .map_err(map_parse_error)?;

    Ok(Json(response))
}

/// POST /api/documents/ingest - Full RAG ingestion pipeline:
/// parse -> semantic/AST chunk -> rate-limited batch embed -> pgvector bulk upsert.
pub async fn ingest_document(
    State(state): State<AppState>,
    Json(payload): Json<IngestDocumentRequest>,
) -> AppResult<Json<IngestDocumentResponse>> {
    let filename = payload.filename.trim().replace('\\', "/");
    if filename.is_empty() {
        return Err(AppError::BadRequest("filename cannot be empty".to_string()));
    }
    if payload.content.trim().is_empty() {
        return Err(AppError::BadRequest("content cannot be empty".to_string()));
    }
    if payload.content.len() > MAX_DOCUMENT_BYTES {
        return Err(AppError::BadRequest(format!(
            "content exceeds maximum size of {} bytes",
            MAX_DOCUMENT_BYTES
        )));
    }
    if payload.max_tokens < MIN_CHUNK_SIZE_TOKENS || payload.max_tokens > MAX_CHUNK_SIZE_TOKENS {
        return Err(AppError::BadRequest(format!(
            "max_tokens must be between {MIN_CHUNK_SIZE_TOKENS} and {MAX_CHUNK_SIZE_TOKENS}"
        )));
    }
    if payload.overlap_tokens >= payload.max_tokens {
        return Err(AppError::BadRequest(
            "overlap_tokens must be strictly less than max_tokens".to_string(),
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

    let document_id = payload
        .document_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| filename.clone());

    let total_started = Instant::now();

    let config = DocumentChunkerConfig {
        max_tokens: payload.max_tokens,
        overlap_tokens: payload.overlap_tokens,
        min_chunk_tokens: payload.min_chunk_tokens,
        encoding: payload.encoding,
        include_context_headers: payload.include_context_headers,
    };

    let (chunks, chunk_time_ms) = {
        let _permit = parse_semaphore().try_acquire().map_err(|_| {
            AppError::ServiceUnavailable(
                "document chunker is busy; please retry shortly".to_string(),
            )
        })?;

        let chunk_filename = filename.clone();
        let chunk_doc_id = document_id.clone();
        let content = payload.content;

        tokio::task::spawn_blocking(move || {
            let started = Instant::now();
            let chunker = DocumentChunker::new(config);
            let chunks = chunker.chunk_file(&chunk_doc_id, &chunk_filename, &content)?;
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            Ok::<_, ParseError>((chunks, elapsed_ms))
        })
        .await
        .map_err(|err| AppError::Internal(err.into()))?
        .map_err(map_parse_error)?
    };

    let total_chunks = chunks.len();
    let total_tokens: usize = chunks.iter().map(|c| c.token_count).sum();

    let (paired, embed_stats) = state
        .embedding_pipeline
        .embed_items(chunks, |c| c.content.as_str())
        .await?;

    let mut new_chunks = Vec::with_capacity(paired.len());
    for (chunk, embedding) in paired {
        let metadata =
            serde_json::to_value(&chunk.metadata).map_err(|err| AppError::Internal(err.into()))?;
        let chunk_index = i32::try_from(chunk.chunk_index).unwrap_or(i32::MAX);
        let token_count = i32::try_from(chunk.token_count).unwrap_or(i32::MAX);
        new_chunks.push(NewDocumentChunk {
            document_id: document_id.clone(),
            chunk_index,
            content: chunk.content,
            metadata,
            content_hash: chunk.content_hash,
            token_count,
            embedding,
        });
    }

    let upsert_started = Instant::now();
    let summary = ChunkRepository::batch_insert_chunks(&state.db, &new_chunks).await?;
    let upsert_time_ms = upsert_started.elapsed().as_secs_f64() * 1000.0;
    let total_time_ms = total_started.elapsed().as_secs_f64() * 1000.0;

    tracing::info!(
        document_id = %document_id,
        filename = %filename,
        total_chunks,
        upserted_rows = summary.upserted_rows,
        deduplicated_count = summary.deduplicated_count,
        chunk_time_ms,
        embed_time_ms = embed_stats.elapsed_ms,
        upsert_time_ms,
        "Completed document ingestion pipeline"
    );

    Ok(Json(IngestDocumentResponse {
        document_id,
        filename,
        provider: state.embeddings.provider_name().to_string(),
        model: state.embeddings.model_name().to_string(),
        dimensions: provider_dims,
        total_chunks,
        deduplicated_count: summary.deduplicated_count,
        upserted_rows: summary.upserted_rows,
        total_tokens,
        chunk_time_ms,
        embed_time_ms: embed_stats.elapsed_ms,
        upsert_time_ms,
        total_time_ms,
        chunks: summary.chunks,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        build_app,
        search::VectorSearchResponse,
        state::{AppConfig, AppState},
    };
    use ai_core::{
        BatchEmbeddingConfig, BatchEmbeddingPipeline, EmbeddingProvider, MockEmbeddingProvider,
    };
    use axum::{
        body::Body,
        http::{Method, Request, StatusCode, header},
    };
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_config() -> AppConfig {
        AppConfig {
            app_env: "test".to_string(),
            host: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            port: 0,
            run_migrations: false,
            cors_allow_origins: vec!["http://localhost:3000".to_string()],
            llm_api_base: "https://api.deepseek.com/v1".to_string(),
            llm_api_key: String::new(),
            llm_model: "deepseek-chat".to_string(),
            llm_reasoning_model: None,
            llm_reasoning_api_base: None,
            llm_reasoning_api_key: None,
            embedding: crate::state::EmbeddingConfig::default(),
            database_url: "postgres://test:test@localhost:5432/test".to_string(),
        }
    }

    fn test_router() -> axum::Router {
        let config = test_config();
        let db = ai_db::connect_lazy(&config.database_url).unwrap();
        build_app(AppState::new(config, db))
    }

    #[tokio::test]
    async fn test_parse_document_endpoint_markdown_and_rust() {
        let app = test_router();

        // 1. Markdown request
        let md_req = Request::builder()
            .method(Method::POST)
            .uri("/api/documents/parse")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "filename": "guide.md",
                    "content": "# Architecture\n\n## Gateway\nBuilt with Axum.\n"
                })
                .to_string(),
            ))
            .unwrap();

        let md_resp = app.clone().oneshot(md_req).await.unwrap();
        assert_eq!(md_resp.status(), StatusCode::OK);
        let md_body = md_resp.into_body().collect().await.unwrap().to_bytes();
        let parsed_md: ParseDocumentResponse = serde_json::from_slice(&md_body).unwrap();
        assert_eq!(parsed_md.document.path, "guide.md");
        assert_eq!(parsed_md.fingerprint.content_hash.len(), 64);
        assert_eq!(parsed_md.fingerprint.unit_hashes.len(), 2);

        // 2. Rust code request
        let rs_req = Request::builder()
            .method(Method::POST)
            .uri("/api/documents/parse")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "filename": "src/engine.rs",
                    "content": "/// Search engine\npub struct Engine;\nimpl Engine { pub fn run(&self) {} }"
                })
                .to_string(),
            ))
            .unwrap();

        let rs_resp = app.clone().oneshot(rs_req).await.unwrap();
        assert_eq!(rs_resp.status(), StatusCode::OK);
        let rs_body = rs_resp.into_body().collect().await.unwrap().to_bytes();
        let parsed_rs: ParseDocumentResponse = serde_json::from_slice(&rs_body).unwrap();
        assert_eq!(parsed_rs.document.path, "src/engine.rs");
        assert_eq!(parsed_rs.fingerprint.unit_hashes.len(), 3);

        // 3. Unsupported extension returns 400
        let bad_req = Request::builder()
            .method(Method::POST)
            .uri("/api/documents/parse")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "filename": "archive.zip",
                    "content": "binary"
                })
                .to_string(),
            ))
            .unwrap();
        let bad_resp = app.oneshot(bad_req).await.unwrap();
        assert_eq!(bad_resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_parse_documents_batch_endpoint() {
        let app = test_router();

        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/documents/parse-batch")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "files": [
                        { "filename": "README.md", "content": "# Hello\nWorld" },
                        { "filename": "app.py", "content": "def greet():\n    return 'hi'" },
                        { "filename": "photo.png", "content": "nope" }
                    ]
                })
                .to_string(),
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let batch: ParseBatchResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(batch.parsed.len(), 2);
        assert_eq!(batch.failed.len(), 1);
        assert_eq!(batch.failed[0].filename, "photo.png");
    }

    #[tokio::test]
    async fn test_chunk_document_endpoint_and_validation() {
        let app = test_router();

        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/documents/chunk")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "filename": "docs/rag.md",
                    "content": "# RAG Engine\n\n## Chunking\nSemantic chunker splits cleanly by headings.\n\n## Retrieval\nHybrid search fuses dense vectors with BM25.",
                    "max_tokens": 96,
                    "overlap_tokens": 16,
                    "min_chunk_tokens": 12,
                    "encoding": "o200k_base",
                    "include_context_headers": true
                })
                .to_string(),
            ))
            .unwrap();

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let res: ChunkDocumentResponse = serde_json::from_slice(&body).unwrap();
        assert!(res.total_chunks >= 2);
        assert_eq!(res.chunks.len(), res.total_chunks);
        assert!(res.total_tokens > 0);
        for chunk in &res.chunks {
            assert!(chunk.token_count <= 96);
        }

        // Invalid overlap >= max_tokens returns 400
        let bad_req = Request::builder()
            .method(Method::POST)
            .uri("/api/documents/chunk")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "filename": "docs/rag.md",
                    "content": "# Test\nBody",
                    "max_tokens": 64,
                    "overlap_tokens": 64
                })
                .to_string(),
            ))
            .unwrap();
        let bad_resp = app.oneshot(bad_req).await.unwrap();
        assert_eq!(bad_resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_ingest_document_validation_and_dimension_guard() {
        let db = ai_db::connect_lazy("postgres://test:test@localhost:5432/test").unwrap();
        let app = build_app(AppState::new(test_config(), db.clone()));

        for invalid in [
            serde_json::json!({ "filename": "  ", "content": "# Hi" }),
            serde_json::json!({ "filename": "a.md", "content": "   " }),
            serde_json::json!({ "filename": "a.md", "content": "# Hi", "max_tokens": 64, "overlap_tokens": 64 }),
        ] {
            let req = Request::builder()
                .method(Method::POST)
                .uri("/api/documents/ingest")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(invalid.to_string()))
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        }

        // Mismatched provider dimension (768 vs 1536) fails with 400 before DB.
        let wrong_dim_app = build_app(
            AppState::new(test_config(), db)
                .with_embedding_provider(Arc::new(MockEmbeddingProvider::new(768))),
        );
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/documents/ingest")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "filename": "docs/test.md",
                    "content": "# Heading\nBody text for ingestion."
                })
                .to_string(),
            ))
            .unwrap();
        let resp = wrong_dim_app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Day 51 Offline End-to-End Integration Test:
    /// Verifies Parse -> Unified DocumentChunker -> Rate-Limited BatchEmbeddingPipeline
    /// -> pgvector Literal Formatting -> Cosine Similarity Top-K Retrieval without external DB.
    #[tokio::test]
    async fn test_end_to_end_parse_chunk_embed_and_cosine_recall_offline() {
        let architecture_md = r#"# NebulaDB Storage Engine

## Write-Ahead Logging
Every mutation is appended sequentially to an fsynced WAL segment with CRC32C checksums before acknowledging the client.

## HNSW Vector Index
Dense 1536-dimensional embeddings are indexed via Hierarchical Navigable Small World graphs with parameters m=16 and ef_construction=64 for sub-10ms cosine search.

## Billing & Quotas
Enterprise tenants receive 500 GB storage quotas and invoice settlement on the first business day of each month.
"#;

        let retriever_rs = r#"/// Compute Reciprocal Rank Fusion score for a ranked candidate.
pub fn reciprocal_rank_fusion(rank: usize, k: f32) -> f32 {
    1.0 / (k + rank as f32)
}

/// Verify Blake3 content digest before skipping redundant embedding work.
pub fn is_unchanged_fingerprint(previous_hex: &str, current_hex: &str) -> bool {
    previous_hex == current_hex
}
"#;

        let chunker = DocumentChunker::new(DocumentChunkerConfig {
            max_tokens: 96,
            overlap_tokens: 16,
            min_chunk_tokens: 8,
            encoding: TokenizerEncoding::Cl100kBase,
            include_context_headers: true,
        });

        let mut all_chunks = chunker
            .chunk_file("docs/nebula.md", "docs/nebula.md", architecture_md)
            .unwrap();
        all_chunks.extend(
            chunker
                .chunk_file(
                    "crates/engine/src/rrf.rs",
                    "crates/engine/src/rrf.rs",
                    retriever_rs,
                )
                .unwrap(),
        );
        assert!(all_chunks.len() >= 4);

        let provider: Arc<dyn EmbeddingProvider> =
            Arc::new(MockEmbeddingProvider::new(EMBEDDING_DIMENSIONS));
        let pipeline = BatchEmbeddingPipeline::new(
            Arc::clone(&provider),
            BatchEmbeddingConfig::default()
                .with_batch_size(2)
                .with_max_concurrency(2)
                .with_expected_dimensions(EMBEDDING_DIMENSIONS),
        );

        let (embedded_chunks, stats) = pipeline
            .embed_items(all_chunks, |c| c.content.as_str())
            .await
            .unwrap();
        assert_eq!(stats.total_items, embedded_chunks.len());
        assert!(stats.total_batches >= 2);

        // Verify every produced vector formats cleanly for PostgreSQL pgvector(1536).
        for (idx, (_, vec)) in embedded_chunks.iter().enumerate() {
            let literal = ai_db::format_pgvector(vec, EMBEDDING_DIMENSIONS, idx).unwrap();
            assert!(literal.starts_with('[') && literal.ends_with(']'));
        }

        // Query 1: Markdown technical fact about HNSW parameters.
        let q1 = provider
            .embed_one("HNSW graph parameters ef_construction cosine vector search")
            .await
            .unwrap();
        let mut ranked_q1: Vec<(f64, &DocumentChunk)> = embedded_chunks
            .iter()
            .map(|(chunk, vec)| (ai_db::cosine_similarity(&q1, vec).unwrap(), chunk))
            .collect();
        ranked_q1.sort_by(|a, b| b.0.total_cmp(&a.0));
        assert!(
            ranked_q1[0].1.content.contains("ef_construction=64"),
            "Top-1 hit should pinpoint HNSW parameters, got: {}",
            ranked_q1[0].1.content
        );
        assert_eq!(ranked_q1[0].1.metadata.file_path, "docs/nebula.md");

        // Query 2: Rust AST symbol fact about Reciprocal Rank Fusion.
        let q2 = provider
            .embed_one("reciprocal_rank_fusion ranked candidate formula")
            .await
            .unwrap();
        let mut ranked_q2: Vec<(f64, &DocumentChunk)> = embedded_chunks
            .iter()
            .map(|(chunk, vec)| (ai_db::cosine_similarity(&q2, vec).unwrap(), chunk))
            .collect();
        ranked_q2.sort_by(|a, b| b.0.total_cmp(&a.0));
        assert!(
            ranked_q2[0].1.content.contains("reciprocal_rank_fusion"),
            "Top-1 hit should pinpoint Rust RRF function, got: {}",
            ranked_q2[0].1.content
        );
        assert_eq!(
            ranked_q2[0].1.metadata.file_path,
            "crates/engine/src/rrf.rs"
        );
    }

    /// Day 51 Full-Stack HTTP + PostgreSQL pgvector End-to-End Integration Test:
    /// POST /api/documents/ingest (Markdown + Rust) -> pgvector HNSW storage -> POST /api/search/vector.
    #[sqlx::test(migrations = "../../migrations")]
    async fn test_end_to_end_http_ingest_embed_and_vector_search_workflow(pool: ai_db::PgPool) {
        let app = build_app(
            AppState::new(test_config(), pool.clone()).with_embedding_provider(Arc::new(
                MockEmbeddingProvider::new(EMBEDDING_DIMENSIONS),
            )),
        );

        let markdown_doc = r#"# Rust AI SaaS Architecture

## pgvector Cosine Retrieval
PostgreSQL stores 1536-dimensional embeddings with an HNSW index (m=16, ef_construction=64) and answers Top-K cosine similarity queries via the <=> operator.

## Token Budget Control
SemanticChunker uses tiktoken-rs cl100k_base BPE counting so every chunk remains strictly bounded within max_tokens.

## Deployment Notes
Docker multi-stage builds ship a slim Axum binary alongside Next.js standalone output.
"#;

        let rust_doc = r#"/// Exponential backoff calculator honoring Retry-After headers.
pub fn compute_backoff_ms(attempt: u32, base_ms: u64) -> u64 {
    base_ms.saturating_mul(1_u64 << attempt.min(10))
}

/// Normalize L2 vector magnitude in place.
pub fn normalize_unit_vector(values: &mut [f32]) {
    let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for v in values.iter_mut() {
            *v /= norm;
        }
    }
}
"#;

        // 1. Ingest Markdown document via HTTP.
        let ingest_md = Request::builder()
            .method(Method::POST)
            .uri("/api/documents/ingest")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "filename": "docs/architecture.md",
                    "content": markdown_doc,
                    "max_tokens": 96,
                    "overlap_tokens": 16,
                    "min_chunk_tokens": 8
                })
                .to_string(),
            ))
            .unwrap();
        let md_resp = app.clone().oneshot(ingest_md).await.unwrap();
        assert_eq!(md_resp.status(), StatusCode::OK);
        let md_body = md_resp.into_body().collect().await.unwrap().to_bytes();
        let md_ingested: IngestDocumentResponse = serde_json::from_slice(&md_body).unwrap();
        assert_eq!(md_ingested.document_id, "docs/architecture.md");
        assert!(md_ingested.upserted_rows >= 3);

        // 2. Re-ingest identical Markdown document to verify idempotent ON CONFLICT DO UPDATE.
        let reingest_md = Request::builder()
            .method(Method::POST)
            .uri("/api/documents/ingest")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "filename": "docs/architecture.md",
                    "content": markdown_doc,
                    "max_tokens": 96,
                    "overlap_tokens": 16,
                    "min_chunk_tokens": 8
                })
                .to_string(),
            ))
            .unwrap();
        let reingest_resp = app.clone().oneshot(reingest_md).await.unwrap();
        assert_eq!(reingest_resp.status(), StatusCode::OK);
        let stored_md = ChunkRepository::list_chunks_by_document(&pool, "docs/architecture.md")
            .await
            .unwrap();
        assert_eq!(stored_md.len(), md_ingested.upserted_rows);

        // 3. Ingest Rust source file via HTTP.
        let ingest_rs = Request::builder()
            .method(Method::POST)
            .uri("/api/documents/ingest")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "filename": "crates/core/src/backoff.rs",
                    "content": rust_doc,
                    "max_tokens": 96,
                    "overlap_tokens": 16,
                    "min_chunk_tokens": 8
                })
                .to_string(),
            ))
            .unwrap();
        let rs_resp = app.clone().oneshot(ingest_rs).await.unwrap();
        assert_eq!(rs_resp.status(), StatusCode::OK);
        let rs_body = rs_resp.into_body().collect().await.unwrap().to_bytes();
        let rs_ingested: IngestDocumentResponse = serde_json::from_slice(&rs_body).unwrap();
        assert!(rs_ingested.upserted_rows >= 2);

        // 4. Semantic vector query targeting Markdown fact (pgvector HNSW parameters).
        let search_pgvector = Request::builder()
            .method(Method::POST)
            .uri("/api/search/vector")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "query": "pgvector HNSW cosine similarity index ef_construction",
                    "top_k": 3,
                    "min_similarity": 0.1
                })
                .to_string(),
            ))
            .unwrap();
        let resp_pg = app.clone().oneshot(search_pgvector).await.unwrap();
        assert_eq!(resp_pg.status(), StatusCode::OK);
        let body_pg = resp_pg.into_body().collect().await.unwrap().to_bytes();
        let hits_pg: VectorSearchResponse = serde_json::from_slice(&body_pg).unwrap();
        assert!(hits_pg.total_hits >= 1);
        assert_eq!(hits_pg.hits[0].document_id, "docs/architecture.md");
        assert!(hits_pg.hits[0].content.contains("ef_construction=64"));
        assert_eq!(
            hits_pg.hits[0].metadata["file_path"],
            "docs/architecture.md"
        );

        // 5. Semantic vector query targeting Rust AST symbol (exponential backoff).
        let search_backoff = Request::builder()
            .method(Method::POST)
            .uri("/api/search/vector")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "query": "exponential backoff Retry-After compute_backoff_ms",
                    "top_k": 3,
                    "min_similarity": 0.1,
                    "document_id": "crates/core/src/backoff.rs"
                })
                .to_string(),
            ))
            .unwrap();
        let resp_rs = app.oneshot(search_backoff).await.unwrap();
        assert_eq!(resp_rs.status(), StatusCode::OK);
        let body_rs = resp_rs.into_body().collect().await.unwrap().to_bytes();
        let hits_rs: VectorSearchResponse = serde_json::from_slice(&body_rs).unwrap();
        assert!(hits_rs.total_hits >= 1);
        assert_eq!(hits_rs.hits[0].document_id, "crates/core/src/backoff.rs");
        assert!(hits_rs.hits[0].content.contains("compute_backoff_ms"));
        assert_eq!(hits_rs.hits[0].metadata["language"], "rust");
    }
}
