use serde::{Deserialize, Serialize};
use tree_sitter::{Language, Node, Parser};

use crate::error::ParseError;

/// Programming languages supported by the AST code parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SupportedLanguage {
    Rust,
    TypeScript,
    Tsx,
    Python,
}

impl SupportedLanguage {
    /// Detect language from a file extension (without leading dot, case-insensitive).
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "rs" => Some(Self::Rust),
            "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "py" | "pyi" => Some(Self::Python),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::Python => "python",
        }
    }

    fn tree_sitter_language(&self) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
        }
    }
}

/// Kind of structural symbol extracted from an AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Impl,
    Class,
    Interface,
    TypeAlias,
}

/// A single extracted AST symbol with location, docstring, and source slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeSymbol {
    pub name: String,
    pub kind: SymbolKind,
    /// 1-indexed starting line number.
    pub start_line: usize,
    /// 1-indexed ending line number (inclusive).
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub docstring: Option<String>,
    pub signature: String,
    pub source: String,
}

/// Result of parsing a source code file via `tree-sitter`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedCode {
    pub language: SupportedLanguage,
    pub symbols: Vec<CodeSymbol>,
    pub total_lines: usize,
    pub total_chars: usize,
}

pub struct CodeParser;

impl CodeParser {
    /// Parse source code using `tree-sitter` and extract top-level & member AST symbols.
    pub fn parse(source: &str, language: SupportedLanguage) -> Result<ParsedCode, ParseError> {
        let mut parser = Parser::new();
        parser
            .set_language(&language.tree_sitter_language())
            .map_err(|e| ParseError::LanguageInit(e.to_string()))?;

        let tree = parser.parse(source, None).ok_or(ParseError::ParseFailed)?;
        let root = tree.root_node();

        let mut symbols = Vec::new();
        let bytes = source.as_bytes();
        extract_symbols(root, bytes, language, false, &mut symbols);

        let total_lines = if source.is_empty() {
            0
        } else {
            source.lines().count()
        };
        let total_chars = source.chars().count();

        Ok(ParsedCode {
            language,
            symbols,
            total_lines,
            total_chars,
        })
    }
}

fn extract_symbols(
    node: Node<'_>,
    source: &[u8],
    language: SupportedLanguage,
    in_container: bool,
    out: &mut Vec<CodeSymbol>,
) {
    match language {
        SupportedLanguage::Rust => extract_rust(node, source, in_container, out),
        SupportedLanguage::TypeScript | SupportedLanguage::Tsx => {
            extract_typescript(node, source, in_container, out)
        }
        SupportedLanguage::Python => extract_python(node, source, in_container, out),
    }
}

fn extract_rust(node: Node<'_>, source: &[u8], in_impl_or_trait: bool, out: &mut Vec<CodeSymbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_item" | "function_signature_item" => {
                if let Some(name) = child_text_by_field(child, "name", source) {
                    let kind = if in_impl_or_trait {
                        SymbolKind::Method
                    } else {
                        SymbolKind::Function
                    };
                    out.push(build_symbol(
                        child,
                        name,
                        kind,
                        extract_leading_comments(child, source, &["line_comment", "block_comment"]),
                        source,
                    ));
                }
            }
            "struct_item" => {
                if let Some(name) = child_text_by_field(child, "name", source) {
                    out.push(build_symbol(
                        child,
                        name,
                        SymbolKind::Struct,
                        extract_leading_comments(child, source, &["line_comment", "block_comment"]),
                        source,
                    ));
                }
            }
            "enum_item" => {
                if let Some(name) = child_text_by_field(child, "name", source) {
                    out.push(build_symbol(
                        child,
                        name,
                        SymbolKind::Enum,
                        extract_leading_comments(child, source, &["line_comment", "block_comment"]),
                        source,
                    ));
                }
            }
            "trait_item" => {
                if let Some(name) = child_text_by_field(child, "name", source) {
                    out.push(build_symbol(
                        child,
                        name,
                        SymbolKind::Trait,
                        extract_leading_comments(child, source, &["line_comment", "block_comment"]),
                        source,
                    ));
                }
                if let Some(body) = child.child_by_field_name("body") {
                    extract_rust(body, source, true, out);
                }
            }
            "impl_item" => {
                let type_name = child_text_by_field(child, "type", source)
                    .unwrap_or_else(|| "anonymous".to_string());
                let trait_name = child_text_by_field(child, "trait", source);
                let name = match trait_name {
                    Some(tr) => format!("{tr} for {type_name}"),
                    None => type_name,
                };
                out.push(build_symbol(
                    child,
                    name,
                    SymbolKind::Impl,
                    extract_leading_comments(child, source, &["line_comment", "block_comment"]),
                    source,
                ));
                if let Some(body) = child.child_by_field_name("body") {
                    extract_rust(body, source, true, out);
                }
            }
            "type_item" => {
                if let Some(name) = child_text_by_field(child, "name", source) {
                    out.push(build_symbol(
                        child,
                        name,
                        SymbolKind::TypeAlias,
                        extract_leading_comments(child, source, &["line_comment", "block_comment"]),
                        source,
                    ));
                }
            }
            "mod_item" => {
                if let Some(body) = child.child_by_field_name("body") {
                    extract_rust(body, source, false, out);
                }
            }
            _ => {}
        }
    }
}

fn extract_typescript(node: Node<'_>, source: &[u8], in_class: bool, out: &mut Vec<CodeSymbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "export_statement" => {
                // Unwrap exported declaration while preserving comments attached to export_statement
                let export_doc = extract_leading_comments(child, source, &["comment"]);
                let mut inner_cursor = child.walk();
                for inner in child.children(&mut inner_cursor) {
                    handle_ts_declaration(inner, export_doc.clone(), source, in_class, out);
                }
            }
            _ => {
                handle_ts_declaration(child, None, source, in_class, out);
            }
        }
    }
}

fn handle_ts_declaration(
    node: Node<'_>,
    inherited_doc: Option<String>,
    source: &[u8],
    in_class: bool,
    out: &mut Vec<CodeSymbol>,
) {
    let doc = inherited_doc.or_else(|| extract_leading_comments(node, source, &["comment"]));

    match node.kind() {
        "function_declaration" => {
            if let Some(name) = child_text_by_field(node, "name", source) {
                out.push(build_symbol(node, name, SymbolKind::Function, doc, source));
            }
        }
        "method_definition" => {
            if let Some(name) = child_text_by_field(node, "name", source) {
                out.push(build_symbol(node, name, SymbolKind::Method, doc, source));
            }
        }
        "class_declaration" => {
            if let Some(name) = child_text_by_field(node, "name", source) {
                out.push(build_symbol(node, name, SymbolKind::Class, doc, source));
            }
            if let Some(body) = node.child_by_field_name("body") {
                extract_typescript(body, source, true, out);
            }
        }
        "interface_declaration" => {
            if let Some(name) = child_text_by_field(node, "name", source) {
                out.push(build_symbol(node, name, SymbolKind::Interface, doc, source));
            }
        }
        "type_alias_declaration" => {
            if let Some(name) = child_text_by_field(node, "name", source) {
                out.push(build_symbol(node, name, SymbolKind::TypeAlias, doc, source));
            }
        }
        "enum_declaration" => {
            if let Some(name) = child_text_by_field(node, "name", source) {
                out.push(build_symbol(node, name, SymbolKind::Enum, doc, source));
            }
        }
        "lexical_declaration" if !in_class => {
            // Capture arrow functions like `export const fetchUser = async (...) => { ... }`
            let mut cursor = node.walk();
            for declarator in node.children(&mut cursor) {
                if declarator.kind() == "variable_declarator"
                    && let Some(value) = declarator.child_by_field_name("value")
                    && matches!(value.kind(), "arrow_function" | "function_expression")
                    && let Some(name) = child_text_by_field(declarator, "name", source)
                {
                    out.push(build_symbol(
                        node,
                        name,
                        SymbolKind::Function,
                        doc.clone(),
                        source,
                    ));
                }
            }
        }
        _ => {}
    }
}

fn extract_python(node: Node<'_>, source: &[u8], in_class: bool, out: &mut Vec<CodeSymbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "decorated_definition" => {
                let mut inner_cursor = child.walk();
                for inner in child.children(&mut inner_cursor) {
                    if matches!(inner.kind(), "function_definition" | "class_definition") {
                        handle_python_definition(inner, child, source, in_class, out);
                    }
                }
            }
            "function_definition" | "class_definition" => {
                handle_python_definition(child, child, source, in_class, out);
            }
            _ => {}
        }
    }
}

fn handle_python_definition(
    def_node: Node<'_>,
    span_node: Node<'_>,
    source: &[u8],
    in_class: bool,
    out: &mut Vec<CodeSymbol>,
) {
    let Some(name) = child_text_by_field(def_node, "name", source) else {
        return;
    };

    let docstring = extract_python_docstring(def_node, source)
        .or_else(|| extract_leading_comments(span_node, source, &["comment"]));

    match def_node.kind() {
        "function_definition" => {
            let kind = if in_class {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            };
            out.push(build_symbol(span_node, name, kind, docstring, source));
        }
        "class_definition" => {
            out.push(build_symbol(
                span_node,
                name,
                SymbolKind::Class,
                docstring,
                source,
            ));
            if let Some(body) = def_node.child_by_field_name("body") {
                extract_python(body, source, true, out);
            }
        }
        _ => {}
    }
}

fn extract_python_docstring(def_node: Node<'_>, source: &[u8]) -> Option<String> {
    let body = def_node.child_by_field_name("body")?;
    let mut cursor = body.walk();
    let first_stmt = body.named_children(&mut cursor).next()?;
    if first_stmt.kind() != "expression_statement" {
        return None;
    }
    let mut expr_cursor = first_stmt.walk();
    let expr = first_stmt.named_children(&mut expr_cursor).next()?;
    if expr.kind() != "string" {
        return None;
    }
    let raw = expr.utf8_text(source).ok()?.trim();
    let trimmed = raw
        .trim_start_matches("\"\"\"")
        .trim_end_matches("\"\"\"")
        .trim_start_matches("'''")
        .trim_end_matches("'''")
        .trim_matches('"')
        .trim_matches('\'')
        .trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn extract_leading_comments(
    node: Node<'_>,
    source: &[u8],
    comment_kinds: &[&str],
) -> Option<String> {
    let mut comments = Vec::new();
    let mut prev = node.prev_sibling();
    let mut expected_line = node.start_position().row;

    while let Some(sib) = prev {
        // Skip attribute items in Rust (e.g. `#[derive(...)]`) between doc comments and the item
        if sib.kind() == "attribute_item" {
            expected_line = sib.start_position().row;
            prev = sib.prev_sibling();
            continue;
        }

        if !comment_kinds.contains(&sib.kind()) {
            break;
        }

        // Only attach comments that are immediately adjacent (at most 1 blank line away -> actually 0 blank lines)
        let sib_end_line = sib.end_position().row;
        if expected_line.saturating_sub(sib_end_line) > 1 {
            break;
        }

        if let Ok(text) = sib.utf8_text(source) {
            let cleaned = clean_comment_text(text);
            if !cleaned.is_empty() {
                comments.push(cleaned);
            }
        }

        expected_line = sib.start_position().row;
        prev = sib.prev_sibling();
    }

    if comments.is_empty() {
        None
    } else {
        comments.reverse();
        Some(comments.join("\n"))
    }
}

fn clean_comment_text(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("///") {
        rest.trim().to_string()
    } else if let Some(rest) = trimmed.strip_prefix("//!") {
        rest.trim().to_string()
    } else if let Some(rest) = trimmed.strip_prefix("//") {
        rest.trim().to_string()
    } else if let Some(rest) = trimmed.strip_prefix('#') {
        rest.trim().to_string()
    } else if trimmed.starts_with("/*") && trimmed.ends_with("*/") {
        let inner = &trimmed[2..trimmed.len() - 2];
        inner
            .lines()
            .map(|line| line.trim().trim_start_matches('*').trim())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        trimmed.to_string()
    }
}

fn child_text_by_field(node: Node<'_>, field: &str, source: &[u8]) -> Option<String> {
    let child = node.child_by_field_name(field)?;
    child.utf8_text(source).ok().map(|s| s.trim().to_string())
}

fn build_symbol(
    node: Node<'_>,
    name: String,
    kind: SymbolKind,
    docstring: Option<String>,
    source: &[u8],
) -> CodeSymbol {
    let full_source = node.utf8_text(source).unwrap_or_default().to_string();
    let signature = full_source
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();

    CodeSymbol {
        name,
        kind,
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        docstring,
        signature,
        source: full_source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rust_symbols() {
        let code = r#"/// Represents a user session.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: u64,
}

/// Trait for persistence backends.
pub trait Store {
    fn save(&self, session: &Session);
}

impl Store for Session {
    /// Save the session to memory.
    fn save(&self, _session: &Session) {}
}

/// Standalone helper function.
pub fn compute_hash(input: &str) -> usize {
    input.len()
}
"#;

        let parsed = CodeParser::parse(code, SupportedLanguage::Rust).expect("rust parse failed");
        assert_eq!(parsed.language, SupportedLanguage::Rust);

        let names: Vec<(&str, SymbolKind)> = parsed
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind))
            .collect();

        assert_eq!(
            names,
            vec![
                ("Session", SymbolKind::Struct),
                ("Store", SymbolKind::Trait),
                ("save", SymbolKind::Method),
                ("Store for Session", SymbolKind::Impl),
                ("save", SymbolKind::Method),
                ("compute_hash", SymbolKind::Function),
            ]
        );

        assert_eq!(
            parsed.symbols[0].docstring.as_deref(),
            Some("Represents a user session.")
        );
        assert_eq!(parsed.symbols[0].start_line, 3);
        assert_eq!(
            parsed.symbols[5].docstring.as_deref(),
            Some("Standalone helper function.")
        );
    }

    #[test]
    fn test_parse_typescript_symbols() {
        let code = r#"/** Configuration for the API client. */
export interface ApiConfig {
    baseUrl: string;
}

export type DocumentId = string;

/** Main chat service class. */
export class ChatService {
    /** Send a message to the server. */
    async sendMessage(text: string): Promise<void> {
        console.log(text);
    }
}

/** Stream parser helper */
export const parseChunk = (raw: string): string => {
    return raw.trim();
};
"#;

        let parsed =
            CodeParser::parse(code, SupportedLanguage::TypeScript).expect("ts parse failed");
        let names: Vec<(&str, SymbolKind)> = parsed
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind))
            .collect();

        assert_eq!(
            names,
            vec![
                ("ApiConfig", SymbolKind::Interface),
                ("DocumentId", SymbolKind::TypeAlias),
                ("ChatService", SymbolKind::Class),
                ("sendMessage", SymbolKind::Method),
                ("parseChunk", SymbolKind::Function),
            ]
        );

        assert_eq!(
            parsed.symbols[0].docstring.as_deref(),
            Some("Configuration for the API client.")
        );
        assert_eq!(
            parsed.symbols[2].docstring.as_deref(),
            Some("Main chat service class.")
        );
        assert_eq!(
            parsed.symbols[3].docstring.as_deref(),
            Some("Send a message to the server.")
        );
        assert_eq!(
            parsed.symbols[4].docstring.as_deref(),
            Some("Stream parser helper")
        );
    }

    #[test]
    fn test_parse_python_symbols() {
        let code = r#"class Retriever:
    """Hybrid search retriever."""

    def search(self, query: str) -> list[str]:
        """Run hybrid retrieval for the given query."""
        return [query]


def normalize_score(score: float) -> float:
    """Clamp score into [0, 1]."""
    return max(0.0, min(1.0, score))
"#;

        let parsed = CodeParser::parse(code, SupportedLanguage::Python).expect("py parse failed");
        let names: Vec<(&str, SymbolKind)> = parsed
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind))
            .collect();

        assert_eq!(
            names,
            vec![
                ("Retriever", SymbolKind::Class),
                ("search", SymbolKind::Method),
                ("normalize_score", SymbolKind::Function),
            ]
        );

        assert_eq!(
            parsed.symbols[0].docstring.as_deref(),
            Some("Hybrid search retriever.")
        );
        assert_eq!(
            parsed.symbols[1].docstring.as_deref(),
            Some("Run hybrid retrieval for the given query.")
        );
        assert_eq!(
            parsed.symbols[2].docstring.as_deref(),
            Some("Clamp score into [0, 1].")
        );
    }
}
