use std::sync::OnceLock;

use jsonschema::Validator;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::chunker::{ChunkConfig, SemanticChunker};
use crate::code::{SupportedLanguage, SymbolKind};
use crate::code_chunker::{CodeChunkConfig, CodeChunker};
use crate::error::ParseError;
use crate::fingerprint::hash_text;
use crate::scanner::{DocumentContent, ParsedDocument, RepositoryScanner};
use crate::tokenizer::{TokenCounter, TokenizerEncoding};

static DOCUMENT_CHUNK_VALIDATOR: OnceLock<Validator> = OnceLock::new();

/// High-level source document classification for a RAG chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind {
    Markdown,
    Code,
    PlainText,
}

/// Half-open UTF-8 byte offset span `[start, end)` within the original source document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

/// Inclusive 1-indexed line range `start..=end` within the original source document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LineRange {
    /// 1-indexed starting line number.
    #[schemars(range(min = 1))]
    pub start: usize,
    /// 1-indexed ending line number (inclusive).
    #[schemars(range(min = 1))]
    pub end: usize,
}

/// Traceable provenance metadata attached to every [`DocumentChunk`] for RAG citations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChunkMetadata {
    /// Normalized repository-relative path (forward slashes).
    pub file_path: String,
    pub kind: ChunkKind,
    /// Programming language for code chunks.
    pub language: Option<SupportedLanguage>,
    /// Derived logical module path (e.g. `crates::parser::code`).
    pub module_path: Option<String>,
    /// Heading hierarchy path for Markdown chunks (e.g. `["Doc", "Section 2", "Step 1"]`).
    pub breadcrumbs: Vec<String>,
    /// Heading depth `1..=6` for Markdown chunks (or `0` for preamble).
    #[schemars(range(min = 0, max = 6))]
    pub heading_level: Option<u8>,
    /// Primary AST symbol name (function, struct, class, etc.) for code chunks.
    pub symbol_name: Option<String>,
    /// Enclosing container symbol (`impl`, `trait`, `class`) when chunking a member method.
    pub parent_symbol: Option<String>,
    /// AST symbol classification for code chunks.
    pub symbol_kind: Option<SymbolKind>,
    /// Byte offset span in the original document.
    pub byte_range: ByteRange,
    /// 1-indexed inclusive line range in the original document.
    pub line_range: Option<LineRange>,
    /// BPE tokenizer vocabulary used to compute `token_count`.
    pub encoding: TokenizerEncoding,
}

/// Strongly-typed, schema-validated unit of indexed text ready for embedding and retrieval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocumentChunk {
    /// Deterministic Blake3 identifier derived from `(file_id, chunk_index, content_hash)`.
    pub chunk_id: String,
    /// Identifier of the parent file (e.g. document UUID or normalized path/fingerprint).
    pub file_id: String,
    /// Zero-based sequential chunk index inside the parent file.
    pub chunk_index: usize,
    /// Embedding-ready text including breadcrumb or file/module context header.
    pub content: String,
    /// Original unprefixed content snippet for exact source highlighting.
    pub raw_content: String,
    /// Exact BPE token count of `content`.
    pub token_count: usize,
    /// 64-character lowercase hex Blake3 content hash of `content`.
    pub content_hash: String,
    /// Structured provenance metadata (file path, breadcrumbs, offsets, AST symbol).
    pub metadata: ChunkMetadata,
}

impl DocumentChunk {
    /// Generate the canonical JSON Schema for [`DocumentChunk`].
    pub fn json_schema() -> Value {
        let schema = schemars::schema_for!(DocumentChunk);
        serde_json::to_value(&schema).expect("schemars RootSchema always serializes to JSON")
    }

    /// Validate an arbitrary JSON [`Value`] against [`DocumentChunk`]'s JSON Schema
    /// plus domain invariants (byte/line ordering and Blake3 digest length).
    pub fn validate_value(value: &Value) -> Result<(), ParseError> {
        let validator = DOCUMENT_CHUNK_VALIDATOR.get_or_init(|| {
            let schema = Self::json_schema();
            jsonschema::options()
                .should_validate_formats(true)
                .build(&schema)
                .expect("DocumentChunk schema must always compile")
        });

        if let Err(err) = validator.validate(value) {
            return Err(ParseError::SchemaValidation(format!(
                "{err} at {}",
                err.instance_path()
            )));
        }

        Ok(())
    }

    /// Parse and validate a JSON string into a strongly-typed [`DocumentChunk`].
    pub fn from_json_validated(raw_json: &str) -> Result<Self, ParseError> {
        let value: Value = serde_json::from_str(raw_json)
            .map_err(|e| ParseError::SchemaValidation(format!("invalid JSON syntax: {e}")))?;
        Self::validate_value(&value)?;

        let chunk: Self = serde_json::from_value(value)
            .map_err(|e| ParseError::SchemaValidation(format!("deserialization failed: {e}")))?;
        chunk.validate_invariants()?;
        Ok(chunk)
    }

    /// Verify semantic invariants that go beyond JSON Schema structural types.
    pub fn validate_invariants(&self) -> Result<(), ParseError> {
        if self.chunk_id.trim().is_empty() {
            return Err(ParseError::SchemaValidation(
                "chunk_id cannot be empty".to_string(),
            ));
        }
        if self.file_id.trim().is_empty() {
            return Err(ParseError::SchemaValidation(
                "file_id cannot be empty".to_string(),
            ));
        }
        if self.content.trim().is_empty() {
            return Err(ParseError::SchemaValidation(
                "content cannot be empty".to_string(),
            ));
        }
        if self.content_hash.len() != 64
            || !self.content_hash.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(ParseError::SchemaValidation(
                "content_hash must be a 64-character hex Blake3 digest".to_string(),
            ));
        }
        if self.metadata.byte_range.start > self.metadata.byte_range.end {
            return Err(ParseError::SchemaValidation(format!(
                "byte_range.start ({}) exceeds byte_range.end ({})",
                self.metadata.byte_range.start, self.metadata.byte_range.end
            )));
        }
        if let Some(lines) = self.metadata.line_range
            && (lines.start == 0 || lines.start > lines.end)
        {
            return Err(ParseError::SchemaValidation(format!(
                "invalid line_range: {}..={}",
                lines.start, lines.end
            )));
        }
        Ok(())
    }
}

/// Unified configuration for chunking any supported document type (Markdown, Code, PlainText).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocumentChunkerConfig {
    pub max_tokens: usize,
    pub overlap_tokens: usize,
    pub min_chunk_tokens: usize,
    pub encoding: TokenizerEncoding,
    pub include_context_headers: bool,
}

impl Default for DocumentChunkerConfig {
    fn default() -> Self {
        Self {
            max_tokens: 512,
            overlap_tokens: 64,
            min_chunk_tokens: 48,
            encoding: TokenizerEncoding::Cl100kBase,
            include_context_headers: true,
        }
    }
}

/// Facade chunker that routes Markdown, source code, and plain text to the specialized
/// chunkers and outputs uniform, schema-compliant [`DocumentChunk`] records.
#[derive(Debug, Clone)]
pub struct DocumentChunker {
    config: DocumentChunkerConfig,
    markdown_chunker: SemanticChunker,
    code_chunker: CodeChunker,
    counter: TokenCounter,
}

impl Default for DocumentChunker {
    fn default() -> Self {
        Self::new(DocumentChunkerConfig::default())
    }
}

impl DocumentChunker {
    pub fn new(config: DocumentChunkerConfig) -> Self {
        let max_tokens = config.max_tokens.max(1);
        let overlap_tokens = config.overlap_tokens.min(max_tokens.saturating_sub(1));
        let min_chunk_tokens = config.min_chunk_tokens.min(max_tokens);
        let normalized = DocumentChunkerConfig {
            max_tokens,
            overlap_tokens,
            min_chunk_tokens,
            ..config
        };

        let markdown_chunker = SemanticChunker::new(ChunkConfig {
            max_tokens: normalized.max_tokens,
            overlap_tokens: normalized.overlap_tokens,
            min_chunk_tokens: normalized.min_chunk_tokens,
            encoding: normalized.encoding,
            include_breadcrumbs: normalized.include_context_headers,
        });

        let code_chunker = CodeChunker::new(CodeChunkConfig {
            max_tokens: normalized.max_tokens,
            overlap_tokens: normalized.overlap_tokens,
            encoding: normalized.encoding,
            include_file_header: normalized.include_context_headers,
            include_imports: normalized.include_context_headers,
            max_import_tokens: 96,
        });

        let counter = TokenCounter::new(normalized.encoding);

        Self {
            config: normalized,
            markdown_chunker,
            code_chunker,
            counter,
        }
    }

    pub fn config(&self) -> &DocumentChunkerConfig {
        &self.config
    }

    /// Parse raw file text by filename extension and produce uniform [`DocumentChunk`]s.
    pub fn chunk_file(
        &self,
        file_id: &str,
        filename: &str,
        raw_source: &str,
    ) -> Result<Vec<DocumentChunk>, ParseError> {
        let parsed = RepositoryScanner::parse_content(filename, raw_source)?;
        Ok(self.chunk_document(file_id, &parsed, raw_source))
    }

    /// Convert an already-parsed [`ParsedDocument`] and its raw UTF-8 source into [`DocumentChunk`]s.
    pub fn chunk_document(
        &self,
        file_id: &str,
        doc: &ParsedDocument,
        raw_source: &str,
    ) -> Vec<DocumentChunk> {
        match &doc.content {
            DocumentContent::Markdown(md) => {
                let chunks = self.markdown_chunker.chunk_markdown(md);
                let mut locator = OffsetLocator::new(raw_source);
                chunks
                    .into_iter()
                    .map(|chunk| {
                        let (byte_range, line_range) = locator.locate(&chunk.raw_content);
                        let chunk_id =
                            derive_chunk_id(file_id, chunk.chunk_index, &chunk.content_hash);
                        DocumentChunk {
                            chunk_id,
                            file_id: file_id.to_string(),
                            chunk_index: chunk.chunk_index,
                            content: chunk.content,
                            raw_content: chunk.raw_content,
                            token_count: chunk.token_count,
                            content_hash: chunk.content_hash,
                            metadata: ChunkMetadata {
                                file_path: doc.path.clone(),
                                kind: ChunkKind::Markdown,
                                language: None,
                                module_path: None,
                                breadcrumbs: chunk.breadcrumbs,
                                heading_level: Some(chunk.heading_level),
                                symbol_name: None,
                                parent_symbol: None,
                                symbol_kind: None,
                                byte_range,
                                line_range,
                                encoding: self.config.encoding,
                            },
                        }
                    })
                    .collect()
            }
            DocumentContent::Code(parsed_code) => {
                let chunks = self
                    .code_chunker
                    .chunk_parsed(&doc.path, raw_source, parsed_code);
                chunks
                    .into_iter()
                    .map(|chunk| {
                        let chunk_id =
                            derive_chunk_id(file_id, chunk.chunk_index, &chunk.content_hash);
                        DocumentChunk {
                            chunk_id,
                            file_id: file_id.to_string(),
                            chunk_index: chunk.chunk_index,
                            content: chunk.content,
                            raw_content: chunk.raw_code,
                            token_count: chunk.token_count,
                            content_hash: chunk.content_hash,
                            metadata: ChunkMetadata {
                                file_path: chunk.file_path,
                                kind: ChunkKind::Code,
                                language: Some(chunk.language),
                                module_path: Some(chunk.module_path),
                                breadcrumbs: Vec::new(),
                                heading_level: None,
                                symbol_name: chunk.symbol_name,
                                parent_symbol: chunk.parent_symbol,
                                symbol_kind: chunk.symbol_kind,
                                byte_range: ByteRange {
                                    start: chunk.start_byte,
                                    end: chunk.end_byte,
                                },
                                line_range: Some(LineRange {
                                    start: chunk.start_line,
                                    end: chunk.end_line,
                                }),
                                encoding: self.config.encoding,
                            },
                        }
                    })
                    .collect()
            }
            DocumentContent::PlainText { content, .. } => {
                let pieces = self.counter.split_by_tokens(
                    content.trim(),
                    self.config.max_tokens,
                    self.config.overlap_tokens,
                );
                let mut locator = OffsetLocator::new(raw_source);
                pieces
                    .into_iter()
                    .enumerate()
                    .map(|(chunk_index, text)| {
                        let (byte_range, line_range) = locator.locate(&text);
                        let token_count = self.counter.count(&text);
                        let content_hash = hash_text(&text);
                        let chunk_id = derive_chunk_id(file_id, chunk_index, &content_hash);
                        DocumentChunk {
                            chunk_id,
                            file_id: file_id.to_string(),
                            chunk_index,
                            content: text.clone(),
                            raw_content: text,
                            token_count,
                            content_hash,
                            metadata: ChunkMetadata {
                                file_path: doc.path.clone(),
                                kind: ChunkKind::PlainText,
                                language: None,
                                module_path: None,
                                breadcrumbs: Vec::new(),
                                heading_level: None,
                                symbol_name: None,
                                parent_symbol: None,
                                symbol_kind: None,
                                byte_range,
                                line_range,
                                encoding: self.config.encoding,
                            },
                        }
                    })
                    .collect()
            }
        }
    }
}

fn derive_chunk_id(file_id: &str, chunk_index: usize, content_hash: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(file_id.as_bytes());
    hasher.update(b":");
    hasher.update(chunk_index.to_string().as_bytes());
    hasher.update(b":");
    hasher.update(content_hash.as_bytes());
    hasher.finalize().to_hex()[..32].to_string()
}

/// Incremental O(N) snippet byte-offset and 1-indexed line-number locator.
struct OffsetLocator<'a> {
    source: &'a str,
    cursor_byte: usize,
    cursor_line: usize,
}

impl<'a> OffsetLocator<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            cursor_byte: 0,
            cursor_line: 1,
        }
    }

    fn locate(&mut self, snippet: &str) -> (ByteRange, Option<LineRange>) {
        if self.source.is_empty() || snippet.is_empty() {
            return (ByteRange { start: 0, end: 0 }, None);
        }

        let search_slice = self.source.get(self.cursor_byte..).unwrap_or(self.source);
        let anchor = snippet
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or(snippet);

        // Search forward from cursor using the first non-empty line anchor for O(1) locality
        let found_start = search_slice
            .find(anchor)
            .map(|rel| self.cursor_byte + rel)
            .or_else(|| self.source.find(anchor));

        match found_start {
            Some(start) => {
                let end = (start + snippet.len()).min(self.source.len());
                let mut safe_end = end;
                while safe_end < self.source.len() && !self.source.is_char_boundary(safe_end) {
                    safe_end += 1;
                }

                let start_line = if start >= self.cursor_byte {
                    self.cursor_line
                        + self.source[self.cursor_byte..start]
                            .bytes()
                            .filter(|&b| b == b'\n')
                            .count()
                } else {
                    1 + self.source[..start]
                        .bytes()
                        .filter(|&b| b == b'\n')
                        .count()
                };

                self.cursor_byte = start.min(self.source.len());
                self.cursor_line = start_line;

                let extra_lines = self.source[start..safe_end]
                    .bytes()
                    .filter(|&b| b == b'\n')
                    .count();
                let end_line = start_line + extra_lines;
                (
                    ByteRange {
                        start,
                        end: safe_end,
                    },
                    Some(LineRange {
                        start: start_line,
                        end: end_line,
                    }),
                )
            }
            None => (
                ByteRange {
                    start: 0,
                    end: self.source.len(),
                },
                Some(LineRange {
                    start: 1,
                    end: self.source.lines().count().max(1),
                }),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_document_chunk_json_schema_generation_and_roundtrip() {
        let schema = DocumentChunk::json_schema();
        assert_eq!(schema["title"], "DocumentChunk");
        assert!(schema["properties"]["chunk_id"].is_object());
        assert!(schema["properties"]["file_id"].is_object());
        assert!(schema["properties"]["token_count"].is_object());
        assert!(schema["properties"]["content_hash"].is_object());
        assert!(schema["properties"]["metadata"].is_object());

        let chunker = DocumentChunker::default();
        let md =
            "# RAG Core\n\n## Indexing\nUse Blake3 hashes and HNSW indexes for fast retrieval.\n";
        let chunks = chunker
            .chunk_file("doc-001", "docs/rag.md", md)
            .expect("markdown chunking should succeed");
        assert_eq!(chunks.len(), 1);

        let serialized = serde_json::to_string_pretty(&chunks[0]).unwrap();
        let validated = DocumentChunk::from_json_validated(&serialized)
            .expect("serialized DocumentChunk must pass JSON Schema validation");
        assert_eq!(validated, chunks[0]);
        assert_eq!(validated.metadata.kind, ChunkKind::Markdown);
        assert_eq!(validated.metadata.breadcrumbs, vec!["RAG Core", "Indexing"]);
        assert!(validated.metadata.byte_range.end > validated.metadata.byte_range.start);
        assert!(validated.metadata.line_range.is_some());
    }

    #[test]
    fn test_document_chunk_schema_rejects_invalid_payloads() {
        // 1. Unknown top-level field rejected via `deny_unknown_fields` / `additionalProperties: false`
        let chunker = DocumentChunker::default();
        let chunks = chunker
            .chunk_file(
                "file-rs",
                "src/lib.rs",
                "pub fn ping() -> &'static str { \"pong\" }\n",
            )
            .unwrap();
        let mut val = serde_json::to_value(&chunks[0]).unwrap();
        val.as_object_mut()
            .unwrap()
            .insert("unexpected_field".to_string(), Value::Bool(true));

        let err = DocumentChunk::from_json_validated(&val.to_string());
        assert!(matches!(err, Err(ParseError::SchemaValidation(_))));

        // 2. Missing required field rejected
        let mut missing = serde_json::to_value(&chunks[0]).unwrap();
        missing.as_object_mut().unwrap().remove("content_hash");
        assert!(matches!(
            DocumentChunk::from_json_validated(&missing.to_string()),
            Err(ParseError::SchemaValidation(_))
        ));

        // 3. Invalid byte_range ordering rejected by domain invariant check
        let mut bad_range = serde_json::to_value(&chunks[0]).unwrap();
        bad_range["metadata"]["byte_range"]["start"] = Value::from(100);
        bad_range["metadata"]["byte_range"]["end"] = Value::from(10);
        assert!(matches!(
            DocumentChunk::from_json_validated(&bad_range.to_string()),
            Err(ParseError::SchemaValidation(_))
        ));
    }

    #[test]
    fn test_unified_chunker_handles_code_markdown_and_plaintext() {
        let chunker = DocumentChunker::default();

        let code_chunks = chunker
            .chunk_file(
                "file-code",
                "crates/core/src/math.rs",
                "/// Add two numbers.\npub fn add(a: i32, b: i32) -> i32 { a + b }\n",
            )
            .unwrap();
        assert_eq!(code_chunks.len(), 1);
        assert_eq!(code_chunks[0].metadata.kind, ChunkKind::Code);
        assert_eq!(
            code_chunks[0].metadata.language,
            Some(SupportedLanguage::Rust)
        );
        assert_eq!(code_chunks[0].metadata.symbol_name.as_deref(), Some("add"));
        assert_eq!(
            code_chunks[0].metadata.module_path.as_deref(),
            Some("crates::core::math")
        );
        assert!(code_chunks[0].validate_invariants().is_ok());

        let txt_chunks = chunker
            .chunk_file("file-txt", "notes.txt", "Plain text note for RAG indexing.")
            .unwrap();
        assert_eq!(txt_chunks.len(), 1);
        assert_eq!(txt_chunks[0].metadata.kind, ChunkKind::PlainText);
        assert!(txt_chunks[0].validate_invariants().is_ok());
    }
}
