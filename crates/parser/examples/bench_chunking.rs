use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ai_parser::{
    ChunkPipeline, DocumentChunker, DocumentChunkerConfig, PipelineConfig, TokenizerEncoding,
};
use rayon::prelude::*;

fn build_markdown_doc(doc_idx: usize, sections: usize) -> String {
    let mut out = String::with_capacity(sections * 520);
    out.push_str(&format!(
        "# Enterprise Architecture Handbook #{doc_idx}\n\n"
    ));
    for s in 0..sections {
        out.push_str(&format!(
            r#"## Section {s}: High-Throughput Rust RAG Ingestion

Document #{doc_idx}, section {s} describes bounded backpressure streaming using `tokio::sync::mpsc`, exact BPE token counting via `tiktoken-rs`, and hierarchical breadcrumb preservation so downstream vector retrieval keeps full semantic context across H1–H6 boundaries.

- Zero-copy event parsing with `pulldown-cmark`
- Incremental change detection using SIMD `blake3` fingerprints
- Dual-channel hybrid search combining `pgvector` HNSW and `tsvector` BM25

```rust
pub fn score_candidate_{doc_idx}_{s}(rank: usize, k: f32) -> f32 {{
    1.0 / (k + rank as f32)
}}
```

"#
        ));
    }
    out
}

fn build_rust_module(file_idx: usize, functions_per_file: usize) -> String {
    let mut out = String::with_capacity(functions_per_file * 240);
    out.push_str("use std::collections::HashMap;\n\n");
    out.push_str(&format!(
        "/// Concurrent retrieval worker #{file_idx}.\npub struct Worker{file_idx} {{\n    pub id: usize,\n}}\n\n"
    ));
    for f in 0..functions_per_file {
        out.push_str(&format!(
            r#"/// Compute fused score for candidate {file_idx}-{f}.
pub fn fuse_{file_idx}_{f}(scores: &mut HashMap<usize, f32>, rank: usize) -> f32 {{
    let delta = 1.0 / (60.0 + rank as f32);
    *scores.entry({f}).or_insert(0.0) += delta;
    delta
}}

"#
        ));
    }
    out
}

#[tokio::main]
async fn main() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root: PathBuf = std::env::temp_dir().join(format!(
        "ai-parser-chunk-release-bench-{}-{nanos}",
        std::process::id()
    ));
    let docs_dir = root.join("docs");
    let code_dir = root.join("code");
    fs::create_dir_all(&docs_dir).unwrap();
    fs::create_dir_all(&code_dir).unwrap();

    // 1. Generate 10.5+ MB of structured Markdown across 20 files (~540 KB each)
    let mut md_bytes = 0usize;
    for d in 0..20 {
        let md = build_markdown_doc(d, 1000);
        md_bytes += md.len();
        fs::write(docs_dir.join(format!("handbook_{d:02}.md")), md).unwrap();
    }

    // 2. Generate 12,000+ lines of Rust source code across 40 files
    let mut code_files = Vec::with_capacity(40);
    let mut code_lines = 0usize;
    let mut code_bytes = 0usize;
    for i in 0..40 {
        let src = build_rust_module(i, 38);
        code_lines += src.lines().count();
        code_bytes += src.len();
        fs::write(code_dir.join(format!("worker_{i:02}.rs")), &src).unwrap();
        code_files.push((format!("code-{i}"), format!("src/worker_{i:02}.rs"), src));
    }

    let chunker_cfg = DocumentChunkerConfig {
        max_tokens: 512,
        overlap_tokens: 64,
        min_chunk_tokens: 48,
        encoding: TokenizerEncoding::Cl100kBase,
        include_context_headers: true,
    };

    // Phase A: Bounded async streaming over 10MB+ Markdown corpus (O(1) memory)
    let pipeline = ChunkPipeline::new(PipelineConfig {
        channel_capacity: 128,
        batch_size: 32,
        max_file_size_bytes: 2 * 1024 * 1024,
        respect_gitignore: false,
        chunker: chunker_cfg.clone(),
    });

    let stream_start = Instant::now();
    let streamed_chunks = Arc::new(AtomicUsize::new(0));
    let s_clone = Arc::clone(&streamed_chunks);
    let stream = pipeline.stream_directory(&docs_dir).unwrap();
    let stats = stream
        .consume_batches(32, move |batch| {
            let c = Arc::clone(&s_clone);
            async move {
                c.fetch_add(batch.len(), Ordering::Relaxed);
                Ok(())
            }
        })
        .await
        .unwrap();
    let stream_elapsed = stream_start.elapsed();
    let stream_secs = stream_elapsed.as_secs_f64().max(1e-6);

    // Phase B: Multi-threaded Rayon AST chunking over 12,000+ lines of Rust code
    let chunker = DocumentChunker::new(chunker_cfg);
    let code_start = Instant::now();
    let code_chunks: usize = code_files
        .par_iter()
        .map(|(id, path, src)| chunker.chunk_file(id, path, src).unwrap().len())
        .sum();
    let code_elapsed = code_start.elapsed();
    let code_secs = code_elapsed.as_secs_f64().max(1e-6);

    println!("=== ai-parser Week 6 Chunking Pipeline Benchmark ===");
    println!(
        "[Phase A: Bounded mpsc Stream] Markdown: {:.2} MB (20 files) -> {} chunks ({} tokens)",
        md_bytes as f64 / (1024.0 * 1024.0),
        stats.chunks_emitted,
        stats.total_tokens
    );
    println!(
        "  Elapsed: {:.2} ms | Throughput: {:.2} MB/s ({:.0} chunks/sec)",
        stream_elapsed.as_secs_f64() * 1000.0,
        (md_bytes as f64 / (1024.0 * 1024.0)) / stream_secs,
        stats.chunks_emitted as f64 / stream_secs
    );
    println!(
        "[Phase B: Rayon Parallel AST]  Rust Code: {} lines ({:.2} KB, 40 files) -> {} AST chunks",
        code_lines,
        code_bytes as f64 / 1024.0,
        code_chunks
    );
    println!(
        "  Elapsed: {:.2} ms | Throughput: {:.2} MB/s ({:.0} lines/sec)",
        code_elapsed.as_secs_f64() * 1000.0,
        (code_bytes as f64 / (1024.0 * 1024.0)) / code_secs,
        code_lines as f64 / code_secs
    );

    let _ = fs::remove_dir_all(&root);
}
