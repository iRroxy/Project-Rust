use std::{env, sync::Arc};

#[derive(Debug)]
pub struct AppConfig {
    pub app_env: String,
    pub port: u16,
    pub llm_api_base: String,
    pub llm_api_key: String,
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

        Self {
            app_env,
            port,
            llm_api_base,
            llm_api_key,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}
