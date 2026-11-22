use std::sync::OnceLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tiktoken_rs::{CoreBPE, Rank, cl100k_base, o200k_base};

use crate::error::ParseError;

static CL100K_BPE: OnceLock<CoreBPE> = OnceLock::new();
static O200K_BPE: OnceLock<CoreBPE> = OnceLock::new();

/// Supported Byte-Pair Encoding (BPE) vocabularies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TokenizerEncoding {
    /// OpenAI `cl100k_base` (GPT-4, `text-embedding-3-small`, `text-embedding-3-large`).
    #[default]
    Cl100kBase,
    /// OpenAI `o200k_base` (GPT-4o, o1, o3; more compact on CJK and source code).
    O200kBase,
}

impl TokenizerEncoding {
    fn bpe(self) -> &'static CoreBPE {
        match self {
            Self::Cl100kBase => {
                CL100K_BPE.get_or_init(|| cl100k_base().expect("cl100k_base vocabulary is bundled"))
            }
            Self::O200kBase => {
                O200K_BPE.get_or_init(|| o200k_base().expect("o200k_base vocabulary is bundled"))
            }
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cl100kBase => "cl100k_base",
            Self::O200kBase => "o200k_base",
        }
    }
}

/// Thread-safe, zero-overhead BPE token counter and UTF-8-safe truncator.
///
/// Backed by lazily initialized `CoreBPE` singletons, so `TokenCounter` is `Copy`,
/// `Send`, and `Sync` and can be shared freely across `rayon` threads.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokenCounter {
    encoding: TokenizerEncoding,
}

impl TokenCounter {
    pub const fn new(encoding: TokenizerEncoding) -> Self {
        Self { encoding }
    }

    pub const fn cl100k() -> Self {
        Self::new(TokenizerEncoding::Cl100kBase)
    }

    pub const fn o200k() -> Self {
        Self::new(TokenizerEncoding::O200kBase)
    }

    pub const fn encoding(&self) -> TokenizerEncoding {
        self.encoding
    }

    /// Encode text into BPE token IDs (ignoring special token markers).
    pub fn encode(&self, text: &str) -> Vec<Rank> {
        if text.is_empty() {
            return Vec::new();
        }
        self.encoding.bpe().encode_ordinary(text)
    }

    /// Return the exact BPE token count for `text`.
    pub fn count(&self, text: &str) -> usize {
        self.encode(text).len()
    }

    /// Decode BPE token IDs back into a UTF-8 string.
    pub fn decode(&self, tokens: &[Rank]) -> Result<String, ParseError> {
        if tokens.is_empty() {
            return Ok(String::new());
        }
        self.encoding
            .bpe()
            .decode(tokens)
            .map_err(|e| ParseError::TokenizerDecode(e.to_string()))
    }

    /// Truncate `text` to at most `max_tokens` BPE tokens while preserving UTF-8 character boundaries.
    ///
    /// In BPE encodings, a single multi-byte character (such as a CJK ideograph or emoji) may
    /// span 2–3 tokens. If slicing at `max_tokens` splits a UTF-8 sequence mid-character, this
    /// method steps back to the nearest complete UTF-8 boundary so the output is always valid
    /// UTF-8 with no replacement characters and strictly `<= max_tokens` tokens.
    pub fn truncate(&self, text: &str, max_tokens: usize) -> String {
        if max_tokens == 0 || text.is_empty() {
            return String::new();
        }

        let tokens = self.encode(text);
        if tokens.len() <= max_tokens {
            return text.to_string();
        }

        let bpe = self.encoding.bpe();
        let mut end = max_tokens;
        while end > 0 {
            if let Ok(decoded) = decode_strict_utf8(bpe, &tokens[..end]) {
                return decoded;
            }
            end -= 1;
        }

        String::new()
    }

    /// Split `text` into overlapping token-bounded windows while preserving UTF-8 integrity.
    ///
    /// If `max_tokens == 0`, returns an empty `Vec`. `overlap_tokens` is clamped to
    /// `max_tokens.saturating_sub(1)` so the window always advances.
    pub fn split_by_tokens(
        &self,
        text: &str,
        max_tokens: usize,
        overlap_tokens: usize,
    ) -> Vec<String> {
        if max_tokens == 0 || text.is_empty() {
            return Vec::new();
        }

        let tokens = self.encode(text);
        if tokens.len() <= max_tokens {
            return vec![text.to_string()];
        }

        let bpe = self.encoding.bpe();
        let effective_overlap = overlap_tokens.min(max_tokens.saturating_sub(1));
        let mut windows = Vec::new();
        let mut start = 0usize;

        while start < tokens.len() {
            // Advance start past any leading UTF-8 continuation bytes caused by overlap
            let mut aligned_start = start;
            let mut end = (aligned_start + max_tokens).min(tokens.len());

            // Try to find the widest valid UTF-8 slice within [aligned_start..end]
            let mut decoded_window = None;
            while aligned_start < end {
                let mut probe_end = end;
                while probe_end > aligned_start {
                    if let Ok(s) = decode_strict_utf8(bpe, &tokens[aligned_start..probe_end]) {
                        decoded_window = Some((s, probe_end));
                        break;
                    }
                    probe_end -= 1;
                }
                if decoded_window.is_some() {
                    break;
                }
                aligned_start += 1;
                end = (aligned_start + max_tokens).min(tokens.len());
            }

            let Some((chunk, actual_end)) = decoded_window else {
                break;
            };

            if !chunk.is_empty() {
                windows.push(chunk);
            }

            if actual_end >= tokens.len() {
                break;
            }

            let next_start = actual_end.saturating_sub(effective_overlap);
            start = if next_start <= start {
                actual_end
            } else {
                next_start
            };
        }

        windows
    }
}

/// Strictly decode token slice into UTF-8, rejecting incomplete multi-byte sequences.
fn decode_strict_utf8(bpe: &CoreBPE, tokens: &[Rank]) -> Result<String, ()> {
    let bytes = bpe._decode_native_and_split(tokens.to_vec());
    let flat: Vec<u8> = bytes.into_iter().flatten().collect();
    String::from_utf8(flat).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_and_english_token_counting() {
        let counter = TokenCounter::cl100k();
        assert_eq!(counter.count(""), 0);
        assert_eq!(counter.truncate("", 10), "");

        let text = "Hello, Rust RAG engine!";
        let tokens = counter.encode(text);
        assert_eq!(tokens.len(), counter.count(text));
        assert!(tokens.len() >= 4 && tokens.len() <= 10);
        assert_eq!(counter.decode(&tokens).unwrap(), text);
    }

    #[test]
    fn test_multilingual_cjk_code_and_emoji_roundtrip() {
        let cl100k = TokenCounter::cl100k();
        let o200k = TokenCounter::o200k();

        let sample = "Rust 高性能混合 RAG 引擎 🚀: fn rrf_score(k: f32) -> f32 { 1.0 / (k + 1.0) } — 中英混排测试！";
        let cl_tokens = cl100k.encode(sample);
        let o2_tokens = o200k.encode(sample);

        assert!(!cl_tokens.is_empty());
        assert!(!o2_tokens.is_empty());
        // o200k_base has a 200k vocabulary optimized for CJK, so it generally uses fewer tokens than cl100k_base
        assert!(o2_tokens.len() <= cl_tokens.len());

        assert_eq!(cl100k.decode(&cl_tokens).unwrap(), sample);
        assert_eq!(o200k.decode(&o2_tokens).unwrap(), sample);
    }

    #[test]
    fn test_safe_utf8_truncation_on_cjk_and_emoji() {
        let counter = TokenCounter::cl100k();
        let text = "深度学习与向量检索系统🚀Rust编程语言";
        let total = counter.count(text);
        assert!(total > 5);

        // Test every possible cut point from 0..=total to ensure NO panics, NO U+FFFD,
        // strict prefix property, and token count <= budget.
        for limit in 0..=total {
            let truncated = counter.truncate(text, limit);
            assert!(
                !truncated.contains('\u{FFFD}'),
                "replacement char found at limit={limit}: {truncated:?}"
            );
            assert!(
                text.starts_with(&truncated),
                "truncated output is not an exact prefix at limit={limit}: {truncated:?}"
            );
            assert!(
                counter.count(&truncated) <= limit,
                "truncated count {} exceeded limit {limit}",
                counter.count(&truncated)
            );
        }

        // Larger limit returns full text untouched
        assert_eq!(counter.truncate(text, total + 100), text);
    }

    #[test]
    fn test_split_by_tokens_with_overlap() {
        let counter = TokenCounter::o200k();
        let text = "Rust 异步运行时 Tokio 配合 Axum 构建高性能 AI 网关，支持 SSE 流式输出、pgvector 向量检索与 RRF 混合重排。";

        let chunks = counter.split_by_tokens(text, 12, 3);
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(!chunk.contains('\u{FFFD}'));
            assert!(counter.count(chunk) <= 12);
        }
        // First chunk starts at the beginning, last chunk ends at the end
        assert!(text.starts_with(&chunks[0]));
        assert!(text.ends_with(chunks.last().unwrap()));
    }
}
