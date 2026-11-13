use ai_core::llm::{ChatMessage, LlmClient};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::{
    env,
    io::{self, Write},
};

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    let client = LlmClient::new(
        env::var("LLM_API_BASE").context("LLM_API_BASE is required")?,
        env::var("LLM_API_KEY").context("LLM_API_KEY is required")?,
        env::var("LLM_MODEL").unwrap_or_else(|_| "deepseek-chat".to_string()),
    );

    let messages = [ChatMessage {
        role: "user",
        content: "用中文分三点介绍 Rust 异步流，总共不超过六十字。",
    }];

    let stream = client.chat_stream(&messages).await?;
    futures_util::pin_mut!(stream);

    while let Some(chunk) = stream.next().await {
        print!("{}", chunk?);
        io::stdout().flush()?;
    }

    println!();
    Ok(())
}
