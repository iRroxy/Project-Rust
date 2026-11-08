use ai_core::LlmClient;
use std::{env, sync::Arc};

#[derive(Debug)]
#[allow(dead_code)]
pub struct AppConfig {
    pub app_env: String,
    pub port: u16,
    pub llm_api_base: String,
    pub llm_api_key: String,
    pub llm_model: String,
}

impl AppConfig {
    pub fn from_env() -> Self {
        let app_env = env::var("APP_ENV").unwrap_or_else(|_| "development".to_string());
        let port = env::var("PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(3000);
        let llm_api_base =
            env::var("LLM_API_BASE").unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let llm_api_key = env::var("LLM_API_KEY").unwrap_or_default();
        let llm_model = env::var("LLM_MODEL").unwrap_or_else(|_| "deepseek-chat".to_string());

        Self {
            app_env,
            port,
            llm_api_base,
            llm_api_key,
            llm_model,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub llm: LlmClient,
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        let llm = LlmClient::new(
            config.llm_api_base.clone(),
            config.llm_api_key.clone(),
            config.llm_model.clone(),
        );
        Self {
            config: Arc::new(config),
            llm,
        }
    }
}
