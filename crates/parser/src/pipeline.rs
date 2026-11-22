use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::time::Instant;

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::warn;

use crate::document_chunk::{DocumentChunk, DocumentChunker, DocumentChunkerConfig};
use crate::error::ParseError;
use crate::fingerprint::hash_text;
use crate::scanner::RepositoryScanner;

/// Configuration for the bounded asynchronous chunking pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineConfig {
    /// Bounded `mpsc` channel capacity enforcing backpressure between producer and consumer.
    pub channel_capacity: usize,
    /// Default consumer batch size when draining chunks.
    pub batch_size: usize,
    /// Maximum single-file size in bytes to ingest from disk (default: 2 MiB).
    pub max_file_size_bytes: usize,
    /// Whether directory walking respects `.gitignore` / `.ignore` rules.
    pub respect_gitignore: bool,
    /// Chunking strategy and token budget configuration.
    pub chunker: DocumentChunkerConfig,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 128,
            batch_size: 32,
            max_file_size_bytes: 2 * 1024 * 1024,
            respect_gitignore: true,
            chunker: DocumentChunkerConfig::default(),
        }
    }
}

/// An in-memory file payload fed into [`ChunkPipeline::stream_files`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInput {
    pub file_id: String,
    pub filename: String,
    pub content: String,
}

/// Item emitted through the bounded pipeline channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PipelineItem {
    Chunk(Box<DocumentChunk>),
    FileFailed { path: String, error: String },
}

/// Final execution statistics returned once the producer finishes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineStats {
    pub files_processed: usize,
    pub files_failed: usize,
    pub chunks_emitted: usize,
    pub total_tokens: usize,
    pub total_bytes: usize,
    pub elapsed_ms: u64,
    pub aborted_early: bool,
}

/// Consumer handle to a running bounded chunking pipeline.
///
/// Backpressure is enforced automatically: if the consumer slows down, the bounded
/// `tokio::sync::mpsc` channel fills up and suspends the blocking producer until
/// buffer slots free up. Dropping [`ChunkStream`] immediately signals the producer to stop.
pub struct ChunkStream {
    receiver: mpsc::Receiver<PipelineItem>,
    producer: JoinHandle<PipelineStats>,
}

impl ChunkStream {
    /// Receive the next [`PipelineItem`] from the bounded channel.
    pub async fn recv(&mut self) -> Option<PipelineItem> {
        self.receiver.recv().await
    }

    /// Receive up to `max_batch` items, awaiting at least one item if the channel is still open
    /// and then greedily draining immediately available buffered items without extra waiting.
    pub async fn recv_batch(&mut self, max_batch: usize) -> Vec<PipelineItem> {
        let limit = max_batch.max(1);
        let mut batch = Vec::with_capacity(limit);

        let Some(first) = self.receiver.recv().await else {
            return batch;
        };
        batch.push(first);

        while batch.len() < limit {
            match self.receiver.try_recv() {
                Ok(item) => batch.push(item),
                Err(_) => break,
            }
        }

        batch
    }

    /// Stream all emitted [`DocumentChunk`]s in batches of up to `batch_size` to an async consumer
    /// callback (such as a rate-limited embedding client or bulk database writer), returning
    /// final [`PipelineStats`] once the stream completes.
    pub async fn consume_batches<F, Fut>(
        mut self,
        batch_size: usize,
        mut on_batch: F,
    ) -> Result<PipelineStats, ParseError>
    where
        F: FnMut(Vec<DocumentChunk>) -> Fut,
        Fut: Future<Output = Result<(), ParseError>>,
    {
        let limit = batch_size.max(1);
        let mut pending_chunks = Vec::with_capacity(limit);

        while let Some(item) = self.receiver.recv().await {
            match item {
                PipelineItem::Chunk(chunk) => {
                    let chunk = *chunk;
                    pending_chunks.push(chunk);
                    if pending_chunks.len() >= limit {
                        let batch =
                            std::mem::replace(&mut pending_chunks, Vec::with_capacity(limit));
                        on_batch(batch).await?;
                    }
                }
                PipelineItem::FileFailed { path, error } => {
                    warn!(path = %path, error = %error, "Pipeline skipped failed file");
                }
            }
        }

        if !pending_chunks.is_empty() {
            on_batch(pending_chunks).await?;
        }

        self.producer
            .await
            .map_err(|e| ParseError::Pipeline(format!("producer task join error: {e}")))
    }

    /// Close the receiver early (triggering producer cancellation) and await producer shutdown.
    pub async fn cancel(mut self) -> Result<PipelineStats, ParseError> {
        self.receiver.close();
        self.producer
            .await
            .map_err(|e| ParseError::Pipeline(format!("producer task join error: {e}")))
    }

    /// Await the producer task after the channel has been drained.
    pub async fn join(self) -> Result<PipelineStats, ParseError> {
        drop(self.receiver);
        self.producer
            .await
            .map_err(|e| ParseError::Pipeline(format!("producer task join error: {e}")))
    }
}

/// Asynchronous bounded chunking pipeline decoupling memory usage from corpus size.
#[derive(Debug, Clone)]
pub struct ChunkPipeline {
    config: PipelineConfig,
}

impl Default for ChunkPipeline {
    fn default() -> Self {
        Self::new(PipelineConfig::default())
    }
}

impl ChunkPipeline {
    pub fn new(config: PipelineConfig) -> Self {
        let normalized = PipelineConfig {
            channel_capacity: config.channel_capacity.max(1),
            batch_size: config.batch_size.max(1),
            ..config
        };
        Self { config: normalized }
    }

    pub fn config(&self) -> &PipelineConfig {
        &self.config
    }

    /// Stream chunks from an in-memory sequence of [`FileInput`]s through a bounded `mpsc` channel.
    pub fn stream_files(&self, files: Vec<FileInput>) -> ChunkStream {
        let (tx, rx) = mpsc::channel(self.config.channel_capacity);
        let chunker = DocumentChunker::new(self.config.chunker.clone());

        let producer = tokio::task::spawn_blocking(move || {
            let start = Instant::now();
            let mut stats = PipelineStats::default();

            for file in files {
                if tx.is_closed() {
                    stats.aborted_early = true;
                    break;
                }

                let byte_len = file.content.len();
                match chunker.chunk_file(&file.file_id, &file.filename, &file.content) {
                    Ok(chunks) => {
                        stats.files_processed += 1;
                        stats.total_bytes += byte_len;

                        for chunk in chunks {
                            let tokens = chunk.token_count;
                            if tx.blocking_send(PipelineItem::Chunk(Box::new(chunk))).is_err() {
                                stats.aborted_early = true;
                                stats.elapsed_ms = start.elapsed().as_millis() as u64;
                                return stats;
                            }
                            stats.chunks_emitted += 1;
                            stats.total_tokens += tokens;
                        }
                    }
                    Err(err) => {
                        stats.files_failed += 1;
                        if tx
                            .blocking_send(PipelineItem::FileFailed {
                                path: file.filename,
                                error: err.to_string(),
                            })
                            .is_err()
                        {
                            stats.aborted_early = true;
                            break;
                        }
                    }
                }
            }

            stats.elapsed_ms = start.elapsed().as_millis() as u64;
            stats
        });

        ChunkStream {
            receiver: rx,
            producer,
        }
    }

    /// Lazily walk a directory on disk, reading and chunking one file at a time while
    /// streaming [`DocumentChunk`]s through a bounded `mpsc` channel so memory remains O(1).
    pub fn stream_directory(&self, root: impl Into<PathBuf>) -> Result<ChunkStream, ParseError> {
        let root_path = root.into();
        if !root_path.exists() {
            return Err(ParseError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("directory does not exist: {}", root_path.display()),
            )));
        }

        let (tx, rx) = mpsc::channel(self.config.channel_capacity);
        let chunker = DocumentChunker::new(self.config.chunker.clone());
        let respect_gitignore = self.config.respect_gitignore;
        let max_bytes = self.config.max_file_size_bytes;

        let producer = tokio::task::spawn_blocking(move || {
            let start = Instant::now();
            let mut stats = PipelineStats::default();

            let mut builder = WalkBuilder::new(&root_path);
            builder
                .git_ignore(respect_gitignore)
                .git_global(respect_gitignore)
                .git_exclude(respect_gitignore)
                .ignore(respect_gitignore)
                .hidden(true);

            for entry in builder.build() {
                if tx.is_closed() {
                    stats.aborted_early = true;
                    break;
                }

                let dent = match entry {
                    Ok(d) => d,
                    Err(err) => {
                        stats.files_failed += 1;
                        warn!("Skipping unreadable entry during pipeline walk: {err}");
                        continue;
                    }
                };

                let path = dent.path();
                if !dent.file_type().is_some_and(|ft| ft.is_file())
                    || !RepositoryScanner::is_supported_path(path)
                {
                    continue;
                }

                let rel_path = path
                    .strip_prefix(&root_path)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");

                let meta = match fs::metadata(path) {
                    Ok(m) => m,
                    Err(err) => {
                        stats.files_failed += 1;
                        if tx
                            .blocking_send(PipelineItem::FileFailed {
                                path: rel_path,
                                error: err.to_string(),
                            })
                            .is_err()
                        {
                            stats.aborted_early = true;
                            break;
                        }
                        continue;
                    }
                };

                if meta.len() as usize > max_bytes {
                    stats.files_failed += 1;
                    if tx
                        .blocking_send(PipelineItem::FileFailed {
                            path: rel_path,
                            error: format!("exceeds max_file_size_bytes ({max_bytes})"),
                        })
                        .is_err()
                    {
                        stats.aborted_early = true;
                        break;
                    }
                    continue;
                }

                let text = match fs::read_to_string(path) {
                    Ok(t) => t,
                    Err(err) => {
                        stats.files_failed += 1;
                        if tx
                            .blocking_send(PipelineItem::FileFailed {
                                path: rel_path,
                                error: err.to_string(),
                            })
                            .is_err()
                        {
                            stats.aborted_early = true;
                            break;
                        }
                        continue;
                    }
                };

                let file_id = hash_text(&rel_path)[..24].to_string();
                let byte_len = text.len();

                match chunker.chunk_file(&file_id, &rel_path, &text) {
                    Ok(chunks) => {
                        stats.files_processed += 1;
                        stats.total_bytes += byte_len;

                        for chunk in chunks {
                            let tokens = chunk.token_count;
                            if tx.blocking_send(PipelineItem::Chunk(Box::new(chunk))).is_err() {
                                stats.aborted_early = true;
                                stats.elapsed_ms = start.elapsed().as_millis() as u64;
                                return stats;
                            }
                            stats.chunks_emitted += 1;
                            stats.total_tokens += tokens;
                        }
                    }
                    Err(err) => {
                        stats.files_failed += 1;
                        if tx
                            .blocking_send(PipelineItem::FileFailed {
                                path: rel_path,
                                error: err.to_string(),
                            })
                            .is_err()
                        {
                            stats.aborted_early = true;
                            break;
                        }
                    }
                }
            }

            stats.elapsed_ms = start.elapsed().as_millis() as u64;
            stats
        });

        Ok(ChunkStream {
            receiver: rx,
            producer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn test_bounded_pipeline_batch_consumption_and_error_isolation() {
        let pipeline = ChunkPipeline::new(PipelineConfig {
            channel_capacity: 4,
            batch_size: 2,
            ..Default::default()
        });

        let files = vec![
            FileInput {
                file_id: "f1".to_string(),
                filename: "docs/intro.md".to_string(),
                content: "# Intro\n\nFirst paragraph.\n\n## Details\nSecond paragraph.".to_string(),
            },
            FileInput {
                file_id: "f2".to_string(),
                filename: "src/lib.rs".to_string(),
                content: "pub fn alpha() -> i32 { 1 }\npub fn beta() -> i32 { 2 }\n".to_string(),
            },
            FileInput {
                file_id: "f3".to_string(),
                filename: "image.png".to_string(),
                content: "not supported".to_string(),
            },
        ];

        let batches_seen = Arc::new(AtomicUsize::new(0));
        let chunks_seen = Arc::new(AtomicUsize::new(0));
        let b_clone = Arc::clone(&batches_seen);
        let c_clone = Arc::clone(&chunks_seen);

        let stream = pipeline.stream_files(files);
        let stats = stream
            .consume_batches(2, move |batch| {
                let b = Arc::clone(&b_clone);
                let c = Arc::clone(&c_clone);
                async move {
                    assert!(!batch.is_empty() && batch.len() <= 2);
                    for chunk in &batch {
                        chunk.validate_invariants()?;
                    }
                    b.fetch_add(1, Ordering::Relaxed);
                    c.fetch_add(batch.len(), Ordering::Relaxed);
                    Ok(())
                }
            })
            .await
            .expect("pipeline should finish cleanly");

        assert_eq!(stats.files_processed, 2);
        assert_eq!(stats.files_failed, 1);
        assert!(!stats.aborted_early);
        assert_eq!(stats.chunks_emitted, chunks_seen.load(Ordering::Relaxed));
        assert!(stats.chunks_emitted >= 3);
        assert!(batches_seen.load(Ordering::Relaxed) >= 2);
    }

    #[tokio::test]
    async fn test_bounded_channel_backpressure_and_early_cancellation() {
        // Tiny channel capacity = 2 to verify backpressure blocks producer cleanly
        // and early cancellation halts producer without hanging.
        let pipeline = ChunkPipeline::new(PipelineConfig {
            channel_capacity: 2,
            chunker: DocumentChunkerConfig {
                max_tokens: 64,
                overlap_tokens: 8,
                min_chunk_tokens: 8,
                ..Default::default()
            },
            ..Default::default()
        });

        let files: Vec<FileInput> = (0..30)
            .map(|i| FileInput {
                file_id: format!("file-{i}"),
                filename: format!("src/mod_{i}.rs"),
                content: format!(
                    "/// Doc {i}\npub fn fn_{i}_a() -> usize {{ {i} }}\npub fn fn_{i}_b() -> usize {{ {} }}\n",
                    i + 1
                ),
            })
            .collect();

        let mut stream = pipeline.stream_files(files);

        // Simulate slow consumer taking only the first 3 items, then cancelling early
        let first_batch = stream.recv_batch(3).await;
        assert!(!first_batch.is_empty());
        tokio::time::sleep(Duration::from_millis(5)).await;

        let stats = stream.cancel().await.expect("cancel should join cleanly");
        assert!(stats.aborted_early);
        assert!(
            stats.files_processed < 30,
            "producer should have stopped early under backpressure, processed {}",
            stats.files_processed
        );
    }

    #[tokio::test]
    async fn test_stream_directory_on_parser_src() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let src_dir = manifest_dir.join("src");

        let pipeline = ChunkPipeline::default();
        let stream = pipeline
            .stream_directory(&src_dir)
            .expect("stream_directory should start");

        let total_received = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&total_received);

        let stats = stream
            .consume_batches(16, move |batch| {
                let c = Arc::clone(&counter);
                async move {
                    c.fetch_add(batch.len(), Ordering::Relaxed);
                    Ok(())
                }
            })
            .await
            .expect("directory streaming should succeed");

        assert!(stats.files_processed >= 8);
        assert_eq!(stats.files_failed, 0);
        assert_eq!(stats.chunks_emitted, total_received.load(Ordering::Relaxed));
        assert!(stats.total_tokens > 0);
    }
}
