use serde::{Deserialize, Serialize};

use crate::fingerprint::hash_text;
use crate::markdown::{MarkdownParser, MarkdownSection, ParsedMarkdown};
use crate::tokenizer::{TokenCounter, TokenizerEncoding};

/// Configuration controlling semantic Markdown chunking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkConfig {
    /// Hard upper bound on BPE tokens per chunk (including breadcrumb prefix).
    pub max_tokens: usize,
    /// Target token overlap between consecutive chunks split from the same oversized section.
    pub overlap_tokens: usize,
    /// Soft minimum token threshold below which adjacent sections sharing a parent heading
    /// may be merged when they still fit inside `max_tokens`.
    pub min_chunk_tokens: usize,
    /// BPE vocabulary used for exact token counting.
    pub encoding: TokenizerEncoding,
    /// Whether to prepend a `[H1 > H2 > H3]` breadcrumb prefix to each chunk.
    pub include_breadcrumbs: bool,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            max_tokens: 512,
            overlap_tokens: 64,
            min_chunk_tokens: 48,
            encoding: TokenizerEncoding::Cl100kBase,
            include_breadcrumbs: true,
        }
    }
}

/// A semantically coherent Markdown chunk with preserved heading hierarchy and exact token count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticChunk {
    pub chunk_index: usize,
    pub breadcrumbs: Vec<String>,
    pub heading_level: u8,
    /// Final chunk text (prefixed with `[Breadcrumb > Path]` when enabled).
    pub content: String,
    /// Underlying section body without the breadcrumb header.
    pub raw_content: String,
    /// Exact BPE token count of `content`.
    pub token_count: usize,
    /// Blake3 digest of `content`.
    pub content_hash: String,
}

/// Hierarchical, heading-boundary-aware Markdown semantic chunker.
#[derive(Debug, Clone)]
pub struct SemanticChunker {
    config: ChunkConfig,
    counter: TokenCounter,
}

impl Default for SemanticChunker {
    fn default() -> Self {
        Self::new(ChunkConfig::default())
    }
}

impl SemanticChunker {
    pub fn new(config: ChunkConfig) -> Self {
        let max_tokens = config.max_tokens.max(1);
        let overlap_tokens = config.overlap_tokens.min(max_tokens.saturating_sub(1));
        let min_chunk_tokens = config.min_chunk_tokens.min(max_tokens);
        let normalized = ChunkConfig {
            max_tokens,
            overlap_tokens,
            min_chunk_tokens,
            ..config
        };
        let counter = TokenCounter::new(normalized.encoding);
        Self {
            config: normalized,
            counter,
        }
    }

    pub fn config(&self) -> &ChunkConfig {
        &self.config
    }

    pub fn counter(&self) -> TokenCounter {
        self.counter
    }

    /// Convenience helper: parse raw Markdown and chunk it in one call.
    pub fn chunk_markdown_str(&self, input: &str) -> Vec<SemanticChunk> {
        let parsed = MarkdownParser::parse(input);
        self.chunk_markdown(&parsed)
    }

    /// Split a [`ParsedMarkdown`] document into heading-aware [`SemanticChunk`]s.
    pub fn chunk_markdown(&self, parsed: &ParsedMarkdown) -> Vec<SemanticChunk> {
        let non_empty_sections: Vec<&MarkdownSection> = parsed
            .sections
            .iter()
            .filter(|s| !s.content.trim().is_empty())
            .collect();

        // Fallback when a Markdown file contains only headings without body paragraphs
        if non_empty_sections.is_empty() {
            let mut out = Vec::new();
            for section in &parsed.sections {
                if section.title.trim().is_empty() {
                    continue;
                }
                let text = self.format_with_prefix(&section.breadcrumbs, &section.title);
                let final_text = if self.counter.count(&text) > self.config.max_tokens {
                    self.counter.truncate(&text, self.config.max_tokens)
                } else {
                    text
                };
                out.push(self.build_chunk(
                    out.len(),
                    section.breadcrumbs.clone(),
                    section.level,
                    final_text,
                    section.title.clone(),
                ));
            }
            return out;
        }

        let mut chunks: Vec<SemanticChunk> = Vec::new();

        // Optional buffered small section that may merge with a related successor
        let mut pending: Option<PendingSection> = None;

        for section in non_empty_sections {
            let body = section.content.trim();
            let full_candidate = self.format_with_prefix(&section.breadcrumbs, body);
            let candidate_tokens = self.counter.count(&full_candidate);

            if let Some(prev) = pending.take() {
                if candidate_tokens <= self.config.max_tokens
                    && self.can_merge_sections(&prev, section)
                {
                    let merged_breadcrumbs =
                        common_or_primary_breadcrumbs(&prev.breadcrumbs, &section.breadcrumbs);
                    let sub_heading = if section.title.is_empty() {
                        String::new()
                    } else {
                        let hashes = "#".repeat(usize::from(section.level.clamp(1, 6)));
                        format!("{hashes} {}\n", section.title)
                    };
                    let merged_raw = format!("{}\n\n{}{}", prev.raw_content, sub_heading, body);
                    let merged_full = self.format_with_prefix(&merged_breadcrumbs, &merged_raw);
                    let merged_tokens = self.counter.count(&merged_full);

                    if merged_tokens <= self.config.max_tokens {
                        let updated = PendingSection {
                            breadcrumbs: merged_breadcrumbs,
                            heading_level: prev.heading_level.min(section.level),
                            raw_content: merged_raw,
                            full_content: merged_full,
                            token_count: merged_tokens,
                        };
                        if updated.token_count < self.config.min_chunk_tokens {
                            pending = Some(updated);
                        } else {
                            chunks.push(self.build_chunk(
                                chunks.len(),
                                updated.breadcrumbs,
                                updated.heading_level,
                                updated.full_content,
                                updated.raw_content,
                            ));
                        }
                        continue;
                    }
                }

                // Could not merge; flush pending first
                chunks.push(self.build_chunk(
                    chunks.len(),
                    prev.breadcrumbs,
                    prev.heading_level,
                    prev.full_content,
                    prev.raw_content,
                ));
            }

            if candidate_tokens <= self.config.max_tokens {
                let item = PendingSection {
                    breadcrumbs: section.breadcrumbs.clone(),
                    heading_level: section.level,
                    raw_content: body.to_string(),
                    full_content: full_candidate,
                    token_count: candidate_tokens,
                };
                if item.token_count < self.config.min_chunk_tokens {
                    pending = Some(item);
                } else {
                    chunks.push(self.build_chunk(
                        chunks.len(),
                        item.breadcrumbs,
                        item.heading_level,
                        item.full_content,
                        item.raw_content,
                    ));
                }
            } else {
                // Oversized section: split without tearing code blocks or sentences where possible
                let split_bodies = self.split_oversized_section(&section.breadcrumbs, body);
                for piece in split_bodies {
                    let formatted = self.format_with_prefix(&section.breadcrumbs, &piece);
                    let bounded = if self.counter.count(&formatted) > self.config.max_tokens {
                        self.counter.truncate(&formatted, self.config.max_tokens)
                    } else {
                        formatted
                    };
                    chunks.push(self.build_chunk(
                        chunks.len(),
                        section.breadcrumbs.clone(),
                        section.level,
                        bounded,
                        piece,
                    ));
                }
            }
        }

        if let Some(prev) = pending {
            chunks.push(self.build_chunk(
                chunks.len(),
                prev.breadcrumbs,
                prev.heading_level,
                prev.full_content,
                prev.raw_content,
            ));
        }

        chunks
    }

    fn can_merge_sections(&self, prev: &PendingSection, next: &MarkdownSection) -> bool {
        if prev.token_count >= self.config.min_chunk_tokens {
            return false;
        }
        // Merge if both share the same root heading or if either has no breadcrumbs (preamble)
        match (prev.breadcrumbs.first(), next.breadcrumbs.first()) {
            (Some(a), Some(b)) => a == b,
            (None, _) => true,
            _ => false,
        }
    }

    fn breadcrumb_prefix(&self, breadcrumbs: &[String]) -> String {
        if !self.config.include_breadcrumbs || breadcrumbs.is_empty() {
            return String::new();
        }
        format!("[{}]\n\n", breadcrumbs.join(" > "))
    }

    fn format_with_prefix(&self, breadcrumbs: &[String], body: &str) -> String {
        let prefix = self.breadcrumb_prefix(breadcrumbs);
        if prefix.is_empty() {
            body.to_string()
        } else {
            format!("{prefix}{body}")
        }
    }

    fn split_oversized_section(&self, breadcrumbs: &[String], body: &str) -> Vec<String> {
        let prefix = self.breadcrumb_prefix(breadcrumbs);
        let prefix_tokens = self.counter.count(&prefix);
        let body_budget = self.config.max_tokens.saturating_sub(prefix_tokens).max(1);
        let effective_overlap = self
            .config
            .overlap_tokens
            .min(body_budget.saturating_sub(1));

        // 1. Split into atomic blocks (keeping fenced code blocks intact)
        let blocks = split_atomic_markdown_blocks(body);

        // 2. Break any single oversized block into sentence-level or token-level sub-blocks
        let mut units: Vec<String> = Vec::new();
        for block in blocks {
            if self.fits_in_budget(breadcrumbs, &block) {
                units.push(block);
            } else {
                units.extend(self.split_oversized_block(breadcrumbs, &block, body_budget));
            }
        }

        // 3. Pack units greedily into chunks with overlap carry-over
        let mut results: Vec<String> = Vec::new();
        let mut current = String::new();

        for unit in units {
            if current.is_empty() {
                current = unit;
                continue;
            }

            let joined = format!("{current}\n\n{unit}");
            if self.fits_in_budget(breadcrumbs, &joined) {
                current = joined;
            } else {
                results.push(current.clone());

                // Build overlap tail from the emitted chunk if it fits with the incoming unit
                let overlap_tail = self.extract_overlap_tail(&current, effective_overlap);
                if !overlap_tail.is_empty() {
                    let with_overlap = format!("{overlap_tail}\n\n{unit}");
                    if self.fits_in_budget(breadcrumbs, &with_overlap) {
                        current = with_overlap;
                        continue;
                    }
                }
                current = unit;
            }
        }

        if !current.trim().is_empty() {
            results.push(current);
        }

        results
    }

    fn split_oversized_block(
        &self,
        breadcrumbs: &[String],
        block: &str,
        body_budget: usize,
    ) -> Vec<String> {
        let effective_overlap = self
            .config
            .overlap_tokens
            .min(body_budget.saturating_sub(1));

        // If it's a fenced code block, split by lines instead of punctuation
        let pieces = if block.trim_start().starts_with("```") {
            block
                .lines()
                .map(|l| l.to_string())
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
        } else {
            split_into_sentences(block)
        };

        let separator = if block.trim_start().starts_with("```") {
            "\n"
        } else {
            " "
        };

        let mut packed = Vec::new();
        let mut current = String::new();

        for piece in pieces {
            if !self.fits_in_budget(breadcrumbs, &piece) {
                if !current.is_empty() {
                    packed.push(std::mem::take(&mut current));
                }
                // Hard fallback: token-window split preserves strict UTF-8 safety
                packed.extend(
                    self.counter
                        .split_by_tokens(&piece, body_budget, effective_overlap),
                );
                continue;
            }

            if current.is_empty() {
                current = piece;
            } else {
                let candidate = format!("{current}{separator}{piece}");
                if self.fits_in_budget(breadcrumbs, &candidate) {
                    current = candidate;
                } else {
                    packed.push(current);
                    current = piece;
                }
            }
        }

        if !current.is_empty() {
            packed.push(current);
        }

        packed
    }

    fn fits_in_budget(&self, breadcrumbs: &[String], body: &str) -> bool {
        let formatted = self.format_with_prefix(breadcrumbs, body);
        self.counter.count(&formatted) <= self.config.max_tokens
    }

    /// Take up to `overlap_tokens` from the tail of `text`, preferring sentence/line boundaries.
    fn extract_overlap_tail(&self, text: &str, overlap_tokens: usize) -> String {
        if overlap_tokens == 0 || text.is_empty() {
            return String::new();
        }

        // Prefer taking trailing sentences/lines that fit cleanly within `overlap_tokens`
        let sentences = split_into_sentences(text);
        let mut tail_parts: Vec<&str> = Vec::new();
        let mut combined = String::new();

        for sent in sentences.iter().rev() {
            let candidate = if combined.is_empty() {
                sent.clone()
            } else {
                format!("{sent} {combined}")
            };
            if self.counter.count(&candidate) <= overlap_tokens {
                combined = candidate;
                tail_parts.push(sent.as_str());
            } else {
                break;
            }
        }

        if !tail_parts.is_empty() {
            return combined;
        }

        // Fallback: take the last `overlap_tokens` via UTF-8-safe token slice
        let tokens = self.counter.encode(text);
        if tokens.len() <= overlap_tokens {
            return text.to_string();
        }
        let mut start = tokens.len() - overlap_tokens;
        while start < tokens.len() {
            if let Ok(decoded) = self.counter.decode(&tokens[start..])
                && !decoded.contains('\u{FFFD}')
            {
                return decoded.trim().to_string();
            }
            start += 1;
        }

        String::new()
    }

    fn build_chunk(
        &self,
        chunk_index: usize,
        breadcrumbs: Vec<String>,
        heading_level: u8,
        content: String,
        raw_content: String,
    ) -> SemanticChunk {
        let token_count = self.counter.count(&content);
        let content_hash = hash_text(&content);
        SemanticChunk {
            chunk_index,
            breadcrumbs,
            heading_level,
            content,
            raw_content,
            token_count,
            content_hash,
        }
    }
}

struct PendingSection {
    breadcrumbs: Vec<String>,
    heading_level: u8,
    raw_content: String,
    full_content: String,
    token_count: usize,
}

fn common_or_primary_breadcrumbs(a: &[String], b: &[String]) -> Vec<String> {
    let common: Vec<String> = a
        .iter()
        .zip(b.iter())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.clone())
        .collect();
    if !common.is_empty() {
        common
    } else if !a.is_empty() {
        a.to_vec()
    } else {
        b.to_vec()
    }
}

/// Split Markdown section body into logical blocks while keeping fenced code blocks intact.
fn split_atomic_markdown_blocks(body: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut in_fence = false;

    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if !in_fence {
                // Flush preceding prose paragraph before starting code block
                if !current.trim().is_empty() {
                    blocks.push(current.trim().to_string());
                    current.clear();
                }
                in_fence = true;
                current.push_str(line);
                current.push('\n');
            } else {
                current.push_str(line);
                blocks.push(current.trim().to_string());
                current.clear();
                in_fence = false;
            }
            continue;
        }

        if in_fence {
            current.push_str(line);
            current.push('\n');
        } else if line.trim().is_empty() {
            if !current.trim().is_empty() {
                blocks.push(current.trim().to_string());
                current.clear();
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }

    if !current.trim().is_empty() {
        blocks.push(current.trim().to_string());
    }

    blocks
}

/// Split prose into sentence-like segments supporting both ASCII and CJK punctuation.
fn split_into_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        current.push(ch);
        let is_cjk_end = matches!(ch, '。' | '！' | '？');
        let is_ascii_end =
            matches!(ch, '.' | '!' | '?') && chars.peek().is_none_or(|next| next.is_whitespace());
        let is_newline = ch == '\n';

        if is_cjk_end || is_ascii_end || is_newline {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                sentences.push(trimmed.to_string());
            }
            current.clear();
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        sentences.push(trimmed.to_string());
    }

    sentences
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hierarchical_breadcrumbs_preserved_per_chunk() {
        let md = r#"# Doc

Introductory overview for the Rust RAG platform with enough detail to stand on its own as a meaningful section for retrieval.

## Section 2

### Step 1
Configure PostgreSQL with the pgvector extension and create an HNSW cosine index for sub-millisecond nearest neighbor queries.

### Step 2
Run the parallel Rayon document scanner and compute Blake3 fingerprints to avoid re-embedding unchanged chunks.
"#;

        let chunker = SemanticChunker::new(ChunkConfig {
            max_tokens: 120,
            overlap_tokens: 16,
            min_chunk_tokens: 10,
            encoding: TokenizerEncoding::Cl100kBase,
            include_breadcrumbs: true,
        });

        let chunks = chunker.chunk_markdown_str(md);
        assert_eq!(chunks.len(), 3);

        assert_eq!(chunks[0].breadcrumbs, vec!["Doc"]);
        assert!(chunks[0].content.starts_with("[Doc]\n\n"));

        assert_eq!(chunks[1].breadcrumbs, vec!["Doc", "Section 2", "Step 1"]);
        assert!(
            chunks[1]
                .content
                .starts_with("[Doc > Section 2 > Step 1]\n\n")
        );
        assert!(chunks[1].content.contains("pgvector"));

        assert_eq!(chunks[2].breadcrumbs, vec!["Doc", "Section 2", "Step 2"]);
        assert!(
            chunks[2]
                .content
                .starts_with("[Doc > Section 2 > Step 2]\n\n")
        );
        assert!(chunks[2].content.contains("Blake3"));

        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.chunk_index, i);
            assert_eq!(chunk.token_count, chunker.counter().count(&chunk.content));
            assert!(chunk.token_count <= 120);
            assert_eq!(chunk.content_hash.len(), 64);
        }
    }

    #[test]
    fn test_small_sibling_sections_merge_within_window() {
        let md = r#"# Guide

## Part A
Short note A.

## Part B
Short note B.
"#;

        let chunker = SemanticChunker::new(ChunkConfig {
            max_tokens: 128,
            overlap_tokens: 16,
            min_chunk_tokens: 40,
            encoding: TokenizerEncoding::Cl100kBase,
            include_breadcrumbs: true,
        });

        let chunks = chunker.chunk_markdown_str(md);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].breadcrumbs, vec!["Guide"]);
        assert!(chunks[0].content.contains("Short note A."));
        assert!(chunks[0].content.contains("Short note B."));
    }

    #[test]
    fn test_oversized_section_splits_with_overlap_and_no_code_block_tearing() {
        let md = r#"# Engine Design

## Retrieval Pipeline

First paragraph explains dual-channel recall combining dense HNSW vector search with PostgreSQL full-text GIN indexes for exact identifier matching.

```rust
pub fn rrf_fuse(rank_a: usize, rank_b: usize, k: f32) -> f32 {
    1.0 / (k + rank_a as f32) + 1.0 / (k + rank_b as f32)
}
```

Final paragraph explains how streaming Server-Sent Events attach citation metadata so the Next.js interface can highlight the exact source snippet in a side drawer.
"#;

        let chunker = SemanticChunker::new(ChunkConfig {
            max_tokens: 75,
            overlap_tokens: 15,
            min_chunk_tokens: 10,
            encoding: TokenizerEncoding::Cl100kBase,
            include_breadcrumbs: true,
        });

        let chunks = chunker.chunk_markdown_str(md);
        assert!(
            chunks.len() >= 2,
            "expected oversized section to split, got {}",
            chunks.len()
        );

        // Every chunk preserves the exact section breadcrumb and respects hard token ceiling
        for chunk in &chunks {
            assert_eq!(
                chunk.breadcrumbs,
                vec!["Engine Design", "Retrieval Pipeline"]
            );
            assert!(
                chunk
                    .content
                    .starts_with("[Engine Design > Retrieval Pipeline]\n\n")
            );
            assert!(
                chunk.token_count <= 75,
                "chunk exceeded max_tokens: {} > 75\n{}",
                chunk.token_count,
                chunk.content
            );
        }

        // The fenced Rust code block must remain intact inside at least one chunk (no mid-block tear)
        assert!(
            chunks.iter().any(|c| c.content.contains("```rust")
                && c.content.contains("1.0 / (k + rank_a as f32)")),
            "expected fenced code block to stay intact without tearing"
        );
    }

    #[test]
    fn test_chinese_oversized_section_splits_cleanly() {
        let md = r#"# 混合检索指南

## 分块策略
在生产级 RAG 系统中，按固定字符截断容易撕裂语义边界。基于标题层级的语义切块器会保留父级面包屑路径。当单个章节超出 Token 预算时，切块器优先按段落与句号切分，并保留适度的上下文重叠窗口，确保召回率与问答忠实度同时达到高水准。
"#;

        let chunker = SemanticChunker::new(ChunkConfig {
            max_tokens: 55,
            overlap_tokens: 12,
            min_chunk_tokens: 10,
            encoding: TokenizerEncoding::O200kBase,
            include_breadcrumbs: true,
        });

        let chunks = chunker.chunk_markdown_str(md);
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(!chunk.content.contains('\u{FFFD}'));
            assert!(chunk.content.starts_with("[混合检索指南 > 分块策略]\n\n"));
            assert!(chunk.token_count <= 55);
        }
    }
}
