use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::scanner::{DocumentContent, ParsedDocument};

/// Compute the lowercase hex Blake3 digest of arbitrary bytes.
pub fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Compute the lowercase hex Blake3 digest of a UTF-8 string.
pub fn hash_text(text: &str) -> String {
    hash_bytes(text.as_bytes())
}

/// Blake3 fingerprint of an individual logical unit (a Markdown section or a code AST symbol).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitFingerprint {
    pub unit_key: String,
    pub hash: String,
}

/// Blake3 fingerprint for a parsed document and all of its inner structural units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentFingerprint {
    pub path: String,
    pub content_hash: String,
    pub unit_hashes: Vec<UnitFingerprint>,
}

/// Deterministic incremental diff result between two fingerprint snapshots.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffSummary {
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: Vec<String>,
}

impl DiffSummary {
    /// Returns true if no additions, modifications, or removals were detected.
    pub fn is_clean(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.removed.is_empty()
    }
}

fn disambiguate_unit_key(base: String, seen: &mut HashMap<String, usize>) -> String {
    let count = seen.entry(base.clone()).or_insert(0);
    *count += 1;
    if *count == 1 {
        base
    } else {
        format!("{base}#{}", *count)
    }
}

pub struct DiffCalculator;

impl DiffCalculator {
    /// Generate a document-level and section/symbol-level Blake3 fingerprint.
    pub fn fingerprint_document(doc: &ParsedDocument) -> DocumentFingerprint {
        let mut seen_keys: HashMap<String, usize> = HashMap::new();
        let unit_hashes = match &doc.content {
            DocumentContent::Markdown(md) => md
                .sections
                .iter()
                .enumerate()
                .map(|(idx, section)| {
                    let base_key = if section.breadcrumbs.is_empty() {
                        format!("section:{idx}")
                    } else {
                        format!("md:{}", section.breadcrumbs.join(" > "))
                    };
                    let key = disambiguate_unit_key(base_key, &mut seen_keys);
                    let mut hasher = blake3::Hasher::new();
                    hasher.update(section.title.as_bytes());
                    hasher.update(b"\0");
                    hasher.update(section.content.as_bytes());
                    UnitFingerprint {
                        unit_key: key,
                        hash: hasher.finalize().to_hex().to_string(),
                    }
                })
                .collect(),
            DocumentContent::Code(code) => code
                .symbols
                .iter()
                .map(|sym| {
                    let base_key = format!("{:?}:{}", sym.kind, sym.name);
                    let key = disambiguate_unit_key(base_key, &mut seen_keys);
                    let mut hasher = blake3::Hasher::new();
                    if let Some(doc_comment) = &sym.docstring {
                        hasher.update(doc_comment.as_bytes());
                        hasher.update(b"\0");
                    }
                    hasher.update(sym.source.as_bytes());
                    UnitFingerprint {
                        unit_key: key,
                        hash: hasher.finalize().to_hex().to_string(),
                    }
                })
                .collect(),
            DocumentContent::PlainText { content, .. } => vec![UnitFingerprint {
                unit_key: "plaintext:body".to_string(),
                hash: hash_text(content),
            }],
        };

        DocumentFingerprint {
            path: doc.path.clone(),
            content_hash: doc.content_hash.clone(),
            unit_hashes,
        }
    }

    /// Compare two key->hash maps and classify entries into added, modified, removed, and unchanged.
    pub fn diff_snapshots(
        previous: &HashMap<String, String>,
        current: &HashMap<String, String>,
    ) -> DiffSummary {
        let mut summary = DiffSummary::default();

        for (key, cur_hash) in current {
            match previous.get(key) {
                None => summary.added.push(key.clone()),
                Some(prev_hash) if prev_hash == cur_hash => summary.unchanged.push(key.clone()),
                Some(_) => summary.modified.push(key.clone()),
            }
        }

        for key in previous.keys() {
            if !current.contains_key(key) {
                summary.removed.push(key.clone());
            }
        }

        summary.added.sort();
        summary.modified.sort();
        summary.removed.sort();
        summary.unchanged.sort();
        summary
    }

    /// Diff two snapshots of parsed document fingerprints at the file level.
    pub fn diff_documents(
        previous: &[DocumentFingerprint],
        current: &[DocumentFingerprint],
    ) -> DiffSummary {
        let prev_map: HashMap<String, String> = previous
            .iter()
            .map(|d| (d.path.clone(), d.content_hash.clone()))
            .collect();
        let cur_map: HashMap<String, String> = current
            .iter()
            .map(|d| (d.path.clone(), d.content_hash.clone()))
            .collect();
        Self::diff_snapshots(&prev_map, &cur_map)
    }

    /// Diff two versions of the same document at the individual section/symbol unit level.
    pub fn diff_units(
        previous: &DocumentFingerprint,
        current: &DocumentFingerprint,
    ) -> DiffSummary {
        let prev_map: HashMap<String, String> = previous
            .unit_hashes
            .iter()
            .map(|u| (u.unit_key.clone(), u.hash.clone()))
            .collect();
        let cur_map: HashMap<String, String> = current
            .unit_hashes
            .iter()
            .map(|u| (u.unit_key.clone(), u.hash.clone()))
            .collect();
        Self::diff_snapshots(&prev_map, &cur_map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::RepositoryScanner;

    #[test]
    fn test_blake3_hash_determinism() {
        let h1 = hash_text("hello rust rag");
        let h2 = hash_text("hello rust rag");
        let h3 = hash_text("hello rust rag!");
        assert_eq!(h1.len(), 64);
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_document_level_incremental_diff() {
        let doc_a1 = RepositoryScanner::parse_content("a.md", "# A\nOriginal").unwrap();
        let doc_b1 = RepositoryScanner::parse_content("b.rs", "fn keep() {}").unwrap();
        let doc_c1 = RepositoryScanner::parse_content("c.md", "# Removed").unwrap();

        let doc_a2 = RepositoryScanner::parse_content("a.md", "# A\nUpdated!").unwrap();
        let doc_b2 = RepositoryScanner::parse_content("b.rs", "fn keep() {}").unwrap();
        let doc_d2 = RepositoryScanner::parse_content("d.py", "def brand_new(): pass").unwrap();

        let prev = vec![
            DiffCalculator::fingerprint_document(&doc_a1),
            DiffCalculator::fingerprint_document(&doc_b1),
            DiffCalculator::fingerprint_document(&doc_c1),
        ];
        let curr = vec![
            DiffCalculator::fingerprint_document(&doc_a2),
            DiffCalculator::fingerprint_document(&doc_b2),
            DiffCalculator::fingerprint_document(&doc_d2),
        ];

        let diff = DiffCalculator::diff_documents(&prev, &curr);
        assert_eq!(diff.added, vec!["d.py"]);
        assert_eq!(diff.modified, vec!["a.md"]);
        assert_eq!(diff.removed, vec!["c.md"]);
        assert_eq!(diff.unchanged, vec!["b.rs"]);
        assert!(!diff.is_clean());
    }

    #[test]
    fn test_symbol_level_incremental_diff() {
        let v1 = r#"pub fn untouched() -> i32 { 1 }
pub fn changed() -> i32 { 2 }
pub fn deleted() -> i32 { 3 }
"#;
        let v2 = r#"pub fn untouched() -> i32 { 1 }
pub fn changed() -> i32 { 42 }
pub fn added() -> i32 { 99 }
"#;
        let fp1 = DiffCalculator::fingerprint_document(
            &RepositoryScanner::parse_content("lib.rs", v1).unwrap(),
        );
        let fp2 = DiffCalculator::fingerprint_document(
            &RepositoryScanner::parse_content("lib.rs", v2).unwrap(),
        );

        let unit_diff = DiffCalculator::diff_units(&fp1, &fp2);
        assert_eq!(unit_diff.unchanged, vec!["Function:untouched"]);
        assert_eq!(unit_diff.modified, vec!["Function:changed"]);
        assert_eq!(unit_diff.removed, vec!["Function:deleted"]);
        assert_eq!(unit_diff.added, vec!["Function:added"]);
    }

    #[test]
    fn test_duplicate_unit_keys_are_disambiguated() {
        let v1 = r#"struct A;
struct B;
impl A { fn new() -> Self { Self } }
impl B { fn new() -> Self { Self } }
"#;
        let v2 = r#"struct A;
struct B;
impl A { fn new() -> Self { Self } }
impl B { fn new() -> Self { let _ = 1; Self } }
"#;
        let fp1 = DiffCalculator::fingerprint_document(
            &RepositoryScanner::parse_content("dup.rs", v1).unwrap(),
        );
        let fp2 = DiffCalculator::fingerprint_document(
            &RepositoryScanner::parse_content("dup.rs", v2).unwrap(),
        );

        let diff = DiffCalculator::diff_units(&fp1, &fp2);
        assert!(diff.unchanged.contains(&"Method:new".to_string()));
        assert!(diff.modified.contains(&"Method:new#2".to_string()));
    }
}
