pub mod chunker;
pub mod code;
pub mod error;
pub mod fingerprint;
pub mod markdown;
pub mod scanner;
pub mod tokenizer;

pub use chunker::{ChunkConfig, SemanticChunk, SemanticChunker};
pub use code::{CodeParser, CodeSymbol, ParsedCode, SupportedLanguage, SymbolKind};
pub use error::ParseError;
pub use fingerprint::{
    DiffCalculator, DiffSummary, DocumentFingerprint, UnitFingerprint, hash_bytes, hash_text,
};
pub use markdown::{CodeBlock, MarkdownParser, MarkdownSection, ParsedMarkdown};
pub use scanner::{DocumentContent, ParsedDocument, RepositoryScanner, ScanConfig, ScanReport};
pub use tokenizer::{TokenCounter, TokenizerEncoding};
