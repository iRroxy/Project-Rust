use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ai_parser::{
    ChunkPipeline, DocumentChunker, DocumentChunkerConfig, PipelineConfig, TokenizerEncoding,
};
use rayon::prelude::*;

struct TempDirGuard(PathBuf);

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp bench dir");
        Self(dir)
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn build_rust_module(file_idx: usize, functions_per_file: usize) -> String {
    let mut out = String::with_capacity(functions_per_file * 220);
    out.push_str("use std::collections::HashMap;\n\n");
    out.push_str(&format!(
        "/// Pipeline worker #{file_idx}.\npub struct Worker{file_idx} {{\n    pub id: usize,\n}}\n\n"
    ));
    for f in 0..functions_per_file {
        out.push_str(&format!(
            r#"/// Compute reciprocal rank score for batch {file_idx}-{f}.
pub fn score_{file_idx}_{f}(ranks: &[usize], k: f32) -> f32 {{
    let mut sum = 0.0_f32;
    for &r in ranks {{
        sum += 1.0 / (k + r as f32);
    }}
    sum
}}

"#
        ));
    }
    out
}

fn build_markdown_doc(doc_idx: usize, sections: usize) -> String {
    let mut out = String::with_capacity(sections * 420);
    out.push_str(&format!("# Knowledge Base Document #{doc_idx}\n\n"));
    for s in 0..sections {
        out.push_str(&format!(
            r#"## Section {s}: Hybrid Retrieval & Chunking Architecture

This section explains how the Rust RAG engine parses document #{doc_idx} section {s} with zero-copy Markdown events, preserves hierarchical breadcrumbs, and enforces exact BPE token limits without tearing fenced code blocks.

```rust
pub fn retrieve_{doc_idx}_{s}(top_k: usize) -> usize {{
    top_k.min(64)
}}
```

"#
        ));
    }
    out
}

#[test]
fn test_chunk_10k_lines_source_code_in_parallel() {
    // 50 Rust files x 25 functions (~225 lines each) => >11,000 lines of source code
    let files: Vec<(String, String, String)> = (0..50)
        .map(|i| {
            (
                format!("file-{i}"),
                format!("crates/bench/src/worker_{i}.rs"),
                build_rust_module(i, 25),
            )
        })
        .collect();

    let total_lines: usize = files.iter().map(|(_, _, s)| s.lines().count()).sum();
    let total_bytes: usize = files.iter().map(|(_, _, s)| s.len()).sum();
    assert!(
        total_lines >= 10_000,
        "expected >= 10,000 lines, got {total_lines}"
    );

    let chunker = DocumentChunker::new(DocumentChunkerConfig {
        max_tokens: 512,
        overlap_tokens: 64,
        min_chunk_tokens: 48,
        encoding: TokenizerEncoding::Cl100kBase,
        include_context_headers: true,
    });

    let start = Instant::now();
    let chunk_lists: Vec<_> = files
        .par_iter()
        .map(|(id, path, src)| chunker.chunk_file(id, path, src).expect("chunk rust file"))
        .collect();
    let elapsed = start.elapsed();

    let total_chunks: usize = chunk_lists.iter().map(Vec::len).sum();
    let mut total_tokens = 0usize;
    for list in &chunk_lists {
        for chunk in list {
            assert!(
                chunk.token_count <= 512,
                "chunk exceeded 512 tokens: {}",
                chunk.token_count
            );
            chunk.validate_invariants().expect("valid chunk invariants");
            total_tokens += chunk.token_count;
        }
    }

    assert!(total_chunks >= 50 * 25);
    let secs = elapsed.as_secs_f64().max(1e-6);
    println!(
        "Parallel code chunking: {} lines ({:.2} KB) -> {} chunks ({} tokens) in {:.2} ms ({:.2} MB/s)",
        total_lines,
        total_bytes as f64 / 1024.0,
        total_chunks,
        total_tokens,
        elapsed.as_secs_f64() * 1000.0,
        (total_bytes as f64 / (1024.0 * 1024.0)) / secs
    );
}

#[tokio::test]
async fn test_stream_chunk_large_markdown_corpus_with_backpressure() {
    let guard = TempDirGuard::new("ai-parser-chunk-stream-bench");
    let root = &guard.0;

    // 10 Markdown files x 120 sections (~400 KB total in debug test for fast CI runs)
    for d in 0..10 {
        let content = build_markdown_doc(d, 120);
        fs::write(root.join(format!("doc_{d}.md")), content).unwrap();
    }

    let pipeline = ChunkPipeline::new(PipelineConfig {
        channel_capacity: 128,
        batch_size: 32,
        respect_gitignore: false,
        chunker: DocumentChunkerConfig {
            max_tokens: 512,
            overlap_tokens: 64,
            min_chunk_tokens: 48,
            encoding: TokenizerEncoding::Cl100kBase,
            include_context_headers: true,
        },
        ..Default::default()
    });

    let start = Instant::now();
    let received = Arc::new(AtomicUsize::new(0));
    let r_clone = Arc::clone(&received);

    let stream = pipeline
        .stream_directory(root)
        .expect("start markdown stream");
    let stats = stream
        .consume_batches(32, move |batch| {
            let r = Arc::clone(&r_clone);
            async move {
                for chunk in &batch {
                    assert!(chunk.token_count <= 512);
                }
                r.fetch_add(batch.len(), Ordering::Relaxed);
                Ok(())
            }
        })
        .await
        .expect("stream markdown corpus");
    let elapsed = start.elapsed();

    assert_eq!(stats.files_processed, 10);
    assert_eq!(stats.files_failed, 0);
    assert!(!stats.aborted_early);
    assert_eq!(stats.chunks_emitted, received.load(Ordering::Relaxed));
    assert!(stats.chunks_emitted >= 100);

    let secs = elapsed.as_secs_f64().max(1e-6);
    println!(
        "Bounded stream chunking: {:.2} KB -> {} chunks ({} tokens) in {:.2} ms ({:.2} MB/s)",
        stats.total_bytes as f64 / 1024.0,
        stats.chunks_emitted,
        stats.total_tokens,
        elapsed.as_secs_f64() * 1000.0,
        (stats.total_bytes as f64 / (1024.0 * 1024.0)) / secs
    );
}
