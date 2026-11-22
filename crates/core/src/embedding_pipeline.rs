use crate::embedding::{EmbeddingError, EmbeddingProvider};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, warn};

/// Default number of texts packed into a single upstream embedding request.
pub const DEFAULT_EMBEDDING_BATCH_SIZE: usize = 16;
/// Default maximum number of concurrent upstream embedding requests.
pub const DEFAULT_EMBEDDING_MAX_CONCURRENCY: usize = 5;
/// Default retry attempts for transient upstream errors (429 / 5xx / network).
pub const DEFAULT_EMBEDDING_MAX_RETRIES: usize = 3;
/// Default base delay for exponential backoff retries.
pub const DEFAULT_EMBEDDING_INITIAL_BACKOFF: Duration = Duration::from_millis(200);
/// Default ceiling for exponential backoff (and `Retry-After`) waits.
pub const DEFAULT_EMBEDDING_MAX_BACKOFF: Duration = Duration::from_secs(10);

/// Configuration for [`BatchEmbeddingPipeline`].
#[derive(Debug, Clone)]
pub struct BatchEmbeddingConfig {
    /// Maximum texts per upstream `embed_batch` call (clamped to `>= 1`).
    pub batch_size: usize,
    /// Maximum concurrent in-flight upstream requests enforced via [`Semaphore`] (clamped to `>= 1`).
    pub max_concurrency: usize,
    /// Number of retries after an initial transient failure (`0` disables retries).
    pub max_retries: usize,
    /// Base delay for exponential backoff (`initial_backoff * 2^attempt`).
    pub initial_backoff: Duration,
    /// Upper bound for any single retry wait (including server `Retry-After` hints).
    pub max_backoff: Duration,
    /// Optional upfront vector dimensionality check (e.g. `Some(1536)` for `pgvector vector(1536)`).
    pub expected_dimensions: Option<usize>,
}

impl Default for BatchEmbeddingConfig {
    fn default() -> Self {
        Self {
            batch_size: DEFAULT_EMBEDDING_BATCH_SIZE,
            max_concurrency: DEFAULT_EMBEDDING_MAX_CONCURRENCY,
            max_retries: DEFAULT_EMBEDDING_MAX_RETRIES,
            initial_backoff: DEFAULT_EMBEDDING_INITIAL_BACKOFF,
            max_backoff: DEFAULT_EMBEDDING_MAX_BACKOFF,
            expected_dimensions: None,
        }
    }
}

impl BatchEmbeddingConfig {
    pub fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = batch_size.max(1);
        self
    }

    pub fn with_max_concurrency(mut self, max_concurrency: usize) -> Self {
        self.max_concurrency = max_concurrency.max(1);
        self
    }

    pub fn with_max_retries(mut self, max_retries: usize) -> Self {
        self.max_retries = max_retries;
        self
    }

    pub fn with_backoff(mut self, initial: Duration, max: Duration) -> Self {
        self.initial_backoff = initial;
        self.max_backoff = max.max(initial);
        self
    }

    pub fn with_expected_dimensions(mut self, dimensions: usize) -> Self {
        self.expected_dimensions = Some(dimensions);
        self
    }
}

/// Execution telemetry returned alongside embedded vectors.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingPipelineStats {
    pub total_items: usize,
    pub total_batches: usize,
    pub total_retries: usize,
    pub elapsed_ms: f64,
}

/// Rate-limited, concurrent batch embedding pipeline backed by [`tokio::sync::Semaphore`].
///
/// Splits large chunk collections into fixed-size batches, bounds concurrent upstream
/// requests with an `Arc<Semaphore>`, retries transient failures (429 / 5xx / network)
/// using exponential backoff with jitter *without* holding a semaphore permit during sleep,
/// and preserves exact input ordering.
#[derive(Clone)]
pub struct BatchEmbeddingPipeline {
    provider: Arc<dyn EmbeddingProvider>,
    semaphore: Arc<Semaphore>,
    config: BatchEmbeddingConfig,
}

impl std::fmt::Debug for BatchEmbeddingPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatchEmbeddingPipeline")
            .field("provider", &self.provider.provider_name())
            .field("model", &self.provider.model_name())
            .field("dimensions", &self.provider.dimensions())
            .field("config", &self.config)
            .finish()
    }
}

impl BatchEmbeddingPipeline {
    /// Create a pipeline with the given provider and configuration.
    pub fn new(provider: Arc<dyn EmbeddingProvider>, mut config: BatchEmbeddingConfig) -> Self {
        config.batch_size = config.batch_size.max(1);
        config.max_concurrency = config.max_concurrency.max(1);
        if config.max_backoff < config.initial_backoff {
            config.max_backoff = config.initial_backoff;
        }
        let semaphore = Arc::new(Semaphore::new(config.max_concurrency));
        Self {
            provider,
            semaphore,
            config,
        }
    }

    /// Create a pipeline with [`BatchEmbeddingConfig::default`] (batch 16, concurrency 5, 3 retries).
    pub fn with_defaults(provider: Arc<dyn EmbeddingProvider>) -> Self {
        Self::new(provider, BatchEmbeddingConfig::default())
    }

    /// Underlying embedding provider.
    pub fn provider(&self) -> &Arc<dyn EmbeddingProvider> {
        &self.provider
    }

    /// Pipeline configuration.
    pub fn config(&self) -> &BatchEmbeddingConfig {
        &self.config
    }

    /// Embed a slice of texts in concurrent, rate-limited batches while preserving input order.
    pub async fn embed_texts(
        &self,
        texts: &[String],
    ) -> Result<(Vec<Vec<f32>>, EmbeddingPipelineStats), EmbeddingError> {
        let started = Instant::now();

        if let Some(expected) = self.config.expected_dimensions {
            let actual = self.provider.dimensions();
            if actual != expected {
                return Err(EmbeddingError::DimensionMismatch {
                    index: 0,
                    expected,
                    actual,
                });
            }
        }

        if texts.is_empty() {
            return Ok((
                Vec::new(),
                EmbeddingPipelineStats {
                    total_items: 0,
                    total_batches: 0,
                    total_retries: 0,
                    elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                },
            ));
        }

        let batches: Vec<Vec<String>> = texts
            .chunks(self.config.batch_size)
            .map(|chunk| chunk.to_vec())
            .collect();
        let total_batches = batches.len();
        let retry_counter = Arc::new(AtomicUsize::new(0));

        debug!(
            provider = self.provider.provider_name(),
            model = self.provider.model_name(),
            total_items = texts.len(),
            total_batches,
            batch_size = self.config.batch_size,
            max_concurrency = self.config.max_concurrency,
            "Starting rate-limited batch embedding pipeline"
        );

        type BatchOutput = Result<(usize, Vec<Vec<f32>>), EmbeddingError>;
        let mut join_set: JoinSet<BatchOutput> = JoinSet::new();

        for (batch_idx, batch_texts) in batches.into_iter().enumerate() {
            let provider = Arc::clone(&self.provider);
            let semaphore = Arc::clone(&self.semaphore);
            let config = self.config.clone();
            let retries = Arc::clone(&retry_counter);

            join_set.spawn(async move {
                let vectors = execute_batch_with_retry(
                    provider,
                    semaphore,
                    batch_idx,
                    batch_texts,
                    &config,
                    &retries,
                )
                .await?;
                Ok((batch_idx, vectors))
            });
        }

        let mut ordered_batches: Vec<Option<Vec<Vec<f32>>>> = vec![None; total_batches];
        while let Some(joined) = join_set.join_next().await {
            match joined {
                Ok(Ok((batch_idx, vectors))) => {
                    ordered_batches[batch_idx] = Some(vectors);
                }
                Ok(Err(err)) => {
                    // Fail fast: abort remaining in-flight batch tasks so we don't burn API quota.
                    join_set.abort_all();
                    return Err(err);
                }
                Err(join_err) => {
                    join_set.abort_all();
                    return Err(EmbeddingError::InvalidResponse(format!(
                        "embedding worker task aborted or panicked: {join_err}"
                    )));
                }
            }
        }

        let mut embeddings = Vec::with_capacity(texts.len());
        for (batch_idx, slot) in ordered_batches.into_iter().enumerate() {
            let batch_vectors = slot.ok_or_else(|| {
                EmbeddingError::InvalidResponse(format!(
                    "missing embedding batch result at index {batch_idx}"
                ))
            })?;
            embeddings.extend(batch_vectors);
        }

        if embeddings.len() != texts.len() {
            return Err(EmbeddingError::CountMismatch {
                expected: texts.len(),
                actual: embeddings.len(),
            });
        }

        let stats = EmbeddingPipelineStats {
            total_items: texts.len(),
            total_batches,
            total_retries: retry_counter.load(Ordering::Relaxed),
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        };

        debug!(
            total_items = stats.total_items,
            total_batches = stats.total_batches,
            total_retries = stats.total_retries,
            elapsed_ms = stats.elapsed_ms,
            "Completed batch embedding pipeline"
        );

        Ok((embeddings, stats))
    }

    /// Convenience helper to embed arbitrary domain items (e.g. `DocumentChunk`) while zipping
    /// each item back together with its corresponding vector in original order.
    pub async fn embed_items<T, F>(
        &self,
        items: Vec<T>,
        extract_text: F,
    ) -> Result<(Vec<(T, Vec<f32>)>, EmbeddingPipelineStats), EmbeddingError>
    where
        F: Fn(&T) -> &str,
    {
        let texts: Vec<String> = items
            .iter()
            .map(|item| extract_text(item).to_owned())
            .collect();
        let (vectors, stats) = self.embed_texts(&texts).await?;
        let paired = items.into_iter().zip(vectors).collect();
        Ok((paired, stats))
    }
}

async fn execute_batch_with_retry(
    provider: Arc<dyn EmbeddingProvider>,
    semaphore: Arc<Semaphore>,
    batch_idx: usize,
    batch_texts: Vec<String>,
    config: &BatchEmbeddingConfig,
    retry_counter: &AtomicUsize,
) -> Result<Vec<Vec<f32>>, EmbeddingError> {
    let mut attempt: usize = 0;

    loop {
        // Acquire semaphore permit only for the active network call, never across backoff sleeps.
        let permit = semaphore.clone().acquire_owned().await.map_err(|_| {
            EmbeddingError::InvalidResponse("embedding semaphore closed".to_string())
        })?;

        let result = provider.embed_batch(&batch_texts).await;
        drop(permit);

        match result {
            Ok(vectors) => {
                if vectors.len() != batch_texts.len() {
                    return Err(EmbeddingError::CountMismatch {
                        expected: batch_texts.len(),
                        actual: vectors.len(),
                    });
                }
                return Ok(vectors);
            }
            Err(err) if err.is_retryable() && attempt < config.max_retries => {
                let delay = compute_backoff_delay(
                    attempt,
                    batch_idx,
                    config.initial_backoff,
                    config.max_backoff,
                    err.retry_after_secs(),
                );
                attempt += 1;
                retry_counter.fetch_add(1, Ordering::Relaxed);

                warn!(
                    provider = provider.provider_name(),
                    batch_idx,
                    attempt,
                    max_retries = config.max_retries,
                    delay_ms = delay.as_millis() as u64,
                    error = %err,
                    "Transient embedding error; retrying batch after backoff"
                );

                tokio::time::sleep(delay).await;
            }
            Err(err) => return Err(err),
        }
    }
}

/// Computes exponential backoff (`initial * 2^attempt`) with up to 25% deterministic jitter,
/// honoring server `Retry-After` hints and clamping to `max_backoff`.
fn compute_backoff_delay(
    attempt: usize,
    batch_idx: usize,
    initial_backoff: Duration,
    max_backoff: Duration,
    retry_after_secs: Option<u64>,
) -> Duration {
    let shift = (attempt as u32).min(16);
    let factor = 1_u32.checked_shl(shift).unwrap_or(u32::MAX);
    let exp_delay = initial_backoff.saturating_mul(factor);

    let base_delay = match retry_after_secs {
        Some(secs) => Duration::from_secs(secs).max(exp_delay),
        None => exp_delay,
    };

    let clamped = base_delay.min(max_backoff);
    if clamped.is_zero() {
        return clamped;
    }

    // Deterministic 0..25% jitter keyed by (batch_idx, attempt) to prevent synchronized bursts.
    let mix = ((batch_idx.wrapping_mul(31) ^ attempt.wrapping_mul(17)) % 25) as u32;
    let jitter = clamped / 100 * mix;
    clamped.saturating_add(jitter).min(max_backoff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::BoxFuture;
    use std::sync::Mutex;

    struct ConcurrencyTrackingProvider {
        dimensions: usize,
        active: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
        recorded_batch_sizes: Arc<Mutex<Vec<usize>>>,
    }

    impl EmbeddingProvider for ConcurrencyTrackingProvider {
        fn provider_name(&self) -> &str {
            "test-concurrency"
        }

        fn model_name(&self) -> &str {
            "test-model"
        }

        fn dimensions(&self) -> usize {
            self.dimensions
        }

        fn embed_batch<'a>(
            &'a self,
            texts: &'a [String],
        ) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
            Box::pin(async move {
                let current = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak.fetch_max(current, Ordering::SeqCst);
                self.recorded_batch_sizes.lock().unwrap().push(texts.len());

                // Invert latency by item index so later batches complete before earlier ones.
                let first_id: u64 = texts[0]
                    .strip_prefix("item-")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let delay_ms = 35_u64.saturating_sub(first_id / 2).max(5);
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;

                let vectors = texts
                    .iter()
                    .map(|t| {
                        let id: f32 = t
                            .strip_prefix("item-")
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(-1.0);
                        vec![id; self.dimensions]
                    })
                    .collect();

                self.active.fetch_sub(1, Ordering::SeqCst);
                Ok(vectors)
            })
        }
    }

    struct FlakyProvider {
        dimensions: usize,
        fail_times: usize,
        attempts: Arc<AtomicUsize>,
        fatal: bool,
    }

    impl EmbeddingProvider for FlakyProvider {
        fn provider_name(&self) -> &str {
            "test-flaky"
        }

        fn model_name(&self) -> &str {
            "flaky-model"
        }

        fn dimensions(&self) -> usize {
            self.dimensions
        }

        fn embed_batch<'a>(
            &'a self,
            texts: &'a [String],
        ) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
            Box::pin(async move {
                let n = self.attempts.fetch_add(1, Ordering::SeqCst);
                if self.fatal {
                    return Err(EmbeddingError::Upstream {
                        status: 400,
                        message: "bad request".to_string(),
                    });
                }
                if n < self.fail_times {
                    return if n.is_multiple_of(2) {
                        Err(EmbeddingError::RateLimited {
                            retry_after_secs: Some(1),
                            message: "too many requests".to_string(),
                        })
                    } else {
                        Err(EmbeddingError::Upstream {
                            status: 503,
                            message: "service temporarily unavailable".to_string(),
                        })
                    };
                }
                Ok(vec![vec![1.0; self.dimensions]; texts.len()])
            })
        }
    }

    #[tokio::test]
    async fn enforces_max_concurrency_and_preserves_input_order() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let sizes = Arc::new(Mutex::new(Vec::new()));

        let provider: Arc<dyn EmbeddingProvider> = Arc::new(ConcurrencyTrackingProvider {
            dimensions: 4,
            active: Arc::clone(&active),
            peak: Arc::clone(&peak),
            recorded_batch_sizes: Arc::clone(&sizes),
        });

        let config = BatchEmbeddingConfig::default()
            .with_batch_size(7)
            .with_max_concurrency(3);
        let pipeline = BatchEmbeddingPipeline::new(provider, config);

        let inputs: Vec<String> = (0..50).map(|i| format!("item-{i}")).collect();
        let (vectors, stats) = pipeline.embed_texts(&inputs).await.unwrap();

        assert_eq!(vectors.len(), 50);
        assert_eq!(stats.total_items, 50);
        assert_eq!(stats.total_batches, 8); // 7 * 7 + 1 = 50
        assert_eq!(stats.total_retries, 0);

        let observed_peak = peak.load(Ordering::SeqCst);
        assert!(
            observed_peak <= 3,
            "concurrency cap 3 violated: peak={observed_peak}"
        );
        assert!(
            observed_peak >= 2,
            "expected concurrent execution, peak={observed_peak}"
        );
        assert_eq!(active.load(Ordering::SeqCst), 0);

        // Verify exact order preservation despite out-of-order batch completion.
        for (i, vec) in vectors.iter().enumerate() {
            assert_eq!(vec, &vec![i as f32; 4]);
        }

        let mut recorded = sizes.lock().unwrap().clone();
        recorded.sort_unstable();
        assert_eq!(recorded, vec![1, 7, 7, 7, 7, 7, 7, 7]);
    }

    #[tokio::test]
    async fn retries_transient_429_and_5xx_with_exponential_backoff() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(FlakyProvider {
            dimensions: 3,
            fail_times: 2,
            attempts: Arc::clone(&attempts),
            fatal: false,
        });

        let config = BatchEmbeddingConfig::default()
            .with_batch_size(4)
            .with_max_concurrency(2)
            .with_max_retries(3)
            .with_backoff(Duration::from_millis(5), Duration::from_millis(20));
        let pipeline = BatchEmbeddingPipeline::new(provider, config);

        let inputs = vec!["chunk-a".to_string(), "chunk-b".to_string()];
        let (vectors, stats) = pipeline.embed_texts(&inputs).await.unwrap();

        assert_eq!(vectors.len(), 2);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert_eq!(stats.total_retries, 2);
    }

    #[tokio::test]
    async fn does_not_retry_non_retryable_errors() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(FlakyProvider {
            dimensions: 3,
            fail_times: 5,
            attempts: Arc::clone(&attempts),
            fatal: true,
        });

        let config = BatchEmbeddingConfig::default()
            .with_max_retries(3)
            .with_backoff(Duration::from_millis(5), Duration::from_millis(20));
        let pipeline = BatchEmbeddingPipeline::new(provider, config);

        let err = pipeline
            .embed_texts(&["bad".to_string()])
            .await
            .expect_err("400 error must fail immediately");
        assert!(matches!(err, EmbeddingError::Upstream { status: 400, .. }));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fails_fast_when_provider_dimensions_mismatch_expected() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(FlakyProvider {
            dimensions: 768,
            fail_times: 0,
            attempts: Arc::clone(&attempts),
            fatal: false,
        });

        let config = BatchEmbeddingConfig::default().with_expected_dimensions(1536);
        let pipeline = BatchEmbeddingPipeline::new(provider, config);

        let err = pipeline
            .embed_texts(&["hello".to_string()])
            .await
            .expect_err("dimension mismatch must fail before API call");
        assert!(matches!(
            err,
            EmbeddingError::DimensionMismatch {
                index: 0,
                expected: 1536,
                actual: 768
            }
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn handles_empty_input_and_generic_embed_items() {
        let provider: Arc<dyn EmbeddingProvider> =
            Arc::new(crate::embedding::MockEmbeddingProvider::new(8));
        let pipeline = BatchEmbeddingPipeline::with_defaults(provider);

        let (empty_vecs, empty_stats) = pipeline.embed_texts(&[]).await.unwrap();
        assert!(empty_vecs.is_empty());
        assert_eq!(empty_stats.total_items, 0);
        assert_eq!(empty_stats.total_batches, 0);

        #[derive(Debug, PartialEq)]
        struct MiniChunk {
            id: usize,
            text: String,
        }

        let chunks = vec![
            MiniChunk {
                id: 1,
                text: "rust tokio semaphore".to_string(),
            },
            MiniChunk {
                id: 2,
                text: "pgvector cosine search".to_string(),
            },
        ];

        let (paired, stats) = pipeline
            .embed_items(chunks, |c| c.text.as_str())
            .await
            .unwrap();
        assert_eq!(stats.total_items, 2);
        assert_eq!(paired.len(), 2);
        assert_eq!(paired[0].0.id, 1);
        assert_eq!(paired[1].0.id, 2);
        assert_eq!(paired[0].1.len(), 8);
        assert_eq!(paired[1].1.len(), 8);
    }
}
