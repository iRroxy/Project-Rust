use crate::llm::ChatMessage;
use anyhow::{Context, Result, bail};
use futures_util::Stream;
use jsonschema::Validator;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    any::TypeId,
    collections::HashMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock, RwLock},
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type TokenStream = Pin<Box<dyn Stream<Item = Result<String>> + Send>>;

type CachedSchema = (Arc<serde_json::Value>, Arc<Validator>);
static STRUCTURED_SCHEMA_CACHE: OnceLock<RwLock<HashMap<TypeId, CachedSchema>>> = OnceLock::new();

fn cached_schema_for<T: JsonSchema + 'static>() -> Result<CachedSchema> {
    let type_id = TypeId::of::<StructuredOutput<T>>();
    let cache = STRUCTURED_SCHEMA_CACHE.get_or_init(|| RwLock::new(HashMap::new()));

    if let Ok(guard) = cache.read()
        && let Some(entry) = guard.get(&type_id)
    {
        return Ok(entry.clone());
    }

    // An object envelope also supports arrays, scalars, and nullable Rust types.
    let schema = Arc::new(serde_json::to_value(schemars::schema_for!(
        StructuredOutput<T>
    ))?);
    let validator = Arc::new(
        jsonschema::options()
            .should_validate_formats(true)
            .build(&schema)
            .map_err(|error| anyhow::anyhow!("Invalid structured output schema: {error}"))?,
    );

    if let Ok(mut guard) = cache.write() {
        guard.insert(type_id, (Arc::clone(&schema), Arc::clone(&validator)));
    }
    Ok((schema, validator))
}

/// Logical model routing tier.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// Fast, cost-effective model for everyday chat.
    #[default]
    Fast,
    /// Stronger flagship or reasoning model for complex tasks.
    Reasoning,
}

impl fmt::Display for ModelTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fast => f.write_str("fast"),
            Self::Reasoning => f.write_str("reasoning"),
        }
    }
}

/// Request-level model selection passed to a [`ModelProvider`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ModelSelection<'a> {
    /// Use the provider or router's default model.
    #[default]
    Default,
    /// Route by logical capability tier.
    Tier(ModelTier),
    /// Route to a specific model identifier.
    Model(&'a str),
}

impl<'a> ModelSelection<'a> {
    /// Build a [`ModelSelection`] from optional API request fields.
    pub fn from_request(
        tier: Option<ModelTier>,
        model: Option<&'a str>,
    ) -> Result<Self, ModelSelectionError> {
        match (tier, model) {
            (Some(_), Some(_)) => Err(ModelSelectionError::ConflictingSelection),
            (Some(tier), None) => Ok(Self::Tier(tier)),
            (None, Some(model)) => {
                let trimmed = model.trim();
                if trimmed.is_empty() {
                    return Err(ModelSelectionError::EmptyModel);
                }
                Ok(Self::Model(trimmed))
            }
            (None, None) => Ok(Self::Default),
        }
    }
}

/// Errors produced while validating or resolving a [`ModelSelection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSelectionError {
    EmptyModel,
    UnknownModel(String),
    UnconfiguredTier(ModelTier),
    ConflictingSelection,
}

impl fmt::Display for ModelSelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyModel => f.write_str("model cannot be empty"),
            Self::UnknownModel(model) => write!(f, "unknown model '{model}'"),
            Self::UnconfiguredTier(tier) => write!(f, "model tier '{tier}' is not configured"),
            Self::ConflictingSelection => f.write_str("specify either 'tier' or 'model', not both"),
        }
    }
}

impl std::error::Error for ModelSelectionError {}

/// Unified, object-safe abstraction for chat, streaming, and structured JSON LLM providers.
pub trait ModelProvider: Send + Sync {
    /// Validate and resolve a [`ModelSelection`] to the concrete model identifier.
    fn resolve_model(&self, selection: ModelSelection<'_>) -> Result<String, ModelSelectionError>;

    /// Perform a non-streaming chat completion.
    fn chat_completion<'a>(
        &'a self,
        messages: &'a [ChatMessage<'a>],
        selection: ModelSelection<'a>,
    ) -> BoxFuture<'a, Result<String>>;

    /// Perform a streaming chat completion, yielding incremental text deltas.
    fn chat_stream<'a>(
        &'a self,
        messages: &'a [ChatMessage<'a>],
        selection: ModelSelection<'a>,
    ) -> BoxFuture<'a, Result<TokenStream>>;

    /// Request a schema-guided JSON value from the backend.
    fn structured_completion<'a>(
        &'a self,
        prompt: &'a str,
        schema: &'a serde_json::Value,
        selection: ModelSelection<'a>,
    ) -> BoxFuture<'a, Result<serde_json::Value>>;
}

impl<P: ModelProvider + ?Sized> ModelProvider for Arc<P> {
    fn resolve_model(&self, selection: ModelSelection<'_>) -> Result<String, ModelSelectionError> {
        (**self).resolve_model(selection)
    }

    fn chat_completion<'a>(
        &'a self,
        messages: &'a [ChatMessage<'a>],
        selection: ModelSelection<'a>,
    ) -> BoxFuture<'a, Result<String>> {
        (**self).chat_completion(messages, selection)
    }

    fn chat_stream<'a>(
        &'a self,
        messages: &'a [ChatMessage<'a>],
        selection: ModelSelection<'a>,
    ) -> BoxFuture<'a, Result<TokenStream>> {
        (**self).chat_stream(messages, selection)
    }

    fn structured_completion<'a>(
        &'a self,
        prompt: &'a str,
        schema: &'a serde_json::Value,
        selection: ModelSelection<'a>,
    ) -> BoxFuture<'a, Result<serde_json::Value>> {
        (**self).structured_completion(prompt, schema, selection)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StructuredOutput<T> {
    result: T,
}

/// Generate a JSON value guided by `T`'s schema via any [`ModelProvider`], validate it locally,
/// and decode it into `T`.
pub async fn generate_structured<P, T>(
    provider: &P,
    prompt: &str,
    selection: ModelSelection<'_>,
) -> Result<T>
where
    P: ModelProvider + ?Sized,
    T: JsonSchema + DeserializeOwned + 'static,
{
    let prompt = prompt.trim();
    if prompt.is_empty() {
        bail!("Structured output prompt cannot be empty");
    }
    provider.resolve_model(selection)?;

    let (schema, validator) = cached_schema_for::<T>()?;

    let value = provider
        .structured_completion(prompt, &schema, selection)
        .await?;
    if let Err(error) = validator.validate(&value) {
        bail!(
            "Structured output violates JSON schema at {}: {error}",
            error.instance_path()
        );
    }
    let output: StructuredOutput<T> =
        serde_json::from_value(value).context("Failed to decode structured output type")?;
    Ok(output.result)
}

/// Extension methods available on all [`ModelProvider`] implementations, including trait objects.
pub trait ModelProviderExt: ModelProvider {
    /// Generate a validated, strongly-typed structured output using the selected model.
    fn generate_structured<'a, T>(
        &'a self,
        prompt: &'a str,
        selection: ModelSelection<'a>,
    ) -> BoxFuture<'a, Result<T>>
    where
        T: JsonSchema + DeserializeOwned + 'static,
    {
        Box::pin(generate_structured(self, prompt, selection))
    }
}

impl<P: ModelProvider + ?Sized> ModelProviderExt for P {}

#[derive(Clone)]
struct ModelRoute {
    provider: Arc<dyn ModelProvider>,
    model: String,
}

impl ModelRoute {
    fn new(
        provider: Arc<dyn ModelProvider>,
        model: impl Into<String>,
    ) -> Result<Self, ModelSelectionError> {
        let model = model.into();
        let trimmed = model.trim();
        if trimmed.is_empty() {
            return Err(ModelSelectionError::EmptyModel);
        }
        Ok(Self {
            provider,
            model: trimmed.to_string(),
        })
    }
}

/// Multi-model router that dispatches requests across one or more [`ModelProvider`] backends
/// by [`ModelTier`] or explicit model name.
#[derive(Clone)]
pub struct ModelRouter {
    default_tier: ModelTier,
    tiers: HashMap<ModelTier, ModelRoute>,
    models: HashMap<String, ModelRoute>,
}

impl ModelRouter {
    /// Create a router with its default tier bound to `(provider, model)`.
    pub fn new(
        default_tier: ModelTier,
        provider: Arc<dyn ModelProvider>,
        model: impl Into<String>,
    ) -> Result<Self, ModelSelectionError> {
        let route = ModelRoute::new(provider, model)?;
        let mut tiers = HashMap::new();
        let mut models = HashMap::new();
        models.insert(route.model.clone(), route.clone());
        tiers.insert(default_tier, route);
        Ok(Self {
            default_tier,
            tiers,
            models,
        })
    }

    /// Register or replace a routing target for `tier`, also updating its model identifier route
    /// so tier and explicit model lookups stay consistent.
    pub fn with_tier(
        mut self,
        tier: ModelTier,
        provider: Arc<dyn ModelProvider>,
        model: impl Into<String>,
    ) -> Result<Self, ModelSelectionError> {
        let route = ModelRoute::new(provider, model)?;
        self.models.insert(route.model.clone(), route.clone());
        self.tiers.insert(tier, route);
        Ok(self)
    }

    /// Register or replace an explicit model identifier route without changing tier bindings.
    pub fn with_model(
        mut self,
        provider: Arc<dyn ModelProvider>,
        model: impl Into<String>,
    ) -> Result<Self, ModelSelectionError> {
        let route = ModelRoute::new(provider, model)?;
        self.models.insert(route.model.clone(), route);
        Ok(self)
    }

    pub fn default_tier(&self) -> ModelTier {
        self.default_tier
    }

    pub fn tier_model(&self, tier: ModelTier) -> Option<&str> {
        self.tiers.get(&tier).map(|route| route.model.as_str())
    }

    pub fn configured_models(&self) -> Vec<&str> {
        let mut models: Vec<&str> = self.models.keys().map(String::as_str).collect();
        models.sort_unstable();
        models
    }

    fn route(&self, selection: ModelSelection<'_>) -> Result<&ModelRoute, ModelSelectionError> {
        match selection {
            ModelSelection::Default => self
                .tiers
                .get(&self.default_tier)
                .ok_or(ModelSelectionError::UnconfiguredTier(self.default_tier)),
            ModelSelection::Tier(tier) => self
                .tiers
                .get(&tier)
                .ok_or(ModelSelectionError::UnconfiguredTier(tier)),
            ModelSelection::Model(name) => {
                let trimmed = name.trim();
                if trimmed.is_empty() {
                    return Err(ModelSelectionError::EmptyModel);
                }
                self.models
                    .get(trimmed)
                    .ok_or_else(|| ModelSelectionError::UnknownModel(trimmed.to_string()))
            }
        }
    }
}

impl ModelProvider for ModelRouter {
    fn resolve_model(&self, selection: ModelSelection<'_>) -> Result<String, ModelSelectionError> {
        let route = self.route(selection)?;
        route
            .provider
            .resolve_model(ModelSelection::Model(&route.model))
    }

    fn chat_completion<'a>(
        &'a self,
        messages: &'a [ChatMessage<'a>],
        selection: ModelSelection<'a>,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let route = self.route(selection)?;
            route
                .provider
                .chat_completion(messages, ModelSelection::Model(&route.model))
                .await
        })
    }

    fn chat_stream<'a>(
        &'a self,
        messages: &'a [ChatMessage<'a>],
        selection: ModelSelection<'a>,
    ) -> BoxFuture<'a, Result<TokenStream>> {
        Box::pin(async move {
            let route = self.route(selection)?;
            route
                .provider
                .chat_stream(messages, ModelSelection::Model(&route.model))
                .await
        })
    }

    fn structured_completion<'a>(
        &'a self,
        prompt: &'a str,
        schema: &'a serde_json::Value,
        selection: ModelSelection<'a>,
    ) -> BoxFuture<'a, Result<serde_json::Value>> {
        Box::pin(async move {
            let route = self.route(selection)?;
            route
                .provider
                .structured_completion(prompt, schema, ModelSelection::Model(&route.model))
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::TechReport;
    use futures_util::{StreamExt, stream};
    use serde_json::json;
    use std::sync::Mutex;

    struct StubProvider {
        name: &'static str,
        default_model: String,
        calls: Mutex<Vec<String>>,
    }

    impl StubProvider {
        fn new(name: &'static str, default_model: &str) -> Arc<Self> {
            Arc::new(Self {
                name,
                default_model: default_model.to_string(),
                calls: Mutex::new(Vec::new()),
            })
        }

        fn recorded_calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl ModelProvider for StubProvider {
        fn resolve_model(
            &self,
            selection: ModelSelection<'_>,
        ) -> Result<String, ModelSelectionError> {
            match selection {
                ModelSelection::Default => Ok(self.default_model.clone()),
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
                self.calls.lock().unwrap().push(format!("chat:{model}"));
                let prompt = messages
                    .last()
                    .map(|message| message.content)
                    .unwrap_or_default();
                Ok(format!("{}:{model}:{prompt}", self.name))
            })
        }

        fn chat_stream<'a>(
            &'a self,
            messages: &'a [ChatMessage<'a>],
            selection: ModelSelection<'a>,
        ) -> BoxFuture<'a, Result<TokenStream>> {
            Box::pin(async move {
                let model = self.resolve_model(selection)?;
                self.calls.lock().unwrap().push(format!("stream:{model}"));
                let prompt = messages
                    .last()
                    .map(|message| message.content)
                    .unwrap_or_default()
                    .to_string();
                let chunks = vec![Ok(format!("{}:{model}:", self.name)), Ok(prompt)];
                Ok(Box::pin(stream::iter(chunks)) as TokenStream)
            })
        }

        fn structured_completion<'a>(
            &'a self,
            prompt: &'a str,
            _schema: &'a serde_json::Value,
            selection: ModelSelection<'a>,
        ) -> BoxFuture<'a, Result<serde_json::Value>> {
            Box::pin(async move {
                let model = self.resolve_model(selection)?;
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("structured:{model}"));
                Ok(json!({
                    "result": {
                        "summary": format!("{}:{model}:{prompt}", self.name),
                        "pros": ["typed"],
                        "cons": ["none"],
                        "score": 95
                    }
                }))
            })
        }
    }

    #[tokio::test]
    async fn router_dispatches_tiers_and_explicit_models_across_providers() {
        let fast = StubProvider::new("fast-backend", "fast-mini");
        let reasoning = StubProvider::new("flagship-backend", "flagship-pro");
        let extra = StubProvider::new("extra-backend", "extra-coder");

        let router = ModelRouter::new(ModelTier::Fast, fast.clone(), "fast-mini")
            .unwrap()
            .with_tier(ModelTier::Reasoning, reasoning.clone(), "flagship-pro")
            .unwrap()
            .with_model(extra.clone(), "extra-coder")
            .unwrap();

        assert_eq!(router.default_tier(), ModelTier::Fast);
        assert_eq!(router.tier_model(ModelTier::Fast), Some("fast-mini"));
        assert_eq!(
            router.tier_model(ModelTier::Reasoning),
            Some("flagship-pro")
        );
        assert_eq!(
            router.configured_models(),
            vec!["extra-coder", "fast-mini", "flagship-pro"]
        );

        let provider: Arc<dyn ModelProvider> = Arc::new(router);
        let messages = [ChatMessage {
            role: "user",
            content: "hello",
        }];

        assert_eq!(
            provider
                .chat_completion(&messages, ModelSelection::Default)
                .await
                .unwrap(),
            "fast-backend:fast-mini:hello"
        );
        assert_eq!(
            provider
                .chat_completion(&messages, ModelSelection::Tier(ModelTier::Reasoning))
                .await
                .unwrap(),
            "flagship-backend:flagship-pro:hello"
        );
        assert_eq!(
            provider
                .chat_completion(&messages, ModelSelection::Model("  extra-coder  "))
                .await
                .unwrap(),
            "extra-backend:extra-coder:hello"
        );

        let mut stream = provider
            .chat_stream(&messages, ModelSelection::Tier(ModelTier::Reasoning))
            .await
            .unwrap();
        let mut collected = String::new();
        while let Some(chunk) = stream.next().await {
            collected.push_str(&chunk.unwrap());
        }
        assert_eq!(collected, "flagship-backend:flagship-pro:hello");

        let report: TechReport = provider
            .generate_structured("Assess Rust", ModelSelection::Model("flagship-pro"))
            .await
            .unwrap();
        assert_eq!(report.summary, "flagship-backend:flagship-pro:Assess Rust");
        assert_eq!(report.score, 95);

        assert_eq!(fast.recorded_calls(), vec!["chat:fast-mini"]);
        assert_eq!(
            reasoning.recorded_calls(),
            vec![
                "chat:flagship-pro",
                "stream:flagship-pro",
                "structured:flagship-pro"
            ]
        );
        assert_eq!(extra.recorded_calls(), vec!["chat:extra-coder"]);
    }

    #[tokio::test]
    async fn router_rejects_invalid_and_unconfigured_selections_without_calling_provider() {
        let fast = StubProvider::new("fast-backend", "fast-mini");
        let router = ModelRouter::new(ModelTier::Fast, fast.clone(), "fast-mini").unwrap();

        assert_eq!(
            router.resolve_model(ModelSelection::Tier(ModelTier::Reasoning)),
            Err(ModelSelectionError::UnconfiguredTier(ModelTier::Reasoning))
        );
        assert_eq!(
            router.resolve_model(ModelSelection::Model("   ")),
            Err(ModelSelectionError::EmptyModel)
        );
        assert_eq!(
            router.resolve_model(ModelSelection::Model("gpt-unknown")),
            Err(ModelSelectionError::UnknownModel("gpt-unknown".to_string()))
        );
        assert_eq!(
            ModelSelection::from_request(Some(ModelTier::Fast), Some("fast-mini")),
            Err(ModelSelectionError::ConflictingSelection)
        );
        assert_eq!(
            ModelSelection::from_request(None, Some("   ")),
            Err(ModelSelectionError::EmptyModel)
        );
        assert_eq!(
            ModelSelection::from_request(None, Some(" fast-mini ")),
            Ok(ModelSelection::Model("fast-mini"))
        );

        let messages = [ChatMessage {
            role: "user",
            content: "hello",
        }];
        assert!(
            router
                .chat_completion(&messages, ModelSelection::Tier(ModelTier::Reasoning))
                .await
                .is_err()
        );
        assert!(
            router
                .chat_stream(&messages, ModelSelection::Model("gpt-unknown"))
                .await
                .is_err()
        );
        assert!(
            router
                .generate_structured::<TechReport>("Assess Rust", ModelSelection::Model("unknown"))
                .await
                .is_err()
        );
        assert!(fast.recorded_calls().is_empty());
    }
}
