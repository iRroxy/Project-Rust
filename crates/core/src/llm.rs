use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone)]
pub struct LlmClient {
    http: Client,
    api_base: String,
    api_key: String,
    model: String,
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessageResponse,
}

#[derive(Deserialize)]
struct ChatMessageResponse {
    content: String,
}

impl LlmClient {
    pub fn new(api_base: String, api_key: String, model: String) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(10)
            .build()
            .expect("Failed to build reqwest client");

        Self {
            http,
            api_base: api_base.trim_end_matches('/').to_string(),
            api_key,
            model,
        }
    }

    pub async fn chat_completion(&self, prompt: &str) -> Result<String> {
        // 如果还没配真实 API Key，返回一个本地 Mock 回复方便随时联调测试
        if self.api_key.is_empty() || self.api_key == "sk-your-key-here" {
            tracing::warn!("LLM_API_KEY not configured, returning mock response");
            return Ok(format!("[Mock LLM ({})] You said: {}", self.model, prompt));
        }

        let url = format!("{}/chat/completions", self.api_base);
        let req_body = ChatCompletionRequest {
            model: &self.model,
            messages: vec![ChatMessage {
                role: "user",
                content: prompt,
            }],
        };

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&req_body)
            .send()
            .await
            .context("Failed to send request to LLM API")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            bail!("LLM API returned error {}: {}", status, err_text);
        }

        let data: ChatCompletionResponse = resp
            .json()
            .await
            .context("Failed to parse LLM response JSON")?;

        let reply = data
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default();

        Ok(reply)
    }
}
