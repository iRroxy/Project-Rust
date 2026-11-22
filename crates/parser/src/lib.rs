pub mod chunker;
pub mod code;
pub mod code_chunker;
pub mod document_chunk;
pub mod error;
pub mod fingerprint;
pub mod markdown;
pub mod scanner;
pub mod tokenizer;

pub use chunker::{ChunkConfig, SemanticChunk, SemanticChunker};
pub use code::{CodeParser, CodeSymbol, ParsedCode, SupportedLanguage, SymbolKind};
pub use code_chunker::{CodeChunk, CodeChunkConfig, CodeChunker, derive_module_path};
pub use document_chunk::{
    ByteRange, ChunkKind, ChunkMetadata, DocumentChunk, DocumentChunker, DocumentChunkerConfig,
    LineRange,
};
pub use error::ParseError;
pub use fingerprint::{
    DiffCalculator, DiffSummary, DocumentFingerprint, UnitFingerprint, hash_bytes, hash_text,
};
pub use markdown::{CodeBlock, MarkdownParser, MarkdownSection, ParsedMarkdown};
pub use scanner::{DocumentContent, ParsedDocument, RepositoryScanner, ScanConfig, ScanReport};
pub use tokenizer::{TokenCounter, TokenizerEncoding};
