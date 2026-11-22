export type ModelTier = "fast" | "reasoning";

export interface Conversation {
  id: string;
  title: string | null;
  created_at: string;
}

export interface Message {
  id: string;
  conversation_id: string;
  role: "system" | "user" | "assistant";
  content: string;
  token_count: number | null;
  created_at: string;
}

export interface ModelTiersInfo {
  fast: string;
  reasoning?: string;
}

export interface ModelsInfo {
  default_tier: ModelTier;
  tiers: ModelTiersInfo;
  models: string[];
}

export interface TechReport {
  summary: string;
  pros: string[];
  cons: string[];
  score: number;
}

export interface StreamDonePayload {
  message_id: string;
}

export interface ApiErrorResponse {
  error: {
    code: string;
    message: string;
  };
}

// --- Month 2: Rust Document & AST Parser Types ---

export interface CodeBlock {
  language: string | null;
  code: string;
}

export interface MarkdownSection {
  level: number;
  title: string;
  breadcrumbs: string[];
  content: string;
  code_blocks: CodeBlock[];
}

export interface ParsedMarkdown {
  title: string | null;
  sections: MarkdownSection[];
  total_chars: number;
}

export type SupportedLanguage = "rust" | "typescript" | "tsx" | "python";

export type SymbolKind =
  | "function"
  | "method"
  | "struct"
  | "enum"
  | "trait"
  | "impl"
  | "class"
  | "interface"
  | "type_alias";

export interface CodeSymbol {
  name: string;
  kind: SymbolKind;
  start_line: number;
  end_line: number;
  start_byte: number;
  end_byte: number;
  docstring: string | null;
  signature: string;
  source: string;
}

export interface ParsedCode {
  language: SupportedLanguage;
  symbols: CodeSymbol[];
  total_lines: number;
  total_chars: number;
}

export type DocumentContent =
  | ({ kind: "markdown" } & ParsedMarkdown)
  | ({ kind: "code" } & ParsedCode)
  | { kind: "plain_text"; content: string; total_chars: number };

export interface ParsedDocument {
  path: string;
  size_bytes: number;
  content_hash: string;
  content: DocumentContent;
}

export interface UnitFingerprint {
  unit_key: string;
  hash: string;
}

export interface DocumentFingerprint {
  path: string;
  content_hash: string;
  unit_hashes: UnitFingerprint[];
}

export interface ParseDocumentResponse {
  document: ParsedDocument;
  fingerprint: DocumentFingerprint;
  parse_time_ms: number;
}

export interface ParseBatchItem {
  document: ParsedDocument;
  fingerprint: DocumentFingerprint;
}

export interface ParseBatchFailure {
  filename: string;
  error: string;
}

export interface ParseBatchResponse {
  parsed: ParseBatchItem[];
  failed: ParseBatchFailure[];
  total_bytes: number;
  elapsed_ms: number;
}

// --- Month 2 (Week 6): Semantic & AST Chunker Types ---

export type TokenizerEncoding = "cl100k_base" | "o200k_base";

export type ChunkKind = "markdown" | "code" | "plain_text";

export interface ByteRange {
  start: number;
  end: number;
}

export interface LineRange {
  start: number;
  end: number;
}

export interface ChunkMetadata {
  file_path: string;
  kind: ChunkKind;
  language: SupportedLanguage | null;
  module_path: string | null;
  breadcrumbs: string[];
  heading_level: number | null;
  symbol_name: string | null;
  parent_symbol: string | null;
  symbol_kind: SymbolKind | null;
  byte_range: ByteRange;
  line_range: LineRange | null;
  encoding: TokenizerEncoding;
}

export interface DocumentChunk {
  chunk_id: string;
  file_id: string;
  chunk_index: number;
  content: string;
  raw_content: string;
  token_count: number;
  content_hash: string;
  metadata: ChunkMetadata;
}

export interface ChunkDocumentRequest {
  filename: string;
  content: string;
  max_tokens?: number;
  overlap_tokens?: number;
  min_chunk_tokens?: number;
  encoding?: TokenizerEncoding;
  include_context_headers?: boolean;
}

export interface ChunkDocumentResponse {
  chunks: DocumentChunk[];
  total_chunks: number;
  total_tokens: number;
  avg_tokens: number;
  chunk_time_ms: number;
}
