use anyhow::{Context, Result, bail};
use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use reqwest::Client;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
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

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StructuredOutput<T> {
    result: T,
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
    content: String,
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
            response_format: None,
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

    /// Generate a JSON value guided by `T`'s schema, validate it, and decode it into `T`.
    pub async fn generate_structured<T>(&self, prompt: &str) -> Result<T>
    where
        T: JsonSchema + DeserializeOwned,
    {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            bail!("Structured output prompt cannot be empty");
        }
        if self.api_key.is_empty() || self.api_key == "sk-your-key-here" {
            bail!("LLM_API_KEY is required for structured output");
        }

        // An object envelope also supports arrays, scalars, and nullable Rust types.
        let schema = serde_json::to_value(schemars::schema_for!(StructuredOutput<T>))?;
        let validator = jsonschema::options()
            .should_validate_formats(true)
            .build(&schema)
            .map_err(|error| anyhow::anyhow!("Invalid structured output schema: {error}"))?;
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
            .bearer_auth(&self.api_key)
            .json(&ChatCompletionRequest {
                model: &self.model,
                messages: &messages,
                stream: false,
                response_format: Some(ResponseFormat::JsonObject),
            })
            .send()
            .await
            .context("Failed to request structured output")?
            .error_for_status()
            .context("LLM rejected structured output request")?;
        let data: ChatCompletionResponse = response
            .json()
            .await
            .context("Failed to parse structured output response")?;
        let choice = data
            .choices
            .into_iter()
            .next()
            .context("LLM returned no structured output choices")?;
        if choice.message.refusal.is_some() {
            bail!("LLM refused the structured output request");
        }
        if choice.finish_reason.as_deref() != Some("stop") {
            bail!("LLM structured output did not finish normally");
        }
        if choice.message.content.trim().is_empty() {
            bail!("LLM returned empty structured output");
        }

        let value: serde_json::Value = serde_json::from_str(&choice.message.content)
            .context("Invalid structured output JSON")?;
        if let Err(error) = validator.validate(&value) {
            bail!(
                "Structured output violates JSON schema at {}",
                error.instance_path()
            );
        }
        let output: StructuredOutput<T> =
            serde_json::from_value(value).context("Failed to decode structured output type")?;
        Ok(output.result)
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
                response_format: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::TechReport;
    use axum::{Router, body::Bytes, http::StatusCode, routing::post};
    use serde_json::{Value, json};

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
        let mut response = completion(report_value(90));
        response["choices"][0]["message"]["refusal"] = json!("Cannot comply");
        let (client, _provider) = provider(StatusCode::OK, response).await;
        assert!(
            client
                .generate_structured::<TechReport>("Assess Rust")
                .await
                .is_err()
        );
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
}
