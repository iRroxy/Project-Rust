use crate::provider::{BoxFuture, ModelProvider, ModelSelection, ModelSelectionError, TokenStream};
use anyhow::{Context, Result, bail};
use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use reqwest::Client;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::time::Duration;

const NON_STREAM_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_ERROR_BODY_BYTES: usize = 1024;

#[derive(Clone)]
pub struct LlmClient {
    http: Client,
    api_base: String,
    api_key: String,
    model: String,
    allow_mock: bool,
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage<'a>],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

#[derive(Serialize)]
pub struct ChatMessage<'a> {
    pub role: &'a str,
    pub content: &'a str,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseFormat {
    JsonObject,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    finish_reason: Option<String>,
    message: ChatMessageResponse,
}

#[derive(Deserialize)]
struct ChatMessageResponse {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    refusal: Option<String>,
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

fn is_placeholder_key(api_key: &str) -> bool {
    api_key.is_empty() || api_key == "sk-your-key-here"
}

fn truncate_error_body(body: &str) -> &str {
    if body.len() <= MAX_ERROR_BODY_BYTES {
        return body;
    }
    let mut end = MAX_ERROR_BODY_BYTES;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    &body[..end]
}

impl LlmClient {
    pub fn new(api_base: String, api_key: String, model: String) -> Self {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(10)
            .build()
            .expect("Failed to build reqwest client");

        Self {
            http,
            api_base: api_base.trim().trim_end_matches('/').to_string(),
            api_key: api_key.trim().to_string(),
            model: model.trim().to_string(),
            allow_mock: true,
        }
    }

    /// Control whether non-streaming `chat_completion` may fall back to a mock reply
    /// when `LLM_API_KEY` is missing or placeholder (should be `false` in production).
    pub fn with_mock_fallback(mut self, allow_mock: bool) -> Self {
        self.allow_mock = allow_mock;
        self
    }

    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub async fn chat_completion(&self, messages: &[ChatMessage<'_>]) -> Result<String> {
        let model = self.resolve_model(ModelSelection::Default)?;
        self.chat_completion_with_model(messages, &model).await
    }

    async fn chat_completion_with_model(
        &self,
        messages: &[ChatMessage<'_>],
        model: &str,
    ) -> Result<String> {
        if is_placeholder_key(&self.api_key) {
            if !self.allow_mock {
                bail!("LLM_API_KEY is required in production mode");
            }
            tracing::warn!("LLM_API_KEY not configured, returning mock response");

            let prompt = messages
                .iter()
                .rev()
                .find(|message| message.role == "user")
                .map(|message| message.content)
                .unwrap_or_default();

            return Ok(format!("[Mock LLM ({model})] You said: {prompt}"));
        }

        let url = format!("{}/chat/completions", self.api_base);
        let req_body = ChatCompletionRequest {
            model,
            messages,
            stream: false,
            response_format: None,
        };

        let resp = self
            .http
            .post(&url)
            .timeout(NON_STREAM_TIMEOUT)
            .bearer_auth(&self.api_key)
            .json(&req_body)
            .send()
            .await
            .context("Failed to send request to LLM API")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            bail!(
                "LLM API returned error {}: {}",
                status,
                truncate_error_body(&err_text)
            );
        }

        let data: ChatCompletionResponse = resp
            .json()
            .await
            .context("Failed to parse LLM response JSON")?;

        let choice = data
            .choices
            .into_iter()
            .next()
            .context("LLM returned no completion choices")?;

        if let Some(refusal) = choice.message.refusal {
            bail!("LLM refused the request: {refusal}");
        }

        let reply = choice
            .message
            .content
            .context("LLM returned null completion content")?;

        Ok(reply)
    }

    /// Generate a JSON value guided by `T`'s schema, validate it, and decode it into `T`.
    pub async fn generate_structured<T>(&self, prompt: &str) -> Result<T>
    where
        T: JsonSchema + DeserializeOwned + 'static,
    {
        crate::provider::generate_structured(self, prompt, ModelSelection::Default).await
    }

    async fn structured_completion_with_model(
        &self,
        prompt: &str,
        schema: &serde_json::Value,
        model: &str,
    ) -> Result<serde_json::Value> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            bail!("Structured output prompt cannot be empty");
        }
        if is_placeholder_key(&self.api_key) {
            bail!("LLM_API_KEY is required for structured output");
        }

        let instruction = format!(
            "Return only valid JSON matching this JSON Schema. \
             Put the requested value in the result field. \
             Do not include Markdown or commentary.\nJSON Schema:\n{schema}"
        );
        let messages = [
            ChatMessage {
                role: "system",
                content: &instruction,
            },
            ChatMessage {
                role: "user",
                content: prompt,
            },
        ];
        let response = self
            .http
            .post(format!("{}/chat/completions", self.api_base))
            .timeout(NON_STREAM_TIMEOUT)
            .bearer_auth(&self.api_key)
            .json(&ChatCompletionRequest {
                model,
                messages: &messages,
                stream: false,
                response_format: Some(ResponseFormat::JsonObject),
            })
            .send()
            .await
            .context("Failed to request structured output")?;

        if !response.status().is_success() {
            let status = response.status();
            let err_text = response.text().await.unwrap_or_default();
            bail!(
                "LLM rejected structured output request ({status}): {}",
                truncate_error_body(&err_text)
            );
        }

        let data: ChatCompletionResponse = response
            .json()
            .await
            .context("Failed to parse structured output response")?;
        let choice = data
            .choices
            .into_iter()
            .next()
            .context("LLM returned no structured output choices")?;
        if let Some(refusal) = choice.message.refusal {
            bail!("LLM refused the structured output request: {refusal}");
        }
        if choice.finish_reason.as_deref() != Some("stop") {
            bail!("LLM structured output did not finish normally");
        }
        let content = choice
            .message
            .content
            .context("LLM returned null structured output")?;
        if content.trim().is_empty() {
            bail!("LLM returned empty structured output");
        }

        serde_json::from_str(&content).context("Invalid structured output JSON")
    }

    pub async fn chat_stream(
        &self,
        messages: &[ChatMessage<'_>],
    ) -> Result<impl Stream<Item = Result<String>> + Send + 'static + use<>> {
        let model = self.resolve_model(ModelSelection::Default)?;
        self.chat_stream_with_model(messages, &model).await
    }

    async fn chat_stream_with_model(
        &self,
        messages: &[ChatMessage<'_>],
        model: &str,
    ) -> Result<impl Stream<Item = Result<String>> + Send + 'static + use<>> {
        if is_placeholder_key(&self.api_key) {
            bail!("LLM_API_KEY is required for real token streaming");
        }

        let response = self
            .http
            .post(format!("{}/chat/completions", self.api_base))
            .bearer_auth(&self.api_key)
            .json(&ChatCompletionRequest {
                model,
                messages,
                stream: true,
                response_format: None,
            })
            .send()
            .await
            .context("Failed to start LLM stream")?;

        if !response.status().is_success() {
            let status = response.status();
            let err_text = response.text().await.unwrap_or_default();
            bail!(
                "LLM rejected streaming request ({status}): {}",
                truncate_error_body(&err_text)
            );
        }

        let body = response.bytes_stream();

        Ok(async_stream::try_stream! {
            let events = body.eventsource();
            futures_util::pin_mut!(events);

            let mut done = false;

            while let Some(event) = events.next().await {
                let event = event.context("Failed to read LLM SSE event")?;
                let payload = event.data.trim();

                // Skip empty SSE keep-alive frames
                if payload.is_empty() {
                    continue;
                }

                if payload == "[DONE]" {
                    done = true;
                    break;
                }

                let chunk: ChatStreamResponse =
                    serde_json::from_str(payload)
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

impl ModelProvider for LlmClient {
    fn resolve_model(&self, selection: ModelSelection<'_>) -> Result<String, ModelSelectionError> {
        match selection {
            ModelSelection::Default => {
                let trimmed = self.model.trim();
                if trimmed.is_empty() {
                    Err(ModelSelectionError::EmptyModel)
                } else {
                    Ok(trimmed.to_string())
                }
            }
            ModelSelection::Model(model) => {
                let trimmed = model.trim();
                if trimmed.is_empty() {
                    Err(ModelSelectionError::EmptyModel)
                } else {
                    Ok(trimmed.to_string())
                }
            }
            ModelSelection::Tier(tier) => Err(ModelSelectionError::UnconfiguredTier(tier)),
        }
    }

    fn chat_completion<'a>(
        &'a self,
        messages: &'a [ChatMessage<'a>],
        selection: ModelSelection<'a>,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let model = self.resolve_model(selection)?;
            self.chat_completion_with_model(messages, &model).await
        })
    }

    fn chat_stream<'a>(
        &'a self,
        messages: &'a [ChatMessage<'a>],
        selection: ModelSelection<'a>,
    ) -> BoxFuture<'a, Result<TokenStream>> {
        Box::pin(async move {
            let model = self.resolve_model(selection)?;
            let stream = self.chat_stream_with_model(messages, &model).await?;
            Ok(Box::pin(stream) as TokenStream)
        })
    }

    fn structured_completion<'a>(
        &'a self,
        prompt: &'a str,
        schema: &'a serde_json::Value,
        selection: ModelSelection<'a>,
    ) -> BoxFuture<'a, Result<serde_json::Value>> {
        Box::pin(async move {
            let model = self.resolve_model(selection)?;
            self.structured_completion_with_model(prompt, schema, &model)
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        provider::{ModelProviderExt, ModelTier},
        report::TechReport,
    };
    use axum::{Json, Router, body::Bytes, http::StatusCode, routing::post};
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};

    struct Provider(tokio::task::JoinHandle<()>);

    impl Drop for Provider {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    async fn provider(status: StatusCode, response: Value) -> (LlmClient, Provider) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let body = Bytes::from(serde_json::to_vec(&response).unwrap());
        let app = Router::new().route(
            "/chat/completions",
            post(move || {
                let body = body.clone();
                async move { (status, [("content-type", "application/json")], body) }
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            LlmClient::new(
                format!("http://{address}"),
                "local-test-only".into(),
                "test".into(),
            ),
            Provider(task),
        )
    }

    fn report_value(score: u8) -> Value {
        json!({
            "summary": "Rust assessment",
            "pros": ["memory safety"],
            "cons": ["learning curve"],
            "score": score
        })
    }

    fn completion(value: Value) -> Value {
        json!({"choices": [{
            "finish_reason": "stop",
            "message": {"content": json!({"result": value}).to_string()}
        }]})
    }

    #[tokio::test]
    async fn structured_report_decodes_fields_and_score_boundaries() {
        for score in [0, 100] {
            let (client, _provider) =
                provider(StatusCode::OK, completion(report_value(score))).await;
            let report: TechReport = client.generate_structured("Assess Rust").await.unwrap();
            assert_eq!(report.summary, "Rust assessment");
            assert_eq!(report.pros, ["memory safety"]);
            assert_eq!(report.cons, ["learning curve"]);
            assert_eq!(report.score, score);
        }
    }

    #[tokio::test]
    async fn structured_report_rejects_schema_violations() {
        let mut missing = report_value(90);
        missing.as_object_mut().unwrap().remove("cons");
        let mut wrong_type = report_value(90);
        wrong_type["pros"] = json!("not an array");
        let mut extra = report_value(90);
        extra["unexpected"] = json!(true);
        for value in [report_value(101), missing, wrong_type, extra] {
            let (client, _provider) = provider(StatusCode::OK, completion(value)).await;
            assert!(
                client
                    .generate_structured::<TechReport>("Assess Rust")
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn structured_output_supports_arrays_and_nullable_types() {
        let (client, _provider) =
            provider(StatusCode::OK, completion(json!(["Rust", "Tokio"]))).await;
        let value: Vec<String> = client
            .generate_structured("List technologies")
            .await
            .unwrap();
        assert_eq!(value, ["Rust", "Tokio"]);
        let (client, _provider) = provider(StatusCode::OK, completion(Value::Null)).await;
        let value: Option<TechReport> = client
            .generate_structured("No applicable report")
            .await
            .unwrap();
        assert!(value.is_none());
    }

    #[tokio::test]
    async fn structured_output_rejects_unfinished_or_refused_responses() {
        for reason in [
            json!("length"),
            json!("content_filter"),
            json!("tool_calls"),
            Value::Null,
        ] {
            let mut response = completion(report_value(90));
            response["choices"][0]["finish_reason"] = reason;
            let (client, _provider) = provider(StatusCode::OK, response).await;
            assert!(
                client
                    .generate_structured::<TechReport>("Assess Rust")
                    .await
                    .is_err()
            );
        }
        // OpenAI refusal responses often set `content: null` alongside `refusal`.
        let response = json!({"choices": [{
            "finish_reason": "stop",
            "message": {"content": null, "refusal": "Cannot comply"}
        }]});
        let (client, _provider) = provider(StatusCode::OK, response).await;
        let err = client
            .generate_structured::<TechReport>("Assess Rust")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Cannot comply"));
    }

    #[tokio::test]
    async fn structured_output_rejects_empty_malformed_and_failed_responses() {
        for content in ["", " \n ", "not-json", "{", "```json\n{}\n```", "{}"] {
            let mut response = completion(report_value(90));
            response["choices"][0]["message"]["content"] = json!(content);
            let (client, _provider) = provider(StatusCode::OK, response).await;
            assert!(
                client
                    .generate_structured::<TechReport>("Assess Rust")
                    .await
                    .is_err()
            );
        }
        for (status, response) in [
            (StatusCode::OK, json!({"choices": []})),
            (
                StatusCode::OK,
                json!({"choices": [{"finish_reason": "stop", "message": {"content": null}}]}),
            ),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error": "Unavailable"}),
            ),
        ] {
            let (client, _provider) = provider(status, response).await;
            assert!(
                client
                    .generate_structured::<TechReport>("Assess Rust")
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn production_mode_disables_mock_fallback() {
        let client = LlmClient::new(
            "http://127.0.0.1:9".into(),
            String::new(),
            "deepseek-chat".into(),
        )
        .with_mock_fallback(false);
        let messages = [ChatMessage {
            role: "user",
            content: "hello",
        }];
        assert!(client.chat_completion(&messages).await.is_err());
    }

    #[tokio::test]
    async fn model_provider_forwards_dynamic_model_in_upstream_payloads() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen_models = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded = seen_models.clone();

        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(payload): Json<Value>| {
                let recorded = recorded.clone();
                async move {
                    let model = payload["model"].as_str().unwrap_or_default().to_string();
                    recorded.lock().unwrap().push(model.clone());
                    if payload["stream"].as_bool() == Some(true) {
                        // Include an empty keep-alive data frame before the real chunk
                        let sse = format!(
                            "data: \n\ndata: {{\"choices\":[{{\"delta\":{{\"content\":\"stream:{model}\"}}}}]}}\n\ndata: [DONE]\n\n"
                        );
                        (
                            StatusCode::OK,
                            [("content-type", "text/event-stream")],
                            Bytes::from(sse),
                        )
                    } else if payload.get("response_format").is_some() {
                        let body = completion(report_value(88));
                        (
                            StatusCode::OK,
                            [("content-type", "application/json")],
                            Bytes::from(serde_json::to_vec(&body).unwrap()),
                        )
                    } else {
                        let body = json!({"choices": [{
                            "finish_reason": "stop",
                            "message": {"content": format!("reply:{model}")}
                        }]});
                        (
                            StatusCode::OK,
                            [("content-type", "application/json")],
                            Bytes::from(serde_json::to_vec(&body).unwrap()),
                        )
                    }
                }
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let _guard = Provider(task);

        let provider: Arc<dyn ModelProvider> = Arc::new(LlmClient::new(
            format!("http://{address}"),
            "local-test-only".into(),
            "deepseek-chat".into(),
        ));
        let messages = [ChatMessage {
            role: "user",
            content: "hello",
        }];

        assert_eq!(
            provider
                .chat_completion(&messages, ModelSelection::Default)
                .await
                .unwrap(),
            "reply:deepseek-chat"
        );
        assert_eq!(
            provider
                .chat_completion(&messages, ModelSelection::Model("deepseek-reasoner"))
                .await
                .unwrap(),
            "reply:deepseek-reasoner"
        );

        let mut stream = provider
            .chat_stream(&messages, ModelSelection::Model("deepseek-reasoner"))
            .await
            .unwrap();
        let mut streamed = String::new();
        while let Some(chunk) = stream.next().await {
            streamed.push_str(&chunk.unwrap());
        }
        assert_eq!(streamed, "stream:deepseek-reasoner");

        let report: TechReport = provider
            .generate_structured("Assess Rust", ModelSelection::Model("deepseek-reasoner"))
            .await
            .unwrap();
        assert_eq!(report.score, 88);

        // Bare LlmClient rejects empty model and unconfigured tier before sending HTTP requests.
        assert_eq!(
            provider.resolve_model(ModelSelection::Model("   ")),
            Err(ModelSelectionError::EmptyModel)
        );
        assert_eq!(
            provider.resolve_model(ModelSelection::Tier(ModelTier::Reasoning)),
            Err(ModelSelectionError::UnconfiguredTier(ModelTier::Reasoning))
        );
        assert!(
            provider
                .chat_completion(&messages, ModelSelection::Tier(ModelTier::Reasoning))
                .await
                .is_err()
        );

        assert_eq!(
            seen_models.lock().unwrap().clone(),
            vec![
                "deepseek-chat",
                "deepseek-reasoner",
                "deepseek-reasoner",
                "deepseek-reasoner",
            ]
        );
    }
}
