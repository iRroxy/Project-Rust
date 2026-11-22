use serde::{Deserialize, Serialize};

use crate::code::{CodeParser, CodeSymbol, ParsedCode, SupportedLanguage, SymbolKind};
use crate::error::ParseError;
use crate::fingerprint::hash_text;
use crate::tokenizer::{TokenCounter, TokenizerEncoding};

/// Configuration controlling AST-aware source code chunking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeChunkConfig {
    /// Hard upper bound on BPE tokens per chunk (including header and imports).
    pub max_tokens: usize,
    /// Target token overlap when an oversized single symbol must be split across lines.
    pub overlap_tokens: usize,
    /// BPE vocabulary used for exact token counting.
    pub encoding: TokenizerEncoding,
    /// Whether to prepend file path, derived module path, and symbol context comments.
    pub include_file_header: bool,
    /// Whether to attach top-of-file import / module declarations for cross-file context.
    pub include_imports: bool,
    /// Maximum tokens allocated to leading import/module declarations.
    pub max_import_tokens: usize,
}

impl Default for CodeChunkConfig {
    fn default() -> Self {
        Self {
            max_tokens: 512,
            overlap_tokens: 64,
            encoding: TokenizerEncoding::Cl100kBase,
            include_file_header: true,
            include_imports: true,
            max_import_tokens: 96,
        }
    }
}

/// An AST-aligned code chunk preserving function/struct integrity and file/module metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeChunk {
    pub chunk_index: usize,
    pub file_path: String,
    pub module_path: String,
    pub language: SupportedLanguage,
    pub symbol_name: Option<String>,
    pub parent_symbol: Option<String>,
    pub symbol_kind: Option<SymbolKind>,
    /// 1-indexed starting line number in the original file.
    pub start_line: usize,
    /// 1-indexed ending line number in the original file (inclusive).
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    /// Final chunk text including optional file/module header, imports, docstring, and code.
    pub content: String,
    /// Underlying source code slice without synthetic headers.
    pub raw_code: String,
    /// Exact BPE token count of `content`.
    pub token_count: usize,
    /// Blake3 digest of `content`.
    pub content_hash: String,
}

/// AST-aware source code chunker backed by `tree-sitter` symbols and exact BPE tokenization.
#[derive(Debug, Clone)]
pub struct CodeChunker {
    config: CodeChunkConfig,
    counter: TokenCounter,
}

impl Default for CodeChunker {
    fn default() -> Self {
        Self::new(CodeChunkConfig::default())
    }
}

impl CodeChunker {
    pub fn new(config: CodeChunkConfig) -> Self {
        let max_tokens = config.max_tokens.max(1);
        let overlap_tokens = config.overlap_tokens.min(max_tokens.saturating_sub(1));
        let max_import_tokens = config.max_import_tokens.min(max_tokens / 3);
        let normalized = CodeChunkConfig {
            max_tokens,
            overlap_tokens,
            max_import_tokens,
            ..config
        };
        let counter = TokenCounter::new(normalized.encoding);
        Self {
            config: normalized,
            counter,
        }
    }

    pub fn config(&self) -> &CodeChunkConfig {
        &self.config
    }

    pub fn counter(&self) -> TokenCounter {
        self.counter
    }

    /// Parse raw source code with `tree-sitter` and split into AST-aligned [`CodeChunk`]s.
    pub fn chunk_source(
        &self,
        file_path: &str,
        source: &str,
        language: SupportedLanguage,
    ) -> Result<Vec<CodeChunk>, ParseError> {
        let parsed = CodeParser::parse(source, language)?;
        Ok(self.chunk_parsed(file_path, source, &parsed))
    }

    /// Split an already-parsed [`ParsedCode`] file into AST-aligned [`CodeChunk`]s.
    pub fn chunk_parsed(
        &self,
        file_path: &str,
        source: &str,
        parsed: &ParsedCode,
    ) -> Vec<CodeChunk> {
        if source.trim().is_empty() {
            return Vec::new();
        }

        let normalized_path = file_path.replace('\\', "/");
        let module_path = derive_module_path(&normalized_path, parsed.language);
        let imports_block = if self.config.include_imports {
            extract_imports_header(
                source,
                parsed.language,
                &self.counter,
                self.config.max_import_tokens,
            )
        } else {
            String::new()
        };

        // Fallback for files with no extracted AST declarations (e.g. `lib.rs` re-exports or scripts)
        if parsed.symbols.is_empty() {
            return self.chunk_fallback_file(
                &normalized_path,
                &module_path,
                parsed.language,
                source,
            );
        }

        // Partition symbols into top-level nodes vs. children nested inside containers (Impl/Trait/Class)
        let plan = build_symbol_plan(&parsed.symbols);
        let mut chunks = Vec::new();

        for item in plan {
            match item {
                SymbolPlanItem::Standalone(sym) => {
                    self.emit_symbol_chunks(
                        &normalized_path,
                        &module_path,
                        parsed.language,
                        &imports_block,
                        sym,
                        None,
                        &mut chunks,
                    );
                }
                SymbolPlanItem::Container {
                    container,
                    children,
                } => {
                    // Keep the entire container (impl/trait/class) intact if it fits within max_tokens;
                    // otherwise decompose cleanly along AST child method boundaries to avoid tearing functions.
                    let container_body = render_symbol_with_doc(container, parsed.language);
                    let header = self.build_header(
                        &normalized_path,
                        &module_path,
                        parsed.language,
                        &imports_block,
                        Some(container.name.as_str()),
                        None,
                        Some(container.kind),
                        container.start_line,
                        container.end_line,
                    );
                    let full = combine_header_and_code(&header, &container_body);

                    if children.is_empty() || self.counter.count(&full) <= self.config.max_tokens {
                        self.emit_symbol_chunks(
                            &normalized_path,
                            &module_path,
                            parsed.language,
                            &imports_block,
                            container,
                            None,
                            &mut chunks,
                        );
                    } else {
                        for child in children {
                            self.emit_symbol_chunks(
                                &normalized_path,
                                &module_path,
                                parsed.language,
                                &imports_block,
                                child,
                                Some(container.name.as_str()),
                                &mut chunks,
                            );
                        }
                    }
                }
            }
        }

        chunks
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_symbol_chunks(
        &self,
        file_path: &str,
        module_path: &str,
        language: SupportedLanguage,
        imports_block: &str,
        symbol: &CodeSymbol,
        parent_symbol: Option<&str>,
        out: &mut Vec<CodeChunk>,
    ) {
        let code_body = render_symbol_with_doc(symbol, language);
        let header = self.build_header(
            file_path,
            module_path,
            language,
            imports_block,
            Some(symbol.name.as_str()),
            parent_symbol,
            Some(symbol.kind),
            symbol.start_line,
            symbol.end_line,
        );
        let full = combine_header_and_code(&header, &code_body);

        if self.counter.count(&full) <= self.config.max_tokens {
            out.push(self.build_chunk(
                out.len(),
                file_path,
                module_path,
                language,
                Some(symbol.name.clone()),
                parent_symbol.map(str::to_string),
                Some(symbol.kind),
                symbol.start_line,
                symbol.end_line,
                symbol.start_byte,
                symbol.end_byte,
                full,
                code_body,
            ));
            return;
        }

        // Single AST symbol exceeds max_tokens: split along line boundaries with overlap while
        // preserving file/module/symbol header on every fragment.
        let fragments = self.split_lines_within_budget(&header, &code_body, symbol.start_line);
        for (frag_code, frag_start_line, frag_end_line) in fragments {
            let frag_header = self.build_header(
                file_path,
                module_path,
                language,
                imports_block,
                Some(symbol.name.as_str()),
                parent_symbol,
                Some(symbol.kind),
                frag_start_line,
                frag_end_line,
            );
            let combined = combine_header_and_code(&frag_header, &frag_code);
            let bounded = if self.counter.count(&combined) > self.config.max_tokens {
                self.counter.truncate(&combined, self.config.max_tokens)
            } else {
                combined
            };
            out.push(self.build_chunk(
                out.len(),
                file_path,
                module_path,
                language,
                Some(symbol.name.clone()),
                parent_symbol.map(str::to_string),
                Some(symbol.kind),
                frag_start_line,
                frag_end_line,
                symbol.start_byte,
                symbol.end_byte,
                bounded,
                frag_code,
            ));
        }
    }

    fn chunk_fallback_file(
        &self,
        file_path: &str,
        module_path: &str,
        language: SupportedLanguage,
        source: &str,
    ) -> Vec<CodeChunk> {
        let total_lines = source.lines().count().max(1);
        let header = self.build_header(
            file_path,
            module_path,
            language,
            "",
            None,
            None,
            None,
            1,
            total_lines,
        );
        let full = combine_header_and_code(&header, source.trim());
        if self.counter.count(&full) <= self.config.max_tokens {
            return vec![self.build_chunk(
                0,
                file_path,
                module_path,
                language,
                None,
                None,
                None,
                1,
                total_lines,
                0,
                source.len(),
                full,
                source.trim().to_string(),
            )];
        }

        let fragments = self.split_lines_within_budget(&header, source.trim(), 1);
        let mut out = Vec::with_capacity(fragments.len());
        for (frag_code, start_line, end_line) in fragments {
            let frag_header = self.build_header(
                file_path,
                module_path,
                language,
                "",
                None,
                None,
                None,
                start_line,
                end_line,
            );
            let combined = combine_header_and_code(&frag_header, &frag_code);
            let bounded = if self.counter.count(&combined) > self.config.max_tokens {
                self.counter.truncate(&combined, self.config.max_tokens)
            } else {
                combined
            };
            out.push(self.build_chunk(
                out.len(),
                file_path,
                module_path,
                language,
                None,
                None,
                None,
                start_line,
                end_line,
                0,
                source.len(),
                bounded,
                frag_code,
            ));
        }
        out
    }

    /// Split source text along newline boundaries with line-level token overlap.
    /// Returns `(fragment_text, start_line, end_line)` tuples.
    fn split_lines_within_budget(
        &self,
        header: &str,
        code: &str,
        base_start_line: usize,
    ) -> Vec<(String, usize, usize)> {
        let header_tokens = self.counter.count(header);
        let body_budget = self
            .config
            .max_tokens
            .saturating_sub(header_tokens + 2)
            .max(1);
        let effective_overlap = self
            .config
            .overlap_tokens
            .min(body_budget.saturating_sub(1));

        let lines: Vec<&str> = code.lines().collect();
        if lines.is_empty() {
            return Vec::new();
        }

        let mut results: Vec<(String, usize, usize)> = Vec::new();
        let mut idx = 0usize;

        while idx < lines.len() {
            // Handle a single ultra-long line via UTF-8-safe BPE token windowing
            let single_line_full = combine_header_and_code(header, lines[idx]);
            if self.counter.count(&single_line_full) > self.config.max_tokens {
                let line_num = base_start_line + idx;
                for piece in
                    self.counter
                        .split_by_tokens(lines[idx], body_budget, effective_overlap)
                {
                    results.push((piece, line_num, line_num));
                }
                idx += 1;
                continue;
            }

            let start_idx = idx;
            let mut buf = String::from(lines[idx]);
            idx += 1;

            while idx < lines.len() {
                let candidate = format!("{buf}\n{}", lines[idx]);
                let candidate_full = combine_header_and_code(header, &candidate);
                if self.counter.count(&candidate_full) <= self.config.max_tokens {
                    buf = candidate;
                    idx += 1;
                } else {
                    break;
                }
            }

            let end_idx = idx - 1;
            results.push((buf, base_start_line + start_idx, base_start_line + end_idx));

            if idx >= lines.len() {
                break;
            }

            // Rewind by trailing lines that fit within `effective_overlap`
            if effective_overlap > 0 {
                let mut overlap_idx = idx;
                let mut overlap_buf = String::new();
                while overlap_idx > start_idx + 1 {
                    let prev_line = lines[overlap_idx - 1];
                    let next_buf = if overlap_buf.is_empty() {
                        prev_line.to_string()
                    } else {
                        format!("{prev_line}\n{overlap_buf}")
                    };
                    if self.counter.count(&next_buf) <= effective_overlap {
                        overlap_buf = next_buf;
                        overlap_idx -= 1;
                    } else {
                        break;
                    }
                }
                idx = overlap_idx;
            }
        }

        results
    }

    #[allow(clippy::too_many_arguments)]
    fn build_header(
        &self,
        file_path: &str,
        module_path: &str,
        language: SupportedLanguage,
        imports_block: &str,
        symbol_name: Option<&str>,
        parent_symbol: Option<&str>,
        symbol_kind: Option<SymbolKind>,
        start_line: usize,
        end_line: usize,
    ) -> String {
        if !self.config.include_file_header {
            return String::new();
        }

        let comment = comment_prefix(language);
        let mut lines = Vec::new();
        lines.push(format!(
            "{comment} File: {file_path} | Module: {module_path} | Lang: {}",
            language.as_str()
        ));

        if let Some(name) = symbol_name {
            let kind_str = symbol_kind.map(symbol_kind_label).unwrap_or("symbol");
            let qualified = match parent_symbol {
                Some(parent) => format!("{parent}::{name}"),
                None => name.to_string(),
            };
            lines.push(format!(
                "{comment} Symbol: {kind_str} {qualified} (L{start_line}-L{end_line})"
            ));
        } else {
            lines.push(format!("{comment} Lines: L{start_line}-L{end_line}"));
        }

        let mut header = lines.join("\n");
        if !imports_block.is_empty() {
            header.push('\n');
            header.push_str(imports_block);
        }
        header
    }

    #[allow(clippy::too_many_arguments)]
    fn build_chunk(
        &self,
        chunk_index: usize,
        file_path: &str,
        module_path: &str,
        language: SupportedLanguage,
        symbol_name: Option<String>,
        parent_symbol: Option<String>,
        symbol_kind: Option<SymbolKind>,
        start_line: usize,
        end_line: usize,
        start_byte: usize,
        end_byte: usize,
        content: String,
        raw_code: String,
    ) -> CodeChunk {
        let token_count = self.counter.count(&content);
        let content_hash = hash_text(&content);
        CodeChunk {
            chunk_index,
            file_path: file_path.to_string(),
            module_path: module_path.to_string(),
            language,
            symbol_name,
            parent_symbol,
            symbol_kind,
            start_line,
            end_line,
            start_byte,
            end_byte,
            content,
            raw_code,
            token_count,
            content_hash,
        }
    }
}

enum SymbolPlanItem<'a> {
    Standalone(&'a CodeSymbol),
    Container {
        container: &'a CodeSymbol,
        children: Vec<&'a CodeSymbol>,
    },
}

fn is_container_kind(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Impl | SymbolKind::Trait | SymbolKind::Class
    )
}

/// Group container symbols (`Impl`, `Trait`, `Class`) with their nested child methods
/// so chunks never duplicate the same method body twice.
fn build_symbol_plan(symbols: &[CodeSymbol]) -> Vec<SymbolPlanItem<'_>> {
    let mut owned_by_container = vec![false; symbols.len()];

    // Mark symbols strictly nested inside a container's byte span
    for (i, sym) in symbols.iter().enumerate() {
        if !is_container_kind(sym.kind) {
            continue;
        }
        for (j, other) in symbols.iter().enumerate() {
            if i != j
                && other.start_byte >= sym.start_byte
                && other.end_byte <= sym.end_byte
                && (other.start_byte > sym.start_byte || other.end_byte < sym.end_byte)
            {
                owned_by_container[j] = true;
            }
        }
    }

    let mut plan = Vec::new();
    for (i, sym) in symbols.iter().enumerate() {
        if owned_by_container[i] {
            continue;
        }
        if is_container_kind(sym.kind) {
            let children: Vec<&CodeSymbol> = symbols
                .iter()
                .enumerate()
                .filter(|(j, other)| {
                    *j != i
                        && owned_by_container[*j]
                        && other.start_byte >= sym.start_byte
                        && other.end_byte <= sym.end_byte
                })
                .map(|(_, other)| other)
                .collect();
            plan.push(SymbolPlanItem::Container {
                container: sym,
                children,
            });
        } else {
            plan.push(SymbolPlanItem::Standalone(sym));
        }
    }

    plan
}

fn comment_prefix(language: SupportedLanguage) -> &'static str {
    match language {
        SupportedLanguage::Python => "#",
        SupportedLanguage::Rust | SupportedLanguage::TypeScript | SupportedLanguage::Tsx => "//",
    }
}

fn symbol_kind_label(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "fn",
        SymbolKind::Method => "method",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Trait => "trait",
        SymbolKind::Impl => "impl",
        SymbolKind::Class => "class",
        SymbolKind::Interface => "interface",
        SymbolKind::TypeAlias => "type",
    }
}

fn combine_header_and_code(header: &str, code: &str) -> String {
    if header.is_empty() {
        code.to_string()
    } else {
        format!("{header}\n\n{code}")
    }
}

/// Include leading doc comments above the symbol when they are outside `symbol.source`
/// (Rust & TypeScript doc comments precede the AST item node, whereas Python docstrings live inside).
fn render_symbol_with_doc(symbol: &CodeSymbol, language: SupportedLanguage) -> String {
    let Some(doc) = &symbol.docstring else {
        return symbol.source.clone();
    };
    if language == SupportedLanguage::Python || symbol.source.contains(doc) {
        return symbol.source.clone();
    }
    let doc_lines: String = doc
        .lines()
        .map(|line| format!("/// {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{doc_lines}\n{}", symbol.source)
}

/// Derive a clean logical module path from a source file path.
pub fn derive_module_path(file_path: &str, language: SupportedLanguage) -> String {
    let normalized = file_path.replace('\\', "/");
    let trimmed = normalized.trim_start_matches("./").trim_start_matches('/');
    let without_ext = match trimmed.rsplit_once('.') {
        Some((stem, _)) => stem,
        None => trimmed,
    };

    let parts: Vec<&str> = without_ext
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != "src")
        .collect();

    if parts.is_empty() {
        return without_ext.to_string();
    }

    match language {
        SupportedLanguage::Rust => {
            let mut filtered = parts;
            if matches!(filtered.last().copied(), Some("mod" | "lib" | "main"))
                && filtered.len() > 1
            {
                filtered.pop();
            }
            filtered.join("::")
        }
        SupportedLanguage::Python => {
            let mut filtered = parts;
            if filtered.last().copied() == Some("__init__") && filtered.len() > 1 {
                filtered.pop();
            }
            filtered.join(".")
        }
        SupportedLanguage::TypeScript | SupportedLanguage::Tsx => {
            let mut filtered = parts;
            if filtered.last().copied() == Some("index") && filtered.len() > 1 {
                filtered.pop();
            }
            filtered.join("/")
        }
    }
}

/// Extract top-of-file import/module declarations bounded by `max_tokens`.
fn extract_imports_header(
    source: &str,
    language: SupportedLanguage,
    counter: &TokenCounter,
    max_tokens: usize,
) -> String {
    if max_tokens == 0 {
        return String::new();
    }

    let mut import_lines = Vec::new();
    for line in source.lines().take(60) {
        let trimmed = line.trim();
        let is_import = match language {
            SupportedLanguage::Rust => {
                trimmed.starts_with("use ")
                    || trimmed.starts_with("pub use ")
                    || trimmed.starts_with("mod ")
                    || trimmed.starts_with("pub mod ")
            }
            SupportedLanguage::TypeScript | SupportedLanguage::Tsx => {
                trimmed.starts_with("import ")
            }
            SupportedLanguage::Python => {
                trimmed.starts_with("import ") || trimmed.starts_with("from ")
            }
        };
        if is_import {
            let candidate = if import_lines.is_empty() {
                trimmed.to_string()
            } else {
                format!("{}\n{trimmed}", import_lines.join("\n"))
            };
            if counter.count(&candidate) <= max_tokens {
                import_lines.push(trimmed);
            } else {
                break;
            }
        }
    }

    import_lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_module_paths() {
        assert_eq!(
            derive_module_path("crates/parser/src/code_chunker.rs", SupportedLanguage::Rust),
            "crates::parser::code_chunker"
        );
        assert_eq!(
            derive_module_path("crates/core/src/lib.rs", SupportedLanguage::Rust),
            "crates::core"
        );
        assert_eq!(
            derive_module_path("web/src/lib/api.ts", SupportedLanguage::TypeScript),
            "web/lib/api"
        );
        assert_eq!(
            derive_module_path("rag/engine/retriever.py", SupportedLanguage::Python),
            "rag.engine.retriever"
        );
    }

    #[test]
    fn test_rust_functions_and_structs_stay_intact_with_module_header() {
        let src = r#"use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// Represents a scored retrieval candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hit {
    pub id: String,
    pub score: f32,
}

/// Fuse rankings using Reciprocal Rank Fusion.
pub fn rrf_score(rank: usize, k: f32) -> f32 {
    1.0 / (k + rank as f32)
}
"#;

        let chunker = CodeChunker::new(CodeChunkConfig {
            max_tokens: 256,
            overlap_tokens: 32,
            ..Default::default()
        });

        let chunks = chunker
            .chunk_source("crates/core/src/rrf.rs", src, SupportedLanguage::Rust)
            .expect("chunking rust source should succeed");

        assert_eq!(chunks.len(), 2);

        // Chunk 0: Hit struct intact with header, imports, and docstring
        assert_eq!(chunks[0].symbol_name.as_deref(), Some("Hit"));
        assert_eq!(chunks[0].symbol_kind, Some(SymbolKind::Struct));
        assert_eq!(chunks[0].module_path, "crates::core::rrf");
        assert!(chunks[0].content.contains("File: crates/core/src/rrf.rs"));
        assert!(chunks[0].content.contains("Module: crates::core::rrf"));
        assert!(chunks[0].content.contains("use std::collections::HashMap;"));
        assert!(
            chunks[0]
                .content
                .contains("Represents a scored retrieval candidate.")
        );
        assert!(chunks[0].content.contains("pub struct Hit"));

        // Chunk 1: rrf_score function intact
        assert_eq!(chunks[1].symbol_name.as_deref(), Some("rrf_score"));
        assert_eq!(chunks[1].symbol_kind, Some(SymbolKind::Function));
        assert!(
            chunks[1]
                .content
                .contains("pub fn rrf_score(rank: usize, k: f32) -> f32")
        );

        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.chunk_index, i);
            assert_eq!(c.token_count, chunker.counter().count(&c.content));
            assert!(c.token_count <= 256);
            assert_eq!(c.content_hash.len(), 64);
        }
    }

    #[test]
    fn test_large_impl_block_decomposes_along_method_boundaries() {
        let src = r#"pub struct SearchEngine;

impl SearchEngine {
    /// First retrieval step: dense vector lookup.
    pub fn vector_search(&self, query: &str, top_k: usize) -> Vec<String> {
        let mut hits = Vec::with_capacity(top_k);
        for i in 0..top_k {
            hits.push(format!("vec:{query}:{i}"));
        }
        hits
    }

    /// Second retrieval step: BM25 full-text lookup.
    pub fn keyword_search(&self, query: &str, top_k: usize) -> Vec<String> {
        let mut hits = Vec::with_capacity(top_k);
        for i in 0..top_k {
            hits.push(format!("bm25:{query}:{i}"));
        }
        hits
    }
}
"#;

        // Small max_tokens so the entire `impl` block does not fit in one chunk,
        // forcing clean AST method-level decomposition without splitting either method body.
        let chunker = CodeChunker::new(CodeChunkConfig {
            max_tokens: 130,
            overlap_tokens: 16,
            include_imports: false,
            ..Default::default()
        });

        let chunks = chunker
            .chunk_source("crates/core/src/search.rs", src, SupportedLanguage::Rust)
            .expect("chunking should succeed");

        // Expect: Struct(SearchEngine), Method(vector_search), Method(keyword_search)
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].symbol_name.as_deref(), Some("SearchEngine"));
        assert_eq!(chunks[1].symbol_name.as_deref(), Some("vector_search"));
        assert_eq!(chunks[1].parent_symbol.as_deref(), Some("SearchEngine"));
        assert!(chunks[1].content.contains("SearchEngine::vector_search"));
        assert_eq!(chunks[2].symbol_name.as_deref(), Some("keyword_search"));
        assert_eq!(chunks[2].parent_symbol.as_deref(), Some("SearchEngine"));

        for c in &chunks {
            assert!(c.token_count <= 130, "exceeded budget: {}", c.token_count);
        }
    }

    #[test]
    fn test_python_and_typescript_ast_chunking() {
        let py_src = r#"from typing import List

class HybridRetriever:
    """Combines vector search and keyword search."""

    def retrieve(self, query: str) -> List[str]:
        """Fetch top documents."""
        return [query]
"#;

        let chunker = CodeChunker::default();
        let py_chunks = chunker
            .chunk_source("rag/retriever.py", py_src, SupportedLanguage::Python)
            .expect("py chunking");
        // Small class fits in one intact chunk with no duplicate method chunk
        assert_eq!(py_chunks.len(), 1);
        assert_eq!(py_chunks[0].symbol_name.as_deref(), Some("HybridRetriever"));
        assert!(py_chunks[0].content.starts_with("# File: rag/retriever.py"));
        assert!(py_chunks[0].content.contains("from typing import List"));

        let ts_src = r#"import { api } from "@/lib/api";

/** Run document parse request. */
export async function parseDoc(name: string, text: string) {
    return api.parseDocument(name, text);
}
"#;
        let ts_chunks = chunker
            .chunk_source(
                "web/src/hooks/useParse.ts",
                ts_src,
                SupportedLanguage::TypeScript,
            )
            .expect("ts chunking");
        assert_eq!(ts_chunks.len(), 1);
        assert_eq!(ts_chunks[0].symbol_name.as_deref(), Some("parseDoc"));
        assert_eq!(ts_chunks[0].module_path, "web/hooks/useParse");
        assert!(ts_chunks[0].content.contains("import { api }"));
    }

    #[test]
    fn test_oversized_single_function_splits_cleanly_by_lines_with_overlap() {
        let mut body =
            String::from("pub fn huge_pipeline(input: &[i32]) -> i32 {\n    let mut acc = 0;\n");
        for i in 0..40 {
            body.push_str(&format!(
                "    acc += input.get({i}).copied().unwrap_or(0) * {};\n",
                i + 1
            ));
        }
        body.push_str("    acc\n}\n");

        let chunker = CodeChunker::new(CodeChunkConfig {
            max_tokens: 90,
            overlap_tokens: 18,
            include_imports: false,
            ..Default::default()
        });

        let chunks = chunker
            .chunk_source("crates/core/src/huge.rs", &body, SupportedLanguage::Rust)
            .expect("oversized fn chunking");

        assert!(chunks.len() >= 3);
        for c in &chunks {
            assert_eq!(c.symbol_name.as_deref(), Some("huge_pipeline"));
            assert!(
                c.token_count <= 90,
                "chunk {} exceeded 90 tokens: {}",
                c.chunk_index,
                c.token_count
            );
            assert!(c.start_line <= c.end_line);
        }
    }
}
