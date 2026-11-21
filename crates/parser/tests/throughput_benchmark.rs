use std::fs;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ai_parser::{DiffCalculator, DocumentContent, RepositoryScanner, ScanConfig};
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

fn generate_rust_sample(idx: usize) -> String {
    format!(
        r#"/// Service module #{idx} for concurrent RAG ingestion.
#[derive(Debug, Clone)]
pub struct Worker{idx} {{
    pub id: usize,
    pub weight: f64,
}}

/// Processing contract for worker #{idx}.
pub trait Processor{idx} {{
    fn process(&self, payload: &str) -> usize;
}}

impl Processor{idx} for Worker{idx} {{
    /// Count UTF-8 tokens in payload.
    fn process(&self, payload: &str) -> usize {{
        payload.split_whitespace().count() + self.id
    }}
}}

/// Helper to compute normalized score for item #{idx}.
pub fn normalize_{idx}(value: f64) -> f64 {{
    value.clamp(0.0, 1.0)
}}
"#
    )
}

fn generate_ts_sample(idx: usize) -> String {
    format!(
        r#"/** Metadata payload for document #{idx}. */
export interface DocMeta{idx} {{
    id: string;
    tokens: number;
}}

/** Streaming client #{idx}. */
export class StreamClient{idx} {{
    /** Fetch parsed symbols. */
    async fetchSymbols(path: string): Promise<DocMeta{idx}> {{
        return {{ id: path, tokens: {idx} }};
    }}
}}

/** Format score for display. */
export const formatScore{idx} = (score: number): string => score.toFixed(2);
"#
    )
}

fn generate_python_sample(idx: usize) -> String {
    format!(
        r#"class Indexer{idx}:
    """Incremental chunk indexer #{idx}."""

    def index(self, text: str) -> int:
        """Return token count estimate."""
        return len(text.split())


def score_{idx}(rank: int) -> float:
    """Compute reciprocal rank score."""
    return 1.0 / (60.0 + rank)
"#
    )
}

fn generate_markdown_sample(idx: usize) -> String {
    format!(
        r#"# Design Document #{idx}

Overview of subsystem #{idx} with inline `blake3` hashing and `rayon` execution.

## Architecture
- Parallel AST extraction via `tree-sitter`
- Zero-copy event parsing via `pulldown-cmark`

### Code Example
```rust
pub fn run_{idx}() -> usize {{ {idx} }}
```

## Summary
Subsystem #{idx} keeps peak memory low under high concurrency.
"#
    )
}

#[test]
fn test_benchmark_500_files_parallel_ast_parsing() {
    let guard = TempDirGuard::new("ai-parser-bench-500");
    let root = &guard.0;

    // Generate 500 mixed code & markdown files across subdirectories
    for i in 0..200 {
        let dir = root.join("rust").join(format!("mod_{}", i % 10));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("worker_{i}.rs")), generate_rust_sample(i)).unwrap();
    }
    for i in 0..150 {
        let dir = root.join("web").join(format!("pkg_{}", i % 10));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("client_{i}.ts")), generate_ts_sample(i)).unwrap();
    }
    for i in 0..50 {
        let dir = root.join("py");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("indexer_{i}.py")),
            generate_python_sample(i),
        )
        .unwrap();
    }
    for i in 0..100 {
        let dir = root.join("docs");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("doc_{i}.md")), generate_markdown_sample(i)).unwrap();
    }

    let start = Instant::now();
    let config = ScanConfig {
        max_file_size_bytes: 2 * 1024 * 1024,
        respect_gitignore: false,
    };
    let report =
        RepositoryScanner::scan_directory_with_config(root, &config).expect("scan 500 files");
    let fingerprints: Vec<_> = report
        .documents
        .par_iter()
        .map(DiffCalculator::fingerprint_document)
        .collect();
    let total_elapsed = start.elapsed();

    assert_eq!(report.documents.len(), 500);
    assert_eq!(fingerprints.len(), 500);
    assert_eq!(report.skipped_files, 0);

    let mut total_symbols = 0usize;
    let mut total_sections = 0usize;
    for doc in &report.documents {
        match &doc.content {
            DocumentContent::Code(code) => total_symbols += code.symbols.len(),
            DocumentContent::Markdown(md) => total_sections += md.sections.len(),
            DocumentContent::PlainText { .. } => {}
        }
    }

    // 200*6 (Rust) + 150*4 (TS) + 50*3 (Python) = 1950 AST symbols
    assert_eq!(total_symbols, 200 * 6 + 150 * 4 + 50 * 3);
    // 100*4 (Markdown H1/H2/H3/H2) = 400 sections
    assert_eq!(total_sections, 100 * 4);

    let secs = total_elapsed.as_secs_f64().max(1e-6);
    let mb_per_sec = (report.total_bytes as f64 / (1024.0 * 1024.0)) / secs;
    println!(
        "Parsed 500 files ({} bytes, {} AST symbols, {} MD sections) in {:.2} ms ({:.2} MB/s)",
        report.total_bytes,
        total_symbols,
        total_sections,
        total_elapsed.as_secs_f64() * 1000.0,
        mb_per_sec
    );
}
