use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use std::collections::HashMap;
use std::fmt::Write as _;
use thiserror::Error;
use uuid::Uuid;

/// Fixed embedding vector width declared in `document_chunks.embedding vector(1536)`.
pub const EMBEDDING_DIMENSIONS: usize = 1536;

/// Maximum rows packed into a single `UNNEST(...)` upsert statement to keep PostgreSQL
/// query parameters and transaction working memory bounded.
pub const DEFAULT_UPSERT_BATCH_SIZE: usize = 250;

/// Maximum Top-K neighbors returned by a single vector similarity search.
pub const MAX_SEARCH_LIMIT: usize = 100;

/// Errors returned by [`ChunkRepository`].
#[derive(Debug, Error)]
pub enum ChunkRepoError {
    #[error("invalid chunk at index {index}: {reason}")]
    InvalidChunk { index: usize, reason: String },

    #[error("invalid vector search query: {0}")]
    InvalidQuery(String),

    #[error(
        "embedding dimension mismatch at chunk index {index}: expected {expected}, got {actual}"
    )]
    DimensionMismatch {
        index: usize,
        expected: usize,
        actual: usize,
    },

    #[error("embedding at chunk index {index} contains non-finite float values (NaN/Inf)")]
    NonFiniteEmbedding { index: usize },

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// Input payload for upserting an embedded document chunk into PostgreSQL `pgvector`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewDocumentChunk {
    pub document_id: String,
    pub chunk_index: i32,
    pub content: String,
    pub metadata: serde_json::Value,
    pub content_hash: String,
    pub token_count: i32,
    pub embedding: Vec<f32>,
}

/// Row returned from `document_chunks` after upsert or query.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct StoredDocumentChunk {
    pub id: Uuid,
    pub document_id: String,
    pub chunk_index: i32,
    pub content: String,
    pub metadata: serde_json::Value,
    pub content_hash: String,
    pub token_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Ranked vector search hit returned by [`ChunkRepository::search_similar_chunks`].
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct SimilarChunk {
    pub id: Uuid,
    pub document_id: String,
    pub chunk_index: i32,
    pub content: String,
    pub metadata: serde_json::Value,
    pub content_hash: String,
    pub token_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Raw pgvector cosine distance (`embedding <=> query`), in `[0.0, 2.0]`.
    pub cosine_distance: f64,
    /// Cosine similarity (`1.0 - cosine_distance`), in `[-1.0, 1.0]`.
    pub similarity: f64,
}

/// Summary returned by [`ChunkRepository::batch_insert_chunks`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchUpsertSummary {
    /// Total chunks passed by the caller before intra-batch deduplication.
    pub input_count: usize,
    /// Number of duplicate `(document_id, content_hash)` items collapsed before executing SQL.
    pub deduplicated_count: usize,
    /// Number of rows inserted or updated in `document_chunks`.
    pub upserted_rows: usize,
    /// Persisted rows returned from PostgreSQL.
    pub chunks: Vec<StoredDocumentChunk>,
}

/// Format an `f32` embedding slice into a PostgreSQL `pgvector` text literal (`"[v0,v1,...]"`),
/// strictly validating length and finiteness so invalid payloads never reach PostgreSQL.
pub fn format_pgvector(
    values: &[f32],
    expected_dimensions: usize,
    chunk_index: usize,
) -> Result<String, ChunkRepoError> {
    if values.len() != expected_dimensions {
        return Err(ChunkRepoError::DimensionMismatch {
            index: chunk_index,
            expected: expected_dimensions,
            actual: values.len(),
        });
    }

    let mut out = String::with_capacity(values.len().saturating_mul(10).saturating_add(2));
    out.push('[');
    for (i, &v) in values.iter().enumerate() {
        if !v.is_finite() {
            return Err(ChunkRepoError::NonFiniteEmbedding { index: chunk_index });
        }
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "{v}");
    }
    out.push(']');
    Ok(out)
}

/// Compute cosine similarity between two equal-length vectors in pure Rust.
///
/// Returns `None` if lengths differ, either vector contains non-finite floats,
/// or either vector has zero L2 norm.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }

    let mut dot = 0.0_f64;
    let mut norm_a = 0.0_f64;
    let mut norm_b = 0.0_f64;

    for (&x, &y) in a.iter().zip(b.iter()) {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let xf = f64::from(x);
        let yf = f64::from(y);
        dot += xf * yf;
        norm_a += xf * xf;
        norm_b += yf * yf;
    }

    let denom = (norm_a * norm_b).sqrt();
    if denom <= f64::EPSILON {
        return None;
    }

    Some((dot / denom).clamp(-1.0, 1.0))
}

fn validate_search_query(
    query_vector: &[f32],
    min_similarity: f32,
) -> Result<(String, f64), ChunkRepoError> {
    if !min_similarity.is_finite() || !(-1.0..=1.0).contains(&min_similarity) {
        return Err(ChunkRepoError::InvalidQuery(format!(
            "min_similarity must be finite and within [-1.0, 1.0], got {min_similarity}"
        )));
    }

    let literal = format_pgvector(query_vector, EMBEDDING_DIMENSIONS, 0)?;

    // pgvector's cosine distance `<=>` yields NaN for a zero-norm vector; reject upfront.
    let norm_sq: f64 = query_vector
        .iter()
        .map(|&v| {
            let f = f64::from(v);
            f * f
        })
        .sum();
    if norm_sq <= f64::EPSILON {
        return Err(ChunkRepoError::InvalidQuery(
            "query_vector cannot be the zero vector for cosine similarity search".to_string(),
        ));
    }

    let max_distance = (1.0_f64 - f64::from(min_similarity)).clamp(0.0, 2.0);
    Ok((literal, max_distance))
}

fn validate_chunk(index: usize, chunk: &NewDocumentChunk) -> Result<(), ChunkRepoError> {
    if chunk.document_id.trim().is_empty() {
        return Err(ChunkRepoError::InvalidChunk {
            index,
            reason: "document_id cannot be empty".to_string(),
        });
    }
    if chunk.content_hash.trim().is_empty() {
        return Err(ChunkRepoError::InvalidChunk {
            index,
            reason: "content_hash cannot be empty".to_string(),
        });
    }
    if chunk.chunk_index < 0 {
        return Err(ChunkRepoError::InvalidChunk {
            index,
            reason: "chunk_index cannot be negative".to_string(),
        });
    }
    if chunk.token_count < 0 {
        return Err(ChunkRepoError::InvalidChunk {
            index,
            reason: "token_count cannot be negative".to_string(),
        });
    }
    if chunk.embedding.len() != EMBEDDING_DIMENSIONS {
        return Err(ChunkRepoError::DimensionMismatch {
            index,
            expected: EMBEDDING_DIMENSIONS,
            actual: chunk.embedding.len(),
        });
    }
    if chunk.embedding.iter().any(|v| !v.is_finite()) {
        return Err(ChunkRepoError::NonFiniteEmbedding { index });
    }
    Ok(())
}

/// Deduplicate chunks sharing the same `(document_id, content_hash)` key within a single batch.
///
/// PostgreSQL's `INSERT ... ON CONFLICT DO UPDATE` fails with SQLSTATE `21000`
/// ("ON CONFLICT DO UPDATE command cannot affect row a second time") if the same statement
/// contains duplicate conflict keys. Keeping the last occurrence per key in Rust prevents
/// this failure deterministically while preserving relative order.
fn deduplicate_chunks(chunks: &[NewDocumentChunk]) -> (Vec<&NewDocumentChunk>, usize) {
    let mut key_to_pos: HashMap<(&str, &str), usize> = HashMap::with_capacity(chunks.len());
    let mut unique: Vec<&NewDocumentChunk> = Vec::with_capacity(chunks.len());
    let mut duplicates = 0_usize;

    for chunk in chunks {
        let key = (chunk.document_id.as_str(), chunk.content_hash.as_str());
        if let Some(&pos) = key_to_pos.get(&key) {
            unique[pos] = chunk;
            duplicates += 1;
        } else {
            key_to_pos.insert(key, unique.len());
            unique.push(chunk);
        }
    }

    (unique, duplicates)
}

/// Repository for bulk-ingesting and querying vectorized `document_chunks` in PostgreSQL.
pub struct ChunkRepository;

impl ChunkRepository {
    /// Bulk upsert document chunks using PostgreSQL `UNNEST(...)` with
    /// `ON CONFLICT (document_id, content_hash) DO UPDATE`.
    ///
    /// - Validates every chunk and embedding dimension (`1536`) before opening a transaction.
    /// - Deduplicates `(document_id, content_hash)` inside the batch to prevent SQLSTATE `21000`.
    /// - Executes sub-batches of up to [`DEFAULT_UPSERT_BATCH_SIZE`] inside a single atomic transaction.
    pub async fn batch_insert_chunks(
        pool: &PgPool,
        chunks: &[NewDocumentChunk],
    ) -> Result<BatchUpsertSummary, ChunkRepoError> {
        if chunks.is_empty() {
            return Ok(BatchUpsertSummary {
                input_count: 0,
                deduplicated_count: 0,
                upserted_rows: 0,
                chunks: Vec::new(),
            });
        }

        for (idx, chunk) in chunks.iter().enumerate() {
            validate_chunk(idx, chunk)?;
        }

        let (unique_chunks, deduplicated_count) = deduplicate_chunks(chunks);
        let mut tx = pool.begin().await?;
        let mut persisted: Vec<StoredDocumentChunk> = Vec::with_capacity(unique_chunks.len());

        for batch in unique_chunks.chunks(DEFAULT_UPSERT_BATCH_SIZE) {
            let mut document_ids = Vec::with_capacity(batch.len());
            let mut chunk_indices = Vec::with_capacity(batch.len());
            let mut contents = Vec::with_capacity(batch.len());
            let mut metadatas = Vec::with_capacity(batch.len());
            let mut content_hashes = Vec::with_capacity(batch.len());
            let mut token_counts = Vec::with_capacity(batch.len());
            let mut embeddings = Vec::with_capacity(batch.len());

            for (idx, chunk) in batch.iter().enumerate() {
                document_ids.push(chunk.document_id.as_str());
                chunk_indices.push(chunk.chunk_index);
                contents.push(chunk.content.as_str());
                metadatas.push(chunk.metadata.clone());
                content_hashes.push(chunk.content_hash.as_str());
                token_counts.push(chunk.token_count);
                embeddings.push(format_pgvector(
                    &chunk.embedding,
                    EMBEDDING_DIMENSIONS,
                    idx,
                )?);
            }

            let rows: Vec<StoredDocumentChunk> = sqlx::query_as(
                r#"
                INSERT INTO document_chunks (
                    document_id,
                    chunk_index,
                    content,
                    metadata,
                    content_hash,
                    token_count,
                    embedding
                )
                SELECT
                    u.document_id,
                    u.chunk_index,
                    u.content,
                    u.metadata,
                    u.content_hash,
                    u.token_count,
                    u.embedding::vector(1536)
                FROM UNNEST(
                    $1::text[],
                    $2::int4[],
                    $3::text[],
                    $4::jsonb[],
                    $5::text[],
                    $6::int4[],
                    $7::text[]
                ) AS u(
                    document_id,
                    chunk_index,
                    content,
                    metadata,
                    content_hash,
                    token_count,
                    embedding
                )
                ON CONFLICT (document_id, content_hash) DO UPDATE
                SET
                    chunk_index = EXCLUDED.chunk_index,
                    content     = EXCLUDED.content,
                    metadata    = EXCLUDED.metadata,
                    token_count = EXCLUDED.token_count,
                    embedding   = EXCLUDED.embedding,
                    updated_at  = now()
                RETURNING
                    id,
                    document_id,
                    chunk_index,
                    content,
                    metadata,
                    content_hash,
                    token_count,
                    created_at,
                    updated_at
                "#,
            )
            .bind(&document_ids)
            .bind(&chunk_indices)
            .bind(&contents)
            .bind(&metadatas)
            .bind(&content_hashes)
            .bind(&token_counts)
            .bind(&embeddings)
            .fetch_all(&mut *tx)
            .await?;

            persisted.extend(rows);
        }

        tx.commit().await?;

        Ok(BatchUpsertSummary {
            input_count: chunks.len(),
            deduplicated_count,
            upserted_rows: persisted.len(),
            chunks: persisted,
        })
    }

    /// Perform Top-K cosine similarity search using pgvector's `<=>` operator and HNSW index.
    ///
    /// Returns chunks ordered from highest similarity (`1.0 - distance`) to lowest,
    /// filtering out candidates below `min_similarity`.
    pub async fn search_similar_chunks(
        pool: &PgPool,
        query_vector: &[f32],
        limit: usize,
        min_similarity: f32,
    ) -> Result<Vec<SimilarChunk>, ChunkRepoError> {
        Self::search_similar_chunks_filtered(pool, query_vector, limit, min_similarity, None).await
    }

    /// Perform Top-K cosine similarity search with an optional `document_id` scope filter.
    pub async fn search_similar_chunks_filtered(
        pool: &PgPool,
        query_vector: &[f32],
        limit: usize,
        min_similarity: f32,
        document_id: Option<&str>,
    ) -> Result<Vec<SimilarChunk>, ChunkRepoError> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let (query_literal, max_distance) = validate_search_query(query_vector, min_similarity)?;
        let clamped_limit =
            i64::try_from(limit.min(MAX_SEARCH_LIMIT)).unwrap_or(MAX_SEARCH_LIMIT as i64);
        let doc_filter = document_id.map(str::trim).filter(|s| !s.is_empty());

        // Ordering by `embedding <=> $1::vector(1536) ASC LIMIT $2` inside the CTE allows
        // PostgreSQL to utilize `idx_document_chunks_embedding_hnsw` (vector_cosine_ops),
        // then applies the similarity threshold cleanly on the candidate set.
        let rows: Vec<SimilarChunk> = sqlx::query_as(
            r#"
            WITH nearest AS (
                SELECT
                    id,
                    document_id,
                    chunk_index,
                    content,
                    metadata,
                    content_hash,
                    token_count,
                    created_at,
                    updated_at,
                    (embedding <=> $1::vector(1536))::float8 AS cosine_distance
                FROM document_chunks
                WHERE ($4::text IS NULL OR document_id = $4)
                ORDER BY embedding <=> $1::vector(1536) ASC
                LIMIT $2
            )
            SELECT
                id,
                document_id,
                chunk_index,
                content,
                metadata,
                content_hash,
                token_count,
                created_at,
                updated_at,
                cosine_distance,
                (1.0::float8 - cosine_distance) AS similarity
            FROM nearest
            WHERE cosine_distance <= $3::float8
            ORDER BY cosine_distance ASC, id ASC
            "#,
        )
        .bind(&query_literal)
        .bind(clamped_limit)
        .bind(max_distance)
        .bind(doc_filter)
        .fetch_all(pool)
        .await?;

        Ok(rows)
    }

    /// List all stored chunks belonging to a single document, ordered by `chunk_index`.
    pub async fn list_chunks_by_document(
        pool: &PgPool,
        document_id: &str,
    ) -> Result<Vec<StoredDocumentChunk>, ChunkRepoError> {
        let rows: Vec<StoredDocumentChunk> = sqlx::query_as(
            r#"
            SELECT
                id,
                document_id,
                chunk_index,
                content,
                metadata,
                content_hash,
                token_count,
                created_at,
                updated_at
            FROM document_chunks
            WHERE document_id = $1
            ORDER BY chunk_index ASC, created_at ASC
            "#,
        )
        .bind(document_id)
        .fetch_all(pool)
        .await?;

        Ok(rows)
    }

    /// Delete all stored chunks belonging to a single document and return the affected row count.
    pub async fn delete_chunks_by_document(
        pool: &PgPool,
        document_id: &str,
    ) -> Result<u64, ChunkRepoError> {
        let res = sqlx::query("DELETE FROM document_chunks WHERE document_id = $1")
            .bind(document_id)
            .execute(pool)
            .await?;
        Ok(res.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_embedding(x: f32, y: f32) -> Vec<f32> {
        let mut vec = vec![0.0_f32; EMBEDDING_DIMENSIONS];
        vec[0] = x;
        vec[1] = y;
        vec
    }

    fn sample_chunk(doc: &str, idx: i32, hash: &str, content: &str) -> NewDocumentChunk {
        NewDocumentChunk {
            document_id: doc.to_string(),
            chunk_index: idx,
            content: content.to_string(),
            metadata: json!({ "source": doc, "chunk_index": idx }),
            content_hash: hash.to_string(),
            token_count: 42,
            embedding: sample_embedding(0.5, 0.5),
        }
    }

    #[test]
    fn format_pgvector_validates_dimension_and_finiteness() {
        let formatted = format_pgvector(&[0.25, -0.5, 1.0], 3, 0).unwrap();
        assert_eq!(formatted, "[0.25,-0.5,1]");

        let dim_err = format_pgvector(&[0.1, 0.2], 3, 2).unwrap_err();
        assert!(matches!(
            dim_err,
            ChunkRepoError::DimensionMismatch {
                index: 2,
                expected: 3,
                actual: 2
            }
        ));

        let nan_err = format_pgvector(&[0.1, f32::NAN, 0.3], 3, 1).unwrap_err();
        assert!(matches!(
            nan_err,
            ChunkRepoError::NonFiniteEmbedding { index: 1 }
        ));

        let inf_err = format_pgvector(&[f32::INFINITY, 0.2, 0.3], 3, 0).unwrap_err();
        assert!(matches!(
            inf_err,
            ChunkRepoError::NonFiniteEmbedding { index: 0 }
        ));
    }

    #[test]
    fn cosine_similarity_and_search_query_validation() {
        let a = sample_embedding(1.0, 0.0);
        let b = sample_embedding(0.8, 0.6);
        let c = sample_embedding(0.0, 1.0);
        let d = sample_embedding(-1.0, 0.0);

        assert!((cosine_similarity(&a, &a).unwrap() - 1.0).abs() < 1e-6);
        assert!((cosine_similarity(&a, &b).unwrap() - 0.8).abs() < 1e-6);
        assert!(cosine_similarity(&a, &c).unwrap().abs() < 1e-6);
        assert!((cosine_similarity(&a, &d).unwrap() + 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&[0.0; 4], &[1.0, 0.0, 0.0, 0.0]).is_none());

        let (_, max_dist) = validate_search_query(&a, 0.75).unwrap();
        assert!((max_dist - 0.25).abs() < 1e-6);

        // Zero vector must be rejected before pgvector produces NaN cosine distance.
        let zero = vec![0.0_f32; EMBEDDING_DIMENSIONS];
        assert!(matches!(
            validate_search_query(&zero, 0.5),
            Err(ChunkRepoError::InvalidQuery(_))
        ));

        // Out-of-range or NaN similarity threshold must be rejected.
        assert!(matches!(
            validate_search_query(&a, 1.5),
            Err(ChunkRepoError::InvalidQuery(_))
        ));
        assert!(matches!(
            validate_search_query(&a, f32::NAN),
            Err(ChunkRepoError::InvalidQuery(_))
        ));
    }

    #[test]
    fn deduplicate_chunks_keeps_latest_entry_per_doc_hash() {
        let chunks = vec![
            sample_chunk("doc-a", 0, "hash-1", "old version"),
            sample_chunk("doc-a", 1, "hash-2", "second chunk"),
            sample_chunk("doc-a", 0, "hash-1", "updated version"),
            sample_chunk("doc-b", 0, "hash-1", "different doc same hash"),
        ];

        let (unique, dup_count) = deduplicate_chunks(&chunks);
        assert_eq!(dup_count, 1);
        assert_eq!(unique.len(), 3);
        assert_eq!(unique[0].content, "updated version");
        assert_eq!(unique[1].content, "second chunk");
        assert_eq!(unique[2].document_id, "doc-b");
    }

    #[test]
    fn validate_chunk_rejects_empty_ids_and_negative_fields() {
        let mut bad_doc = sample_chunk("   ", 0, "h1", "text");
        assert!(matches!(
            validate_chunk(0, &bad_doc),
            Err(ChunkRepoError::InvalidChunk { index: 0, .. })
        ));

        bad_doc.document_id = "doc-1".to_string();
        bad_doc.content_hash = "".to_string();
        assert!(matches!(
            validate_chunk(1, &bad_doc),
            Err(ChunkRepoError::InvalidChunk { index: 1, .. })
        ));

        bad_doc.content_hash = "h1".to_string();
        bad_doc.chunk_index = -1;
        assert!(matches!(
            validate_chunk(2, &bad_doc),
            Err(ChunkRepoError::InvalidChunk { index: 2, .. })
        ));

        bad_doc.chunk_index = 0;
        bad_doc.token_count = -5;
        assert!(matches!(
            validate_chunk(3, &bad_doc),
            Err(ChunkRepoError::InvalidChunk { index: 3, .. })
        ));
    }

    #[tokio::test]
    async fn batch_insert_and_search_short_circuit_invalid_inputs_before_db() {
        let lazy_pool = crate::connect_lazy("postgres://user:pass@127.0.0.1:5432/nonexistent")
            .expect("lazy pool creation should not connect");

        let empty = ChunkRepository::batch_insert_chunks(&lazy_pool, &[])
            .await
            .unwrap();
        assert_eq!(empty.input_count, 0);
        assert_eq!(empty.upserted_rows, 0);
        assert!(empty.chunks.is_empty());

        let mut bad_dim = sample_chunk("doc-1", 0, "h1", "hello");
        bad_dim.embedding = vec![0.1; 768];
        let err = ChunkRepository::batch_insert_chunks(&lazy_pool, &[bad_dim])
            .await
            .expect_err("wrong dimension must fail before touching PostgreSQL");
        assert!(matches!(
            err,
            ChunkRepoError::DimensionMismatch {
                index: 0,
                expected: EMBEDDING_DIMENSIONS,
                actual: 768
            }
        ));

        // limit = 0 short-circuits cleanly.
        let zero_limit =
            ChunkRepository::search_similar_chunks(&lazy_pool, &sample_embedding(1.0, 0.0), 0, 0.5)
                .await
                .unwrap();
        assert!(zero_limit.is_empty());

        // Zero-norm query vector fails before touching PostgreSQL.
        let zero_vec = vec![0.0_f32; EMBEDDING_DIMENSIONS];
        let search_err = ChunkRepository::search_similar_chunks(&lazy_pool, &zero_vec, 5, 0.5)
            .await
            .expect_err("zero query vector must fail before DB");
        assert!(matches!(search_err, ChunkRepoError::InvalidQuery(_)));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn batch_insert_chunks_upserts_deduplicates_and_updates_on_conflict(pool: PgPool) {
        let initial_batch = vec![
            sample_chunk("docs/rag.md", 0, "blake3-aaa", "initial chunk 0"),
            sample_chunk("docs/rag.md", 1, "blake3-bbb", "initial chunk 1"),
            // Intra-batch duplicate for (docs/rag.md, blake3-aaa): must collapse cleanly without SQLSTATE 21000.
            sample_chunk("docs/rag.md", 0, "blake3-aaa", "deduped chunk 0"),
        ];

        let first = ChunkRepository::batch_insert_chunks(&pool, &initial_batch)
            .await
            .unwrap();
        assert_eq!(first.input_count, 3);
        assert_eq!(first.deduplicated_count, 1);
        assert_eq!(first.upserted_rows, 2);

        let listed = ChunkRepository::list_chunks_by_document(&pool, "docs/rag.md")
            .await
            .unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].content, "deduped chunk 0");
        assert_eq!(listed[1].content, "initial chunk 1");
        let original_id = listed[0].id;

        // Re-upsert same (document_id, content_hash) with updated metadata & chunk_index.
        let mut updated = sample_chunk("docs/rag.md", 2, "blake3-aaa", "refreshed chunk 0");
        updated.metadata = json!({ "source": "docs/rag.md", "updated": true });
        updated.token_count = 99;
        updated.embedding = sample_embedding(0.9, 0.1);

        let second = ChunkRepository::batch_insert_chunks(&pool, &[updated])
            .await
            .unwrap();
        assert_eq!(second.upserted_rows, 1);
        assert_eq!(second.chunks[0].id, original_id);
        assert_eq!(second.chunks[0].chunk_index, 2);
        assert_eq!(second.chunks[0].content, "refreshed chunk 0");
        assert_eq!(second.chunks[0].token_count, 99);
        assert_eq!(second.chunks[0].metadata["updated"], true);

        let deleted = ChunkRepository::delete_chunks_by_document(&pool, "docs/rag.md")
            .await
            .unwrap();
        assert_eq!(deleted, 2);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn search_similar_chunks_ranks_by_cosine_similarity_and_applies_threshold(pool: PgPool) {
        let mut exact = sample_chunk("doc-a", 0, "h-exact", "exact match");
        exact.embedding = sample_embedding(1.0, 0.0);

        let mut close = sample_chunk("doc-b", 0, "h-close", "close match");
        close.embedding = sample_embedding(0.8, 0.6);

        let mut orthogonal = sample_chunk("doc-a", 1, "h-ortho", "orthogonal match");
        orthogonal.embedding = sample_embedding(0.0, 1.0);

        let mut opposite = sample_chunk("doc-b", 1, "h-opposite", "opposite match");
        opposite.embedding = sample_embedding(-1.0, 0.0);

        ChunkRepository::batch_insert_chunks(&pool, &[exact, close, orthogonal, opposite])
            .await
            .unwrap();

        let query = sample_embedding(1.0, 0.0);

        // Threshold 0.5 should return exact (~1.0) then close (~0.8) only.
        let hits = ChunkRepository::search_similar_chunks(&pool, &query, 10, 0.5)
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].content, "exact match");
        assert!((hits[0].similarity - 1.0).abs() < 1e-5);
        assert!(hits[0].cosine_distance.abs() < 1e-5);
        assert_eq!(hits[1].content, "close match");
        assert!((hits[1].similarity - 0.8).abs() < 1e-5);

        // Top-1 limit returns only exact match.
        let top1 = ChunkRepository::search_similar_chunks(&pool, &query, 1, 0.0)
            .await
            .unwrap();
        assert_eq!(top1.len(), 1);
        assert_eq!(top1[0].content, "exact match");

        // Document scope filter returns only doc-b's matching chunk.
        let scoped =
            ChunkRepository::search_similar_chunks_filtered(&pool, &query, 10, 0.5, Some("doc-b"))
                .await
                .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].document_id, "doc-b");
        assert_eq!(scoped[0].content, "close match");
    }
}
