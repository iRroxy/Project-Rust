use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use ignore::WalkBuilder;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::code::{CodeParser, ParsedCode, SupportedLanguage};
use crate::error::ParseError;
use crate::markdown::{MarkdownParser, ParsedMarkdown};

/// Structured payload of a parsed document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DocumentContent {
    Markdown(ParsedMarkdown),
    Code(ParsedCode),
    PlainText { content: String, total_chars: usize },
}

/// Unified parsed document representation for both Markdown and source code files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedDocument {
    pub path: String,
    pub size_bytes: usize,
    pub content: DocumentContent,
}

/// Configuration for repository scanning.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// Maximum file size to read and parse (default: 2 MiB).
    pub max_file_size_bytes: usize,
    /// Whether to honor `.gitignore` / `.ignore` rules (default: true).
    pub respect_gitignore: bool,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            max_file_size_bytes: 2 * 1024 * 1024,
            respect_gitignore: true,
        }
    }
}

/// Summary report returned after parallel repository scanning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanReport {
    pub documents: Vec<ParsedDocument>,
    pub skipped_files: usize,
    pub total_bytes: usize,
    pub elapsed_ms: u64,
}

/// Multi-threaded repository and document parser backed by `ignore` + `rayon`.
pub struct RepositoryScanner;

impl RepositoryScanner {
    /// Check whether a file path has a supported extension for structured parsing.
    pub fn is_supported_path(path: &Path) -> bool {
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return false;
        };
        let lower = ext.to_ascii_lowercase();
        matches!(lower.as_str(), "md" | "mdx" | "markdown" | "txt")
            || SupportedLanguage::from_extension(&lower).is_some()
    }

    /// Parse raw UTF-8 text content using the filename's extension to choose the parser.
    pub fn parse_content(filename: &str, text: &str) -> Result<ParsedDocument, ParseError> {
        let path = Path::new(filename);
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();

        let content = match ext.as_str() {
            "md" | "mdx" | "markdown" => DocumentContent::Markdown(MarkdownParser::parse(text)),
            "txt" => DocumentContent::PlainText {
                total_chars: text.chars().count(),
                content: text.to_string(),
            },
            other => {
                let lang = SupportedLanguage::from_extension(other)
                    .ok_or_else(|| ParseError::UnsupportedLanguage(other.to_string()))?;
                DocumentContent::Code(CodeParser::parse(text, lang)?)
            }
        };

        Ok(ParsedDocument {
            path: normalize_path(filename),
            size_bytes: text.len(),
            content,
        })
    }

    /// Scan a directory with default configuration and parse all supported files in parallel.
    pub fn scan_directory(root: impl AsRef<Path>) -> Result<ScanReport, ParseError> {
        Self::scan_directory_with_config(root, &ScanConfig::default())
    }

    /// Scan a directory with custom [`ScanConfig`] using `ignore` for discovery
    /// and `rayon` for data-parallel AST/Markdown parsing.
    pub fn scan_directory_with_config(
        root: impl AsRef<Path>,
        config: &ScanConfig,
    ) -> Result<ScanReport, ParseError> {
        let start = Instant::now();
        let root_path = root.as_ref();

        if !root_path.exists() {
            return Err(ParseError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("directory does not exist: {}", root_path.display()),
            )));
        }

        let mut builder = WalkBuilder::new(root_path);
        builder
            .git_ignore(config.respect_gitignore)
            .git_global(config.respect_gitignore)
            .git_exclude(config.respect_gitignore)
            .ignore(config.respect_gitignore)
            .hidden(true);

        let mut candidate_files: Vec<PathBuf> = Vec::new();
        let mut skipped_during_walk: usize = 0;

        for entry in builder.build() {
            match entry {
                Ok(dent) => {
                    let path = dent.path();
                    if dent.file_type().is_some_and(|ft| ft.is_file()) {
                        if Self::is_supported_path(path) {
                            candidate_files.push(path.to_path_buf());
                        } else {
                            skipped_during_walk += 1;
                        }
                    }
                }
                Err(err) => {
                    warn!("Skipping directory entry due to walk error: {err}");
                    skipped_during_walk += 1;
                }
            }
        }

        let skipped_during_parse = AtomicUsize::new(0);
        let max_bytes = config.max_file_size_bytes;

        let mut documents: Vec<ParsedDocument> = candidate_files
            .par_iter()
            .filter_map(|file_path| {
                let meta = match fs::metadata(file_path) {
                    Ok(m) => m,
                    Err(err) => {
                        warn!("Failed to stat {}: {err}", file_path.display());
                        skipped_during_parse.fetch_add(1, Ordering::Relaxed);
                        return None;
                    }
                };

                if meta.len() as usize > max_bytes {
                    skipped_during_parse.fetch_add(1, Ordering::Relaxed);
                    return None;
                }

                let text = match fs::read_to_string(file_path) {
                    Ok(t) => t,
                    Err(err) => {
                        warn!("Failed to read UTF-8 file {}: {err}", file_path.display());
                        skipped_during_parse.fetch_add(1, Ordering::Relaxed);
                        return None;
                    }
                };

                let rel_path = file_path
                    .strip_prefix(root_path)
                    .unwrap_or(file_path)
                    .to_string_lossy()
                    .to_string();

                match Self::parse_content(&rel_path, &text) {
                    Ok(doc) => Some(doc),
                    Err(err) => {
                        warn!("Failed to parse {}: {err}", file_path.display());
                        skipped_during_parse.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                }
            })
            .collect();

        // Keep output deterministic across multi-threaded runs
        documents.sort_by(|a, b| a.path.cmp(&b.path));

        let total_bytes: usize = documents.iter().map(|d| d.size_bytes).sum();
        let skipped_files = skipped_during_walk + skipped_during_parse.load(Ordering::Relaxed);

        Ok(ScanReport {
            documents,
            skipped_files,
            total_bytes,
            elapsed_ms: start.elapsed().as_millis() as u64,
        })
    }
}

fn normalize_path(raw: &str) -> String {
    raw.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_parse_content_dispatch() {
        let md = RepositoryScanner::parse_content("docs/README.md", "# Hello\nWorld")
            .expect("md should parse");
        assert_eq!(md.path, "docs/README.md");
        assert!(matches!(md.content, DocumentContent::Markdown(_)));

        let rs = RepositoryScanner::parse_content("src\\main.rs", "pub fn main() {}")
            .expect("rs should parse");
        assert_eq!(rs.path, "src/main.rs");
        assert!(matches!(rs.content, DocumentContent::Code(_)));

        let err = RepositoryScanner::parse_content("image.png", "binary");
        assert!(matches!(err, Err(ParseError::UnsupportedLanguage(_))));
    }

    #[test]
    fn test_scan_crates_directory_in_parallel() {
        // Scan our own `crates/parser/src` directory as a real multi-file test
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let src_dir = manifest_dir.join("src");

        let report =
            RepositoryScanner::scan_directory(&src_dir).expect("scanning parser src should work");
        assert!(report.documents.len() >= 4);
        assert!(report.total_bytes > 0);
        assert!(
            report
                .documents
                .iter()
                .any(|d| d.path.ends_with("markdown.rs"))
        );
        assert!(report.documents.iter().any(|d| d.path.ends_with("code.rs")));
    }
}
