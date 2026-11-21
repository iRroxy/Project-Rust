use std::fs;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ai_parser::{DiffCalculator, DocumentContent, RepositoryScanner, ScanConfig};
use rayon::prelude::*;

fn main() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root: PathBuf = std::env::temp_dir().join(format!(
        "ai-parser-release-bench-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create temp dir");

    // Generate 500 realistic code & markdown files (~250 KB - 1 MB)
    for i in 0..200 {
        let dir = root.join("rust").join(format!("mod_{}", i % 10));
        fs::create_dir_all(&dir).unwrap();
        let src = format!(
            "/// Worker {i}\n#[derive(Debug)]\npub struct Worker{i} {{ id: usize }}\nimpl Worker{i} {{ pub fn run(&self) -> usize {{ self.id + {i} }} }}\npub fn helper_{i}(x: i32) -> i32 {{ x * 2 }}\n"
        );
        fs::write(dir.join(format!("w_{i}.rs")), src.repeat(4)).unwrap();
    }
    for i in 0..150 {
        let dir = root.join("web");
        fs::create_dir_all(&dir).unwrap();
        let src = format!(
            "/** Client {i} */\nexport interface Cfg{i} {{ id: string; }}\nexport class Client{i} {{ async call(x: string) {{ return x; }} }}\nexport const fn{i} = (n: number) => n + {i};\n"
        );
        fs::write(dir.join(format!("c_{i}.ts")), src.repeat(4)).unwrap();
    }
    for i in 0..50 {
        let dir = root.join("py");
        fs::create_dir_all(&dir).unwrap();
        let src = format!(
            "class Model{i}:\n    \"\"\"Model {i}\"\"\"\n    def run(self, x: int) -> int:\n        \"\"\"Run\"\"\"\n        return x + {i}\n"
        );
        fs::write(dir.join(format!("m_{i}.py")), src.repeat(4)).unwrap();
    }
    for i in 0..100 {
        let dir = root.join("docs");
        fs::create_dir_all(&dir).unwrap();
        let md = format!(
            "# Doc {i}\nIntro paragraph.\n\n## Section A\nDetails with `code_{i}`.\n\n```rust\nfn demo_{i}() {{}}\n```\n"
        );
        fs::write(dir.join(format!("d_{i}.md")), md.repeat(4)).unwrap();
    }

    let config = ScanConfig {
        max_file_size_bytes: 2 * 1024 * 1024,
        respect_gitignore: false,
    };

    let start = Instant::now();
    let report = RepositoryScanner::scan_directory_with_config(&root, &config).unwrap();
    let fps: Vec<_> = report
        .documents
        .par_iter()
        .map(DiffCalculator::fingerprint_document)
        .collect();
    let elapsed = start.elapsed();

    let mut symbols = 0usize;
    let mut sections = 0usize;
    for doc in &report.documents {
        match &doc.content {
            DocumentContent::Code(c) => symbols += c.symbols.len(),
            DocumentContent::Markdown(m) => sections += m.sections.len(),
            DocumentContent::PlainText { .. } => {}
        }
    }

    let secs = elapsed.as_secs_f64().max(1e-6);
    let mb = report.total_bytes as f64 / (1024.0 * 1024.0);
    println!("=== ai-parser Release Throughput Benchmark ===");
    println!("Files parsed    : {}", report.documents.len());
    println!(
        "Total size      : {:.2} MB ({} bytes)",
        mb, report.total_bytes
    );
    println!("AST symbols     : {}", symbols);
    println!("MD sections     : {}", sections);
    println!("Fingerprints    : {}", fps.len());
    println!("Elapsed time    : {:.2} ms", elapsed.as_secs_f64() * 1000.0);
    println!(
        "Throughput      : {:.2} MB/s ({:.0} files/sec)",
        mb / secs,
        report.documents.len() as f64 / secs
    );

    let _ = fs::remove_dir_all(&root);
}
