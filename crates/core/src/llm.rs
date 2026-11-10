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
    messages: &'a [ChatMessage<'a>],
}

#[derive(Serialize)]
pub struct ChatMessage<'a> {
    pub role: &'a str,
    pub content: &'a str,
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

    pub async fn chat_completion(&self, messages: &[ChatMessage<'_>]) -> Result<String> {
        if self.api_key.is_empty() || self.api_key == "sk-your-key-here" {
            tracing::warn!("LLM_API_KEY not configured, returning mock response");

            let prompt = messages
                .iter()
                .rev()
                .find(|message| message.role == "user")
                .map(|message| message.content)
                .unwrap_or_default();

            return Ok(format!("[Mock LLM ({})] You said: {}", self.model, prompt));
        }

        let url = format!("{}/chat/completions", self.api_base);
        let req_body = ChatCompletionRequest {
            model: &self.model,
            messages,
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
