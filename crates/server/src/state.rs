use crate::error::{AppError, AppResult};
use ai_core::{
    BatchEmbeddingConfig, BatchEmbeddingPipeline, EmbeddingProvider, LlmClient,
    MockEmbeddingProvider, ModelProvider, ModelRouter, ModelSelection, ModelTier,
    OllamaEmbeddingProvider, OpenAiEmbeddingProvider,
    embedding::{
        DEFAULT_OLLAMA_BASE, DEFAULT_OLLAMA_EMBEDDING_DIMENSIONS, DEFAULT_OLLAMA_EMBEDDING_MODEL,
        DEFAULT_OPENAI_EMBEDDING_BASE, DEFAULT_OPENAI_EMBEDDING_DIMENSIONS,
        DEFAULT_OPENAI_EMBEDDING_MODEL,
    },
};
use ai_db::PgPool;
use serde::Serialize;
use std::{
    env, fmt,
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
};

const DEFAULT_CORS_ORIGINS: [&str; 4] = [
    "http://localhost:3000",
    "http://127.0.0.1:3000",
    "http://localhost:3001",
    "http://127.0.0.1:3001",
];

fn env_non_empty(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_cors_origins(raw: Option<String>) -> Vec<String> {
    let origins: Vec<String> = raw
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .map(|origin| origin.trim().trim_end_matches('/').to_string())
                .filter(|origin| !origin.is_empty())
                .collect::<Vec<_>>()
        })
        .collect();

    if origins.is_empty() {
        DEFAULT_CORS_ORIGINS
            .into_iter()
            .map(str::to_string)
            .collect()
    } else {
        origins
    }
}

/// Configuration for the embedding provider (`openai`, `ollama`, or `mock`).
#[derive(Clone)]
pub struct EmbeddingConfig {
    pub provider: Option<String>,
    pub api_base: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub dimensions: Option<usize>,
    pub batch_size: usize,
    pub max_concurrency: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: None,
            api_base: None,
            api_key: None,
            model: None,
            dimensions: None,
            batch_size: ai_core::embedding_pipeline::DEFAULT_EMBEDDING_BATCH_SIZE,
            max_concurrency: ai_core::embedding_pipeline::DEFAULT_EMBEDDING_MAX_CONCURRENCY,
        }
    }
}

impl fmt::Debug for EmbeddingConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmbeddingConfig")
            .field("provider", &self.provider)
            .field("api_base", &self.api_base)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("model", &self.model)
            .field("dimensions", &self.dimensions)
            .field("batch_size", &self.batch_size)
            .field("max_concurrency", &self.max_concurrency)
            .finish()
    }
}

impl EmbeddingConfig {
    pub fn from_env() -> Self {
        let provider = env_non_empty("EMBEDDING_PROVIDER");
        let api_base = env_non_empty("EMBEDDING_API_BASE");
        let api_key = env_non_empty("EMBEDDING_API_KEY");
        let model = env_non_empty("EMBEDDING_MODEL");
        let dimensions = env_non_empty("EMBEDDING_DIMENSIONS").and_then(|raw| {
            raw.parse::<usize>().ok().filter(|&d| d > 0).or_else(|| {
                tracing::warn!(dimensions = %raw, "Invalid EMBEDDING_DIMENSIONS; using provider default");
                None
            })
        });
        let batch_size = env_non_empty("EMBEDDING_BATCH_SIZE")
            .and_then(|raw| raw.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(ai_core::embedding_pipeline::DEFAULT_EMBEDDING_BATCH_SIZE);
        let max_concurrency = env_non_empty("EMBEDDING_MAX_CONCURRENCY")
            .and_then(|raw| raw.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(ai_core::embedding_pipeline::DEFAULT_EMBEDDING_MAX_CONCURRENCY);

        Self {
            provider,
            api_base,
            api_key,
            model,
            dimensions,
            batch_size,
            max_concurrency,
        }
    }
}

pub struct AppConfig {
    pub app_env: String,
    pub host: IpAddr,
    pub port: u16,
    pub run_migrations: bool,
    pub cors_allow_origins: Vec<String>,
    pub llm_api_base: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub llm_reasoning_model: Option<String>,
    pub llm_reasoning_api_base: Option<String>,
    pub llm_reasoning_api_key: Option<String>,
    pub embedding: EmbeddingConfig,
    pub database_url: String,
}

impl fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppConfig")
            .field("app_env", &self.app_env)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("run_migrations", &self.run_migrations)
            .field("cors_allow_origins", &self.cors_allow_origins)
            .field("llm_api_base", &self.llm_api_base)
            .field("llm_api_key", &"[REDACTED]")
            .field("llm_model", &self.llm_model)
            .field("llm_reasoning_model", &self.llm_reasoning_model)
            .field("llm_reasoning_api_base", &self.llm_reasoning_api_base)
            .field(
                "llm_reasoning_api_key",
                &self.llm_reasoning_api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("embedding", &self.embedding)
            .field("database_url", &"[REDACTED]")
            .finish()
    }
}

impl AppConfig {
    pub fn from_env() -> Self {
        let app_env = env_non_empty("APP_ENV").unwrap_or_else(|| "development".to_string());
        let host = match env_non_empty("HOST") {
            Some(raw) => match raw.parse::<IpAddr>() {
                Ok(addr) => addr,
                Err(_) => {
                    tracing::warn!(host = %raw, "Invalid HOST value, falling back to 127.0.0.1");
                    IpAddr::V4(Ipv4Addr::LOCALHOST)
                }
            },
            None => IpAddr::V4(Ipv4Addr::LOCALHOST),
        };
        let port = match env_non_empty("PORT") {
            Some(raw) => match raw.parse::<u16>() {
                Ok(p) => p,
                Err(_) => {
                    tracing::warn!(port = %raw, "Invalid PORT value, falling back to 3000");
                    3000
                }
            },
            None => 3000,
        };
        let run_migrations = env_non_empty("RUN_MIGRATIONS")
            .map(|v| !matches!(v.to_ascii_lowercase().as_str(), "0" | "false" | "no"))
            .unwrap_or(true);
        let cors_allow_origins = parse_cors_origins(env_non_empty("CORS_ALLOW_ORIGINS"));
        let llm_api_base = env_non_empty("LLM_API_BASE")
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let llm_api_key = env_non_empty("LLM_API_KEY").unwrap_or_default();
        let llm_model = env_non_empty("LLM_MODEL").unwrap_or_else(|| "deepseek-chat".to_string());
        let llm_reasoning_model = env_non_empty("LLM_REASONING_MODEL");
        let llm_reasoning_api_base = env_non_empty("LLM_REASONING_API_BASE");
        let llm_reasoning_api_key = env_non_empty("LLM_REASONING_API_KEY");
        let embedding = EmbeddingConfig::from_env();
        let database_url = env_non_empty("DATABASE_URL").expect("DATABASE_URL must be set");

        Self {
            app_env,
            host,
            port,
            run_migrations,
            cors_allow_origins,
            llm_api_base,
            llm_api_key,
            llm_model,
            llm_reasoning_model,
            llm_reasoning_api_base,
            llm_reasoning_api_key,
            embedding,
            database_url,
        }
    }

    pub fn is_production(&self) -> bool {
        self.app_env.eq_ignore_ascii_case("production")
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ModelTiersInfo {
    pub fast: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ModelsInfo {
    pub default_tier: ModelTier,
    pub tiers: ModelTiersInfo,
    pub models: Vec<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub llm: Arc<dyn ModelProvider>,
    pub embeddings: Arc<dyn EmbeddingProvider>,
    pub embedding_pipeline: BatchEmbeddingPipeline,
    pub db: PgPool,
    models: Arc<ModelsInfo>,
}

fn build_embedding_provider(config: &AppConfig) -> Arc<dyn EmbeddingProvider> {
    let emb = &config.embedding;
    let selected = emb.provider.as_deref().map(str::to_ascii_lowercase);

    match selected.as_deref() {
        Some("ollama") => {
            let base = emb
                .api_base
                .clone()
                .unwrap_or_else(|| DEFAULT_OLLAMA_BASE.to_string());
            let model = emb
                .model
                .clone()
                .unwrap_or_else(|| DEFAULT_OLLAMA_EMBEDDING_MODEL.to_string());
            let dims = emb
                .dimensions
                .unwrap_or(DEFAULT_OLLAMA_EMBEDDING_DIMENSIONS);
            Arc::new(OllamaEmbeddingProvider::new(base, model, dims))
        }
        Some("mock") => {
            let dims = emb.dimensions.unwrap_or(ai_db::EMBEDDING_DIMENSIONS);
            Arc::new(MockEmbeddingProvider::new(dims))
        }
        Some("openai") => build_openai_embedding_provider(emb),
        Some(other) => {
            tracing::warn!(
                provider = %other,
                "Unknown EMBEDDING_PROVIDER; falling back to OpenAI embedding provider"
            );
            build_openai_embedding_provider(emb)
        }
        None => {
            if emb.api_key.is_some() || config.is_production() {
                build_openai_embedding_provider(emb)
            } else {
                let dims = emb.dimensions.unwrap_or(ai_db::EMBEDDING_DIMENSIONS);
                tracing::info!(
                    dimensions = dims,
                    "EMBEDDING_API_KEY not set in non-production mode; using MockEmbeddingProvider"
                );
                Arc::new(MockEmbeddingProvider::new(dims))
            }
        }
    }
}

fn build_openai_embedding_provider(emb: &EmbeddingConfig) -> Arc<dyn EmbeddingProvider> {
    let base = emb
        .api_base
        .clone()
        .unwrap_or_else(|| DEFAULT_OPENAI_EMBEDDING_BASE.to_string());
    let key = emb.api_key.clone().unwrap_or_default();
    let model = emb
        .model
        .clone()
        .unwrap_or_else(|| DEFAULT_OPENAI_EMBEDDING_MODEL.to_string());
    let dims = emb
        .dimensions
        .unwrap_or(DEFAULT_OPENAI_EMBEDDING_DIMENSIONS);
    Arc::new(OpenAiEmbeddingProvider::new(base, key, model, dims))
}

fn build_embedding_pipeline(
    provider: Arc<dyn EmbeddingProvider>,
    emb: &EmbeddingConfig,
) -> BatchEmbeddingPipeline {
    let pipeline_cfg = BatchEmbeddingConfig::default()
        .with_batch_size(emb.batch_size)
        .with_max_concurrency(emb.max_concurrency);
    BatchEmbeddingPipeline::new(provider, pipeline_cfg)
}

impl AppState {
    pub fn new(config: AppConfig, db: PgPool) -> Self {
        let allow_mock = !config.is_production();
        let fast_provider: Arc<dyn ModelProvider> = Arc::new(
            LlmClient::new(
                config.llm_api_base.clone(),
                config.llm_api_key.clone(),
                config.llm_model.clone(),
            )
            .with_mock_fallback(allow_mock),
        );

        let mut router = ModelRouter::new(
            ModelTier::Fast,
            fast_provider.clone(),
            config.llm_model.clone(),
        )
        .expect("LLM_MODEL must not be empty");

        if let Some(reasoning_model) = config.llm_reasoning_model.as_deref() {
            let reasoning_api_base = config
                .llm_reasoning_api_base
                .as_deref()
                .unwrap_or(&config.llm_api_base);
            let same_base = reasoning_api_base.trim_end_matches('/')
                == config.llm_api_base.trim_end_matches('/');
            let same_key = config
                .llm_reasoning_api_key
                .as_deref()
                .is_none_or(|key| key == config.llm_api_key);

            let reasoning_provider: Arc<dyn ModelProvider> = if same_base && same_key {
                fast_provider
            } else {
                let reasoning_key = match &config.llm_reasoning_api_key {
                    Some(key) => key.clone(),
                    None if same_base => config.llm_api_key.clone(),
                    None => {
                        tracing::warn!(
                            "LLM_REASONING_API_BASE differs from LLM_API_BASE without LLM_REASONING_API_KEY"
                        );
                        String::new()
                    }
                };
                Arc::new(
                    LlmClient::new(
                        reasoning_api_base.to_string(),
                        reasoning_key,
                        reasoning_model.to_string(),
                    )
                    .with_mock_fallback(allow_mock),
                )
            };

            router = router
                .with_tier(ModelTier::Reasoning, reasoning_provider, reasoning_model)
                .expect("LLM_REASONING_MODEL must not be empty");
        } else if config.llm_reasoning_api_base.is_some() || config.llm_reasoning_api_key.is_some()
        {
            tracing::warn!(
                "LLM_REASONING_API_BASE or LLM_REASONING_API_KEY is set without LLM_REASONING_MODEL; reasoning tier is disabled"
            );
        }

        let models = ModelsInfo {
            default_tier: router.default_tier(),
            tiers: ModelTiersInfo {
                fast: router
                    .tier_model(ModelTier::Fast)
                    .unwrap_or(&config.llm_model)
                    .to_string(),
                reasoning: router.tier_model(ModelTier::Reasoning).map(str::to_string),
            },
            models: router
                .configured_models()
                .into_iter()
                .map(str::to_string)
                .collect(),
        };

        let embeddings = build_embedding_provider(&config);
        let embedding_pipeline =
            build_embedding_pipeline(Arc::clone(&embeddings), &config.embedding);

        Self {
            config: Arc::new(config),
            llm: Arc::new(router),
            embeddings,
            embedding_pipeline,
            db,
            models: Arc::new(models),
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub fn with_provider(config: AppConfig, db: PgPool, llm: Arc<dyn ModelProvider>) -> Self {
        let mut models = vec![config.llm_model.clone()];
        if let Some(reasoning) = &config.llm_reasoning_model
            && reasoning != &config.llm_model
        {
            models.push(reasoning.clone());
        }
        models.sort_unstable();
        let models_info = ModelsInfo {
            default_tier: ModelTier::Fast,
            tiers: ModelTiersInfo {
                fast: config.llm_model.clone(),
                reasoning: config.llm_reasoning_model.clone(),
            },
            models,
        };
        let embeddings = build_embedding_provider(&config);
        let embedding_pipeline =
            build_embedding_pipeline(Arc::clone(&embeddings), &config.embedding);
        Self {
            config: Arc::new(config),
            llm,
            embeddings,
            embedding_pipeline,
            db,
            models: Arc::new(models_info),
        }
    }

    #[cfg(test)]
    pub fn with_embedding_provider(mut self, embeddings: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedding_pipeline =
            build_embedding_pipeline(Arc::clone(&embeddings), &self.config.embedding);
        self.embeddings = embeddings;
        self
    }

    pub fn models_info(&self) -> &ModelsInfo {
        &self.models
    }

    pub fn resolve_model_selection<'a>(
        &self,
        tier: Option<ModelTier>,
        model: Option<&'a str>,
    ) -> AppResult<ModelSelection<'a>> {
        let selection = ModelSelection::from_request(tier, model)
            .map_err(|err| AppError::BadRequest(err.to_string()))?;
        self.llm
            .resolve_model(selection)
            .map_err(|err| AppError::BadRequest(err.to_string()))?;
        Ok(selection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_core::llm::ChatMessage;
    use axum::{
        Json, Router,
        http::{HeaderMap, header},
        routing::post,
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn cors_origin_parser_defaults_and_trims() {
        assert_eq!(
            parse_cors_origins(None),
            vec![
                "http://localhost:3000",
                "http://127.0.0.1:3000",
                "http://localhost:3001",
                "http://127.0.0.1:3001"
            ]
        );
        assert_eq!(
            parse_cors_origins(Some(
                " http://localhost:3001/ , , https://app.example.com/ ".to_string()
            )),
            vec!["http://localhost:3001", "https://app.example.com"]
        );
    }

    #[test]
    fn app_config_debug_redacts_secrets() {
        let cfg = AppConfig {
            app_env: "production".to_string(),
            host: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: 3000,
            run_migrations: true,
            cors_allow_origins: vec!["https://app.example.com".to_string()],
            llm_api_base: "https://api.deepseek.com/v1".to_string(),
            llm_api_key: "sk-super-secret".to_string(),
            llm_model: "deepseek-chat".to_string(),
            llm_reasoning_model: None,
            llm_reasoning_api_base: None,
            llm_reasoning_api_key: Some("sk-reasoning-secret".to_string()),
            embedding: EmbeddingConfig {
                api_key: Some("sk-embed-secret".to_string()),
                ..EmbeddingConfig::default()
            },
            database_url: "postgres://user:dbpass@localhost:5432/db".to_string(),
        };
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("sk-super-secret"));
        assert!(!rendered.contains("sk-reasoning-secret"));
        assert!(!rendered.contains("sk-embed-secret"));
        assert!(!rendered.contains("dbpass"));
        assert!(rendered.contains("[REDACTED]"));
    }

    #[tokio::test]
    async fn reasoning_provider_does_not_leak_fast_key_to_different_host() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hit = Arc::new(AtomicBool::new(false));
        let hit_flag = hit.clone();
        let foreign_app = Router::new().route(
            "/chat/completions",
            post(move |headers: HeaderMap| {
                let hit_flag = hit_flag.clone();
                async move {
                    hit_flag.store(true, Ordering::SeqCst);
                    assert!(headers.get(header::AUTHORIZATION).is_none());
                    Json(serde_json::json!({
                        "choices": [{"finish_reason": "stop", "message": {"content": "unexpected"}}]
                    }))
                }
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, foreign_app).await.unwrap();
        });

        let db = ai_db::connect_lazy("postgres://test:test@localhost:5432/test").unwrap();
        let state = AppState::new(
            AppConfig {
                app_env: "test".to_string(),
                host: IpAddr::V4(Ipv4Addr::LOCALHOST),
                port: 0,
                run_migrations: false,
                cors_allow_origins: vec!["http://localhost:3000".to_string()],
                llm_api_base: "https://api.deepseek.com/v1".to_string(),
                llm_api_key: "sk-fast-secret".to_string(),
                llm_model: "deepseek-chat".to_string(),
                llm_reasoning_model: Some("o3-mini".to_string()),
                llm_reasoning_api_base: Some(format!("http://{addr}")),
                llm_reasoning_api_key: None,
                embedding: EmbeddingConfig::default(),
                database_url: String::new(),
            },
            db,
        );

        let reply = state
            .llm
            .chat_completion(
                &[ChatMessage {
                    role: "user",
                    content: "hi",
                }],
                ModelSelection::Tier(ModelTier::Reasoning),
            )
            .await
            .unwrap();
        task.abort();
        assert_eq!(reply, "[Mock LLM (o3-mini)] You said: hi");
        assert!(!hit.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn reasoning_provider_reuses_fast_key_on_same_base() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/chat/completions",
            post(
                |headers: HeaderMap, Json(body): Json<serde_json::Value>| async move {
                    let auth = headers
                        .get(header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default();
                    let model = body["model"].as_str().unwrap_or_default();
                    Json(serde_json::json!({
                        "choices": [{
                            "finish_reason": "stop",
                            "message": {"content": format!("{auth}|{model}")}
                        }]
                    }))
                },
            ),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let db = ai_db::connect_lazy("postgres://test:test@localhost:5432/test").unwrap();
        let state = AppState::new(
            AppConfig {
                app_env: "test".to_string(),
                host: IpAddr::V4(Ipv4Addr::LOCALHOST),
                port: 0,
                run_migrations: false,
                cors_allow_origins: vec!["http://localhost:3000".to_string()],
                llm_api_base: format!("http://{addr}/"),
                llm_api_key: "sk-shared-key".to_string(),
                llm_model: "deepseek-chat".to_string(),
                llm_reasoning_model: Some("deepseek-reasoner".to_string()),
                llm_reasoning_api_base: Some(format!("http://{addr}")),
                llm_reasoning_api_key: None,
                embedding: EmbeddingConfig::default(),
                database_url: String::new(),
            },
            db,
        );

        let reply = state
            .llm
            .chat_completion(
                &[ChatMessage {
                    role: "user",
                    content: "hi",
                }],
                ModelSelection::Tier(ModelTier::Reasoning),
            )
            .await
            .unwrap();
        task.abort();
        assert_eq!(reply, "Bearer sk-shared-key|deepseek-reasoner");
    }
}
