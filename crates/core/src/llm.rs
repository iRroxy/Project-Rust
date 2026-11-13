use anyhow::{Context, Result, bail};
use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
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
    stream: bool,
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

#[derive(Deserialize)]
struct ChatStreamResponse {
    choices: Vec<ChatStreamChoice>,
}

#[derive(Deserialize)]
struct ChatStreamChoice {
    delta: ChatStreamDelta,
}

#[derive(Deserialize)]
struct ChatStreamDelta {
    content: Option<String>,
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
            stream: false,
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
    pub async fn chat_stream(
        &self,
        messages: &[ChatMessage<'_>],
    ) -> Result<impl Stream<Item = Result<String>> + Send + 'static + use<>> {
        if self.api_key.is_empty() || self.api_key == "sk-your-key-here" {
            bail!("LLM_API_KEY is required for real token streaming");
        }

        let response = self
            .http
            .post(format!("{}/chat/completions", self.api_base))
            .bearer_auth(&self.api_key)
            .json(&ChatCompletionRequest {
                model: &self.model,
                messages,
                stream: true,
            })
            .send()
            .await
            .context("Failed to start LLM stream")?
            .error_for_status()
            .context("LLM rejected streaming request")?;

        let body = response.bytes_stream();

        Ok(async_stream::try_stream! {
            let events = body.eventsource();
            futures_util::pin_mut!(events);

            let mut done = false;

            while let Some(event) = events.next().await {
                let event = event.context("Failed to read LLM SSE event")?;

                if event.data.trim() == "[DONE]" {
                    done = true;
                    break;
                }

                let chunk: ChatStreamResponse =
                    serde_json::from_str(&event.data)
                        .context("Invalid LLM stream JSON")?;

                if let Some(choice) = chunk.choices.into_iter().next()
                    && let Some(content) = choice.delta.content
                    && !content.is_empty()
                {
                    yield content;
                }
            }

            if !done {
                Err(anyhow::anyhow!("LLM stream ended before [DONE]"))?;
            }
        })
    }
}
