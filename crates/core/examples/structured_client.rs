use ai_core::{LlmClient, report::TechReport};
use anyhow::{Context, Result};
use std::env;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let client = LlmClient::new(
        env::var("LLM_API_BASE").context("LLM_API_BASE is required")?,
        env::var("LLM_API_KEY").context("LLM_API_KEY is required")?,
        env::var("LLM_MODEL").unwrap_or_else(|_| "deepseek-chat".to_string()),
    );
    let prompt = env::args().nth(1).unwrap_or_else(|| {
        "用中文评估 Rust + Axum 构建 AI 聊天服务：summary 不超过四十字，pros 和 cons 各两条，每条不超过十五字，score 为 0 到 100 的整数。".to_string()
    });
    let report: TechReport = client.generate_structured(&prompt).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
