use crate::provider::BoxFuture;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, warn};

/// Default OpenAI API base URL for embedding requests.
pub const DEFAULT_OPENAI_EMBEDDING_BASE: &str = "https://api.openai.com/v1";
/// Default OpenAI embedding model (`text-embedding-3-small`, 1536 dimensions).
pub const DEFAULT_OPENAI_EMBEDDING_MODEL: &str = "text-embedding-3-small";
/// Default output dimension for `text-embedding-3-small` (matches `document_chunks.embedding vector(1536)`).
pub const DEFAULT_OPENAI_EMBEDDING_DIMENSIONS: usize = 1536;

/// Default local Ollama server URL.
pub const DEFAULT_OLLAMA_BASE: &str = "http://127.0.0.1:11434";
/// Default local Ollama embedding model (`nomic-embed-text`, 768 dimensions).
pub const DEFAULT_OLLAMA_EMBEDDING_MODEL: &str = "nomic-embed-text";
/// Default output dimension for `nomic-embed-text`.
pub const DEFAULT_OLLAMA_EMBEDDING_DIMENSIONS: usize = 768;

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_ERROR_BODY_CHARS: usize = 512;

/// Structured errors returned by [`EmbeddingProvider`] implementations.
#[derive(Debug, Error)]
pub enum EmbeddingError {
    #[error("embedding API key is missing")]
    MissingApiKey,

    #[error("invalid embedding input: {0}")]
    InvalidInput(String),

    #[error(
        "rate limited by embedding provider (retry_after_secs={retry_after_secs:?}): {message}"
    )]
    RateLimited {
        retry_after_secs: Option<u64>,
        message: String,
    },

    #[error("embedding provider returned HTTP {status}: {message}")]
    Upstream { status: u16, message: String },

    #[error("network error calling embedding provider: {0}")]
    Network(String),

    #[error("invalid embedding response: {0}")]
    InvalidResponse(String),

    #[error("embedding batch count mismatch: expected {expected}, got {actual}")]
    CountMismatch { expected: usize, actual: usize },

    #[error("embedding dimension mismatch at index {index}: expected {expected}, got {actual}")]
    DimensionMismatch {
        index: usize,
        expected: usize,
        actual: usize,
    },

    #[error("embedding vector at index {index} contains non-finite float values (NaN/Inf)")]
    NonFiniteValue { index: usize },
}

impl EmbeddingError {
    /// Returns `true` if this error is transient and safe to retry with exponential backoff.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } | Self::Network(_) => true,
            Self::Upstream { status, .. } => matches!(*status, 408 | 429 | 500..=599),
            Self::MissingApiKey
            | Self::InvalidInput(_)
            | Self::InvalidResponse(_)
            | Self::CountMismatch { .. }
            | Self::DimensionMismatch { .. }
            | Self::NonFiniteValue { .. } => false,
        }
    }

    /// Optional server-suggested retry delay in seconds (e.g. from `Retry-After` header).
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::RateLimited {
                retry_after_secs, ..
            } => *retry_after_secs,
            _ => None,
        }
    }
}

/// Object-safe asynchronous abstraction for text embedding backends.
///
/// Designed to support both cloud APIs (OpenAI / OpenAI-compatible gateways)
/// and local runtimes (Ollama `nomic-embed-text`, etc.) behind `Arc<dyn EmbeddingProvider>`.
pub trait EmbeddingProvider: Send + Sync {
    /// Human-readable provider identifier (e.g. `"openai"`, `"ollama"`, `"mock"`).
    fn provider_name(&self) -> &str;

    /// Embedding model identifier (e.g. `"text-embedding-3-small"`, `"nomic-embed-text"`).
    fn model_name(&self) -> &str;

    /// Expected vector dimensionality produced by this provider.
    ///
    /// Callers writing to `pgvector` columns (such as `vector(1536)`) can inspect this
    /// upfront to guarantee schema compatibility before issuing batch requests.
    fn dimensions(&self) -> usize;

    /// Embed a batch of input texts in order.
    ///
    /// Implementations must preserve input ordering (`result.len() == texts.len()`),
    /// validate vector dimensions against [`Self::dimensions`], and reject non-finite floats.
    fn embed_batch<'a>(
        &'a self,
        texts: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>>;

    /// Convenience helper to embed a single query or document string.
    fn embed_one<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
        Box::pin(async move {
            let input = vec![text.to_owned()];
            let mut vectors = self.embed_batch(&input).await?;
            vectors.pop().ok_or(EmbeddingError::CountMismatch {
                expected: 1,
                actual: 0,
            })
        })
    }
}

fn build_http_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
        .timeout(timeout)
        .build()
        .expect("failed to build reqwest client for embedding provider")
}

fn truncate_error_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "<empty response body>".to_string();
    }
    let mut out: String = trimmed.chars().take(MAX_ERROR_BODY_CHARS).collect();
    if trimmed.chars().count() > MAX_ERROR_BODY_CHARS {
        out.push_str("...");
    }
    out
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
}

fn validate_embeddings(
    embeddings: &[Vec<f32>],
    expected_count: usize,
    expected_dimensions: usize,
) -> Result<(), EmbeddingError> {
    if embeddings.len() != expected_count {
        return Err(EmbeddingError::CountMismatch {
            expected: expected_count,
            actual: embeddings.len(),
        });
    }

    for (index, vec) in embeddings.iter().enumerate() {
        if vec.len() != expected_dimensions {
            return Err(EmbeddingError::DimensionMismatch {
                index,
                expected: expected_dimensions,
                actual: vec.len(),
            });
        }
        if vec.iter().any(|v| !v.is_finite()) {
            return Err(EmbeddingError::NonFiniteValue { index });
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// OpenAI (and OpenAI-compatible) Embedding Provider
// ---------------------------------------------------------------------------

/// Embedding provider for OpenAI (`/v1/embeddings`) and OpenAI-compatible gateways.
#[derive(Clone)]
pub struct OpenAiEmbeddingProvider {
    client: reqwest::Client,
    api_base: String,
    api_key: String,
    model: String,
    dimensions: usize,
    send_dimensions_param: bool,
}

impl fmt::Debug for OpenAiEmbeddingProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiEmbeddingProvider")
            .field("api_base", &self.api_base)
            .field(
                "api_key",
                if self.api_key.is_empty() {
                    &"<empty>"
                } else {
                    &"[REDACTED]"
                },
            )
            .field("model", &self.model)
            .field("dimensions", &self.dimensions)
            .field("send_dimensions_param", &self.send_dimensions_param)
            .finish()
    }
}

#[derive(Debug, Serialize)]
struct OpenAiEmbeddingRequest<'a> {
    model: &'a str,
    input: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<usize>,
    encoding_format: &'static str,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbeddingResponse {
    data: Vec<OpenAiEmbeddingItem>,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbeddingItem {
    index: usize,
    embedding: Vec<f32>,
}

impl OpenAiEmbeddingProvider {
    /// Construct a new OpenAI embedding provider.
    ///
    /// Defaults to `https://api.openai.com/v1`, `text-embedding-3-small`, and `1536` dimensions
    /// when `api_base` or `model` is empty.
    pub fn new(
        api_base: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        dimensions: usize,
    ) -> Self {
        let raw_base = api_base.into();
        let trimmed_base = raw_base.trim().trim_end_matches('/');
        let api_base = if trimmed_base.is_empty() {
            DEFAULT_OPENAI_EMBEDDING_BASE.to_string()
        } else {
            trimmed_base.to_string()
        };

        let raw_model = model.into();
        let trimmed_model = raw_model.trim();
        let model = if trimmed_model.is_empty() {
            DEFAULT_OPENAI_EMBEDDING_MODEL.to_string()
        } else {
            trimmed_model.to_string()
        };

        let dimensions = if dimensions == 0 {
            infer_openai_dimensions(&model)
        } else {
            dimensions
        };

        Self {
            client: build_http_client(DEFAULT_REQUEST_TIMEOUT),
            api_base,
            api_key: api_key.into().trim().to_string(),
            model,
            dimensions,
            send_dimensions_param: false,
        }
    }

    /// Convenience constructor for OpenAI `text-embedding-3-small` (1536 dimensions).
    pub fn text_embedding_3_small(api_key: impl Into<String>) -> Self {
        Self::new(
            DEFAULT_OPENAI_EMBEDDING_BASE,
            api_key,
            DEFAULT_OPENAI_EMBEDDING_MODEL,
            DEFAULT_OPENAI_EMBEDDING_DIMENSIONS,
        )
    }

    /// Include the optional `dimensions` field in the JSON request body (supported by OpenAI v3 models
    /// for Matryoshka dimension reduction, e.g. shortening `text-embedding-3-large` to 1536 dims).
    pub fn with_dimensions_param(mut self, enabled: bool) -> Self {
        self.send_dimensions_param = enabled;
        self
    }

    /// Override the HTTP request timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.client = build_http_client(timeout);
        self
    }
}

fn infer_openai_dimensions(model: &str) -> usize {
    if model.contains("text-embedding-3-large") {
        3072
    } else {
        DEFAULT_OPENAI_EMBEDDING_DIMENSIONS
    }
}

impl EmbeddingProvider for OpenAiEmbeddingProvider {
    fn provider_name(&self) -> &str {
        "openai"
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed_batch<'a>(
        &'a self,
        texts: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
        Box::pin(async move {
            if texts.is_empty() {
                return Ok(Vec::new());
            }

            if self.api_key.is_empty() {
                return Err(EmbeddingError::MissingApiKey);
            }

            let url = format!("{}/embeddings", self.api_base);
            let payload = OpenAiEmbeddingRequest {
                model: &self.model,
                input: texts,
                dimensions: self.send_dimensions_param.then_some(self.dimensions),
                encoding_format: "float",
            };

            debug!(
                provider = "openai",
                model = %self.model,
                batch_size = texts.len(),
                dimensions = self.dimensions,
                "Sending batch embedding request"
            );

            let response = self
                .client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&payload)
                .send()
                .await
                .map_err(|err| EmbeddingError::Network(err.to_string()))?;

            let status = response.status();
            let headers = response.headers().clone();

            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                let message = truncate_error_body(&body);
                if status == StatusCode::TOO_MANY_REQUESTS {
                    return Err(EmbeddingError::RateLimited {
                        retry_after_secs: parse_retry_after(&headers),
                        message,
                    });
                }
                return Err(EmbeddingError::Upstream {
                    status: status.as_u16(),
                    message,
                });
            }

            let parsed: OpenAiEmbeddingResponse = response
                .json()
                .await
                .map_err(|err| EmbeddingError::InvalidResponse(err.to_string()))?;

            if parsed.data.len() != texts.len() {
                return Err(EmbeddingError::CountMismatch {
                    expected: texts.len(),
                    actual: parsed.data.len(),
                });
            }

            // Reorder items strictly by `index` and detect duplicate/out-of-range indices.
            let mut ordered: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
            for item in parsed.data {
                if item.index >= texts.len() {
                    return Err(EmbeddingError::InvalidResponse(format!(
                        "embedding item index {} out of bounds for batch size {}",
                        item.index,
                        texts.len()
                    )));
                }
                if ordered[item.index].is_some() {
                    return Err(EmbeddingError::InvalidResponse(format!(
                        "duplicate embedding index {} in OpenAI response",
                        item.index
                    )));
                }
                ordered[item.index] = Some(item.embedding);
            }

            let embeddings: Vec<Vec<f32>> = ordered
                .into_iter()
                .enumerate()
                .map(|(idx, slot)| {
                    slot.ok_or_else(|| {
                        EmbeddingError::InvalidResponse(format!(
                            "missing embedding for batch index {idx}"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;

            validate_embeddings(&embeddings, texts.len(), self.dimensions)?;
            Ok(embeddings)
        })
    }
}

// ---------------------------------------------------------------------------
// Ollama Local Embedding Provider
// ---------------------------------------------------------------------------

/// Embedding provider for local Ollama runtimes (e.g. `nomic-embed-text`, `mxbai-embed-large`).
///
/// Uses Ollama's batch `POST /api/embed` endpoint and falls back to legacy
/// `POST /api/embeddings` per-item automatically if the Ollama daemon returns `404 Not Found`.
///
/// Note: `nomic-embed-text` produces 768-dimensional vectors by default. If storing into
/// `document_chunks.embedding vector(1536)`, ensure your Ollama model or database schema
/// dimensions match [`Self::dimensions`].
#[derive(Debug, Clone)]
pub struct OllamaEmbeddingProvider {
    client: reqwest::Client,
    api_base: String,
    model: String,
    dimensions: usize,
    truncate: bool,
}

#[derive(Debug, Serialize)]
struct OllamaBatchEmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
    truncate: bool,
}

#[derive(Debug, Deserialize)]
struct OllamaBatchEmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Debug, Serialize)]
struct OllamaLegacyEmbedRequest<'a> {
    model: &'a str,
    prompt: &'a str,
}

#[derive(Debug, Deserialize)]
struct OllamaLegacyEmbedResponse {
    embedding: Vec<f32>,
}

impl OllamaEmbeddingProvider {
    /// Construct a new local Ollama embedding provider.
    ///
    /// Defaults to `http://127.0.0.1:11434`, `nomic-embed-text`, and `768` dimensions
    /// when `api_base`, `model`, or `dimensions` are empty/zero.
    pub fn new(api_base: impl Into<String>, model: impl Into<String>, dimensions: usize) -> Self {
        let raw_base = api_base.into();
        let trimmed_base = raw_base.trim().trim_end_matches('/');
        let api_base = if trimmed_base.is_empty() {
            DEFAULT_OLLAMA_BASE.to_string()
        } else {
            trimmed_base.to_string()
        };

        let raw_model = model.into();
        let trimmed_model = raw_model.trim();
        let model = if trimmed_model.is_empty() {
            DEFAULT_OLLAMA_EMBEDDING_MODEL.to_string()
        } else {
            trimmed_model.to_string()
        };

        let dimensions = if dimensions == 0 {
            infer_ollama_dimensions(&model)
        } else {
            dimensions
        };

        Self {
            client: build_http_client(DEFAULT_REQUEST_TIMEOUT),
            api_base,
            model,
            dimensions,
            truncate: true,
        }
    }

    /// Convenience constructor for local `nomic-embed-text` (768 dimensions) on `127.0.0.1:11434`.
    pub fn nomic_embed_text() -> Self {
        Self::new(
            DEFAULT_OLLAMA_BASE,
            DEFAULT_OLLAMA_EMBEDDING_MODEL,
            DEFAULT_OLLAMA_EMBEDDING_DIMENSIONS,
        )
    }

    /// Control whether Ollama silently truncates inputs exceeding the model context length.
    pub fn with_truncate(mut self, truncate: bool) -> Self {
        self.truncate = truncate;
        self
    }

    /// Override the HTTP request timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.client = build_http_client(timeout);
        self
    }

    async fn embed_via_legacy_endpoint(
        &self,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let legacy_url = format!("{}/api/embeddings", self.api_base);
        let mut results = Vec::with_capacity(texts.len());

        for prompt in texts {
            let payload = OllamaLegacyEmbedRequest {
                model: &self.model,
                prompt,
            };

            let response = self
                .client
                .post(&legacy_url)
                .json(&payload)
                .send()
                .await
                .map_err(|err| EmbeddingError::Network(err.to_string()))?;

            let status = response.status();
            let headers = response.headers().clone();

            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                let message = truncate_error_body(&body);
                if status == StatusCode::TOO_MANY_REQUESTS {
                    return Err(EmbeddingError::RateLimited {
                        retry_after_secs: parse_retry_after(&headers),
                        message,
                    });
                }
                return Err(EmbeddingError::Upstream {
                    status: status.as_u16(),
                    message,
                });
            }

            let parsed: OllamaLegacyEmbedResponse = response
                .json()
                .await
                .map_err(|err| EmbeddingError::InvalidResponse(err.to_string()))?;
            results.push(parsed.embedding);
        }

        validate_embeddings(&results, texts.len(), self.dimensions)?;
        Ok(results)
    }
}

fn infer_ollama_dimensions(model: &str) -> usize {
    if model.contains("mxbai-embed-large") || model.contains("bge-large") {
        1024
    } else if model.contains("all-minilm") {
        384
    } else {
        DEFAULT_OLLAMA_EMBEDDING_DIMENSIONS
    }
}

impl EmbeddingProvider for OllamaEmbeddingProvider {
    fn provider_name(&self) -> &str {
        "ollama"
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed_batch<'a>(
        &'a self,
        texts: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
        Box::pin(async move {
            if texts.is_empty() {
                return Ok(Vec::new());
            }

            let url = format!("{}/api/embed", self.api_base);
            let payload = OllamaBatchEmbedRequest {
                model: &self.model,
                input: texts,
                truncate: self.truncate,
            };

            debug!(
                provider = "ollama",
                model = %self.model,
                batch_size = texts.len(),
                dimensions = self.dimensions,
                "Sending batch embedding request to Ollama"
            );

            let response = self
                .client
                .post(&url)
                .json(&payload)
                .send()
                .await
                .map_err(|err| EmbeddingError::Network(err.to_string()))?;

            let status = response.status();
            let headers = response.headers().clone();

            if status == StatusCode::NOT_FOUND {
                warn!(
                    api_base = %self.api_base,
                    "Ollama /api/embed returned 404; falling back to legacy /api/embeddings"
                );
                return self.embed_via_legacy_endpoint(texts).await;
            }

            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                let message = truncate_error_body(&body);
                if status == StatusCode::TOO_MANY_REQUESTS {
                    return Err(EmbeddingError::RateLimited {
                        retry_after_secs: parse_retry_after(&headers),
                        message,
                    });
                }
                return Err(EmbeddingError::Upstream {
                    status: status.as_u16(),
                    message,
                });
            }

            let parsed: OllamaBatchEmbedResponse = response
                .json()
                .await
                .map_err(|err| EmbeddingError::InvalidResponse(err.to_string()))?;

            validate_embeddings(&parsed.embeddings, texts.len(), self.dimensions)?;
            Ok(parsed.embeddings)
        })
    }
}

// ---------------------------------------------------------------------------
// Deterministic Local Mock Provider (for offline dev / integration tests)
// ---------------------------------------------------------------------------

/// Deterministic, L2-normalized local embedding provider using lexical token hashing.
///
/// Useful for offline unit/integration tests where no OpenAI API key or Ollama daemon
/// is available. Texts sharing words produce higher cosine similarity than unrelated texts.
#[derive(Debug, Clone)]
pub struct MockEmbeddingProvider {
    model: String,
    dimensions: usize,
}

impl MockEmbeddingProvider {
    pub fn new(dimensions: usize) -> Self {
        Self {
            model: "mock-lexical-embed-v1".to_string(),
            dimensions: dimensions.max(1),
        }
    }

    fn embed_text(&self, text: &str) -> Vec<f32> {
        let mut vec = vec![0.0_f32; self.dimensions];
        let lower = text.to_lowercase();

        // Hash whitespace-delimited words and character trigrams into signed buckets.
        let mut had_token = false;
        for token in lower.split(|c: char| !c.is_alphanumeric() && c != '_') {
            if token.is_empty() {
                continue;
            }
            had_token = true;
            let h = fnv1a_64(token.as_bytes());
            let idx = (h as usize) % self.dimensions;
            let sign = if (h >> 63) == 0 { 1.0_f32 } else { -1.0_f32 };
            vec[idx] += sign;
        }

        if !had_token {
            let h = fnv1a_64(text.as_bytes());
            vec[(h as usize) % self.dimensions] = 1.0;
        }

        l2_normalize(&mut vec);
        vec
    }
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// L2-normalize a vector in place so cosine similarity equals dot product.
pub fn l2_normalize(vec: &mut [f32]) {
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for x in vec.iter_mut() {
            *x /= norm;
        }
    }
}

impl EmbeddingProvider for MockEmbeddingProvider {
    fn provider_name(&self) -> &str {
        "mock"
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed_batch<'a>(
        &'a self,
        texts: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
        Box::pin(async move { Ok(texts.iter().map(|t| self.embed_text(t)).collect()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, HeaderValue, StatusCode},
        routing::post,
    };
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    async fn spawn_mock_server(app: Router) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn openai_provider_embeds_batch_and_restores_index_order() {
        type CapturedRequests = Arc<Mutex<Vec<(Option<String>, Value)>>>;
        let captured: CapturedRequests = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::clone(&captured);

        let app = Router::new()
            .route(
                "/embeddings",
                post(
                    |State(log): State<CapturedRequests>,
                     headers: HeaderMap,
                     Json(body): Json<Value>| async move {
                        let auth = headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok())
                            .map(ToOwned::to_owned);
                        log.lock().unwrap().push((auth, body));

                        // Intentionally return items out of order (index 1 before index 0).
                        Json(json!({
                            "object": "list",
                            "data": [
                                { "object": "embedding", "index": 1, "embedding": [0.0, 1.0, 0.0, 0.0] },
                                { "object": "embedding", "index": 0, "embedding": [1.0, 0.0, 0.0, 0.0] }
                            ],
                            "model": "text-embedding-3-small"
                        }))
                    },
                ),
            )
            .with_state(state);

        let base = spawn_mock_server(app).await;
        let provider =
            OpenAiEmbeddingProvider::new(base, "sk-test-secret", "text-embedding-3-small", 4)
                .with_dimensions_param(true);

        // Verify Debug redacts the API key.
        let debug_str = format!("{provider:?}");
        assert!(debug_str.contains("[REDACTED]"));
        assert!(!debug_str.contains("sk-test-secret"));

        let inputs = vec!["rust pgvector".to_string(), "axum tokio".to_string()];
        let embeddings = provider.embed_batch(&inputs).await.unwrap();

        assert_eq!(embeddings.len(), 2);
        assert_eq!(embeddings[0], vec![1.0, 0.0, 0.0, 0.0]);
        assert_eq!(embeddings[1], vec![0.0, 1.0, 0.0, 0.0]);

        let log = captured.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0.as_deref(), Some("Bearer sk-test-secret"));
        assert_eq!(log[0].1["model"], "text-embedding-3-small");
        assert_eq!(log[0].1["dimensions"], 4);
        assert_eq!(log[0].1["encoding_format"], "float");
    }

    #[tokio::test]
    async fn openai_provider_rejects_missing_key_rate_limit_and_dimension_mismatch() {
        let missing_key_provider =
            OpenAiEmbeddingProvider::new("http://127.0.0.1:9", "   ", "text-embedding-3-small", 4);
        let err = missing_key_provider
            .embed_one("hello")
            .await
            .expect_err("empty key must fail");
        assert!(matches!(err, EmbeddingError::MissingApiKey));
        assert!(!err.is_retryable());

        // 429 with Retry-After header
        let rate_limit_app = Router::new().route(
            "/embeddings",
            post(|| async {
                let mut headers = HeaderMap::new();
                headers.insert("retry-after", HeaderValue::from_static("7"));
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    headers,
                    "rate limit exceeded",
                )
            }),
        );
        let base_429 = spawn_mock_server(rate_limit_app).await;
        let provider_429 =
            OpenAiEmbeddingProvider::new(base_429, "sk-key", "text-embedding-3-small", 4);
        let err_429 = provider_429
            .embed_one("hello")
            .await
            .expect_err("429 should return RateLimited");
        assert!(matches!(
            err_429,
            EmbeddingError::RateLimited {
                retry_after_secs: Some(7),
                ..
            }
        ));
        assert!(err_429.is_retryable());
        assert_eq!(err_429.retry_after_secs(), Some(7));

        // Dimension mismatch
        let bad_dim_app = Router::new().route(
            "/embeddings",
            post(|| async {
                Json(json!({
                    "data": [{ "index": 0, "embedding": [0.1, 0.2] }]
                }))
            }),
        );
        let base_dim = spawn_mock_server(bad_dim_app).await;
        let provider_dim =
            OpenAiEmbeddingProvider::new(base_dim, "sk-key", "text-embedding-3-small", 4);
        let err_dim = provider_dim
            .embed_one("hello")
            .await
            .expect_err("dimension mismatch should fail");
        assert!(matches!(
            err_dim,
            EmbeddingError::DimensionMismatch {
                index: 0,
                expected: 4,
                actual: 2
            }
        ));
        assert!(!err_dim.is_retryable());
    }

    #[tokio::test]
    async fn ollama_provider_supports_batch_embed_and_legacy_fallback() {
        // Modern /api/embed batch endpoint
        let modern_app = Router::new().route(
            "/api/embed",
            post(|Json(body): Json<Value>| async move {
                assert_eq!(body["model"], "nomic-embed-text");
                assert_eq!(body["input"].as_array().unwrap().len(), 2);
                Json(json!({
                    "model": "nomic-embed-text",
                    "embeddings": [
                        [0.25, 0.5, 0.75],
                        [0.75, 0.5, 0.25]
                    ]
                }))
            }),
        );
        let modern_base = spawn_mock_server(modern_app).await;
        let modern_provider = OllamaEmbeddingProvider::new(modern_base, "nomic-embed-text", 3);
        assert_eq!(modern_provider.provider_name(), "ollama");
        assert_eq!(modern_provider.dimensions(), 3);

        let batch = modern_provider
            .embed_batch(&["doc a".to_string(), "doc b".to_string()])
            .await
            .unwrap();
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0], vec![0.25, 0.5, 0.75]);
        assert_eq!(batch[1], vec![0.75, 0.5, 0.25]);

        // Legacy Ollama server (no /api/embed, only /api/embeddings)
        let legacy_app = Router::new().route(
            "/api/embeddings",
            post(|Json(body): Json<Value>| async move {
                let prompt = body["prompt"].as_str().unwrap_or("");
                let v = if prompt == "first" { 1.0 } else { 2.0 };
                Json(json!({ "embedding": [v, 0.0, 0.0] }))
            }),
        );
        let legacy_base = spawn_mock_server(legacy_app).await;
        let legacy_provider = OllamaEmbeddingProvider::new(legacy_base, "nomic-embed-text", 3);
        let legacy_res = legacy_provider
            .embed_batch(&["first".to_string(), "second".to_string()])
            .await
            .unwrap();
        assert_eq!(legacy_res, vec![vec![1.0, 0.0, 0.0], vec![2.0, 0.0, 0.0]]);
    }

    #[tokio::test]
    async fn mock_provider_is_object_safe_deterministic_and_semantically_consistent() {
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(MockEmbeddingProvider::new(1536));
        assert_eq!(provider.dimensions(), 1536);

        let empty = provider.embed_batch(&[]).await.unwrap();
        assert!(empty.is_empty());

        let v1 = provider
            .embed_one("Rust pgvector HNSW search")
            .await
            .unwrap();
        let v2 = provider
            .embed_one("Rust pgvector HNSW index")
            .await
            .unwrap();
        let v3 = provider.embed_one("banana smoothie recipe").await.unwrap();

        assert_eq!(v1.len(), 1536);
        let sim_related: f32 = v1.iter().zip(&v2).map(|(a, b)| a * b).sum();
        let sim_unrelated: f32 = v1.iter().zip(&v3).map(|(a, b)| a * b).sum();
        assert!(
            sim_related > sim_unrelated,
            "related={sim_related} should exceed unrelated={sim_unrelated}"
        );
    }
}
