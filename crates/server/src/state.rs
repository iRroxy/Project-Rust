use crate::error::{AppError, AppResult};
use ai_core::{LlmClient, ModelProvider, ModelRouter, ModelSelection, ModelTier};
use ai_db::PgPool;
use serde::Serialize;
use std::{env, sync::Arc};

const DEFAULT_CORS_ORIGINS: [&str; 2] = ["http://localhost:3000", "http://127.0.0.1:3000"];

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

#[derive(Debug)]
pub struct AppConfig {
    pub app_env: String,
    pub port: u16,
    pub cors_allow_origins: Vec<String>,
    pub llm_api_base: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub llm_reasoning_model: Option<String>,
    pub llm_reasoning_api_base: Option<String>,
    pub llm_reasoning_api_key: Option<String>,
    pub database_url: String,
}

impl AppConfig {
    pub fn from_env() -> Self {
        let app_env = env::var("APP_ENV").unwrap_or_else(|_| "development".to_string());
        let port = env::var("PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(3000);
        let cors_allow_origins = parse_cors_origins(env_non_empty("CORS_ALLOW_ORIGINS"));
        let llm_api_base = env_non_empty("LLM_API_BASE")
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let llm_api_key = env::var("LLM_API_KEY").unwrap_or_default();
        let llm_model = env_non_empty("LLM_MODEL").unwrap_or_else(|| "deepseek-chat".to_string());
        let llm_reasoning_model = env_non_empty("LLM_REASONING_MODEL");
        let llm_reasoning_api_base = env_non_empty("LLM_REASONING_API_BASE");
        let llm_reasoning_api_key = env_non_empty("LLM_REASONING_API_KEY");
        let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");

        Self {
            app_env,
            port,
            cors_allow_origins,
            llm_api_base,
            llm_api_key,
            llm_model,
            llm_reasoning_model,
            llm_reasoning_api_base,
            llm_reasoning_api_key,
            database_url,
        }
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
    pub db: PgPool,
    models: Arc<ModelsInfo>,
}

impl AppState {
    pub fn new(config: AppConfig, db: PgPool) -> Self {
        let fast_provider: Arc<dyn ModelProvider> = Arc::new(LlmClient::new(
            config.llm_api_base.clone(),
            config.llm_api_key.clone(),
            config.llm_model.clone(),
        ));

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
                    None => String::new(),
                };
                Arc::new(LlmClient::new(
                    reasoning_api_base.to_string(),
                    reasoning_key,
                    reasoning_model.to_string(),
                ))
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

        Self {
            config: Arc::new(config),
            llm: Arc::new(router),
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
        Self {
            config: Arc::new(config),
            llm,
            db,
            models: Arc::new(models_info),
        }
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
            vec!["http://localhost:3000", "http://127.0.0.1:3000"]
        );
        assert_eq!(
            parse_cors_origins(Some(
                " http://localhost:3001/ , , https://app.example.com/ ".to_string()
            )),
            vec!["http://localhost:3001", "https://app.example.com"]
        );
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
                port: 0,
                cors_allow_origins: vec!["http://localhost:3000".to_string()],
                llm_api_base: "https://api.deepseek.com/v1".to_string(),
                llm_api_key: "sk-fast-secret".to_string(),
                llm_model: "deepseek-chat".to_string(),
                llm_reasoning_model: Some("o3-mini".to_string()),
                llm_reasoning_api_base: Some(format!("http://{addr}")),
                llm_reasoning_api_key: None,
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
                port: 0,
                cors_allow_origins: vec!["http://localhost:3000".to_string()],
                llm_api_base: format!("http://{addr}/"),
                llm_api_key: "sk-shared-key".to_string(),
                llm_model: "deepseek-chat".to_string(),
                llm_reasoning_model: Some("deepseek-reasoner".to_string()),
                llm_reasoning_api_base: Some(format!("http://{addr}")),
                llm_reasoning_api_key: None,
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
