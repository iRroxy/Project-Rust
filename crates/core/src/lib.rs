pub mod llm;
pub mod provider;
pub mod report;

pub use llm::LlmClient;
pub use provider::{
    ModelProvider, ModelProviderExt, ModelRouter, ModelSelection, ModelSelectionError, ModelTier,
    TokenStream,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct SystemInfo {
    pub app_name: &'static str,
    pub version: &'static str,
    pub engine: &'static str,
}

impl SystemInfo {
    pub fn current() -> Self {
        Self {
            app_name: "AI Native SaaS Starter",
            version: env!("CARGO_PKG_VERSION"),
            engine: "Rust + Tokio",
        }
    }
}
