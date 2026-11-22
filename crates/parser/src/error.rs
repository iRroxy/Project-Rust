use thiserror::Error;

/// Unified error type for `ai-parser`.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("unsupported file extension or language: {0}")]
    UnsupportedLanguage(String),

    #[error("failed to initialize tree-sitter grammar: {0}")]
    LanguageInit(String),

    #[error("tree-sitter failed to produce a syntax tree")]
    ParseFailed,

    #[error("BPE tokenizer decode error: {0}")]
    TokenizerDecode(String),

    #[error("chunk schema validation failed: {0}")]
    SchemaValidation(String),

    #[error("chunking pipeline error: {0}")]
    Pipeline(String),

    #[error("I/O error while reading source: {0}")]
    Io(#[from] std::io::Error),
}
