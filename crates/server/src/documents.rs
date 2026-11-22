use std::{sync::OnceLock, time::Instant};
use tokio::sync::Semaphore;

use ai_parser::{
    DiffCalculator, DocumentChunk, DocumentChunker, DocumentChunkerConfig, DocumentFingerprint,
    ParseError, ParsedDocument, RepositoryScanner, TokenizerEncoding, hash_text,
};
use axum::Json;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

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

    let _permit = parse_semaphore()
        .try_acquire()
        .map_err(|_| AppError::ServiceUnavailable("document parser is busy; please retry shortly".to_string()))?;

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

    let _permit = parse_semaphore()
        .try_acquire()
        .map_err(|_| AppError::ServiceUnavailable("document parser is busy; please retry shortly".to_string()))?;

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
    let _permit = parse_semaphore()
        .try_acquire()
        .map_err(|_| AppError::ServiceUnavailable("document chunker is busy; please retry shortly".to_string()))?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        build_app,
        state::{AppConfig, AppState},
    };
    use axum::{
        body::Body,
        http::{Method, Request, StatusCode, header},
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_router() -> axum::Router {
        let config = AppConfig {
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
            database_url: "postgres://test:test@localhost:5432/test".to_string(),
        };
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
}
