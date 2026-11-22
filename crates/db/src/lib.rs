pub mod chunks;
pub mod conversations;
pub mod message;
pub mod models;

pub use chunks::{
    BatchUpsertSummary, ChunkRepoError, ChunkRepository, DEFAULT_UPSERT_BATCH_SIZE,
    EMBEDDING_DIMENSIONS, NewDocumentChunk, StoredDocumentChunk, format_pgvector,
};
pub use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

/// Embedded SQLx migrator pointing at the workspace `migrations/` directory.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await
}

pub fn connect_lazy(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new().connect_lazy(database_url)
}

/// Run all pending SQL migrations against the provided PostgreSQL pool.
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_migrations_include_pgvector_document_chunks() {
        let migration = MIGRATOR
            .iter()
            .find(|m| m.description.contains("pgvector"))
            .expect("pgvector migration must be embedded in MIGRATOR");

        assert!(
            migration
                .sql
                .contains("CREATE EXTENSION IF NOT EXISTS vector"),
            "migration must enable pgvector extension"
        );
        assert!(
            migration.sql.contains("CREATE TABLE document_chunks"),
            "migration must create document_chunks table"
        );
        assert!(
            migration.sql.contains("vector(1536)"),
            "migration must declare 1536-dim vector column"
        );
        assert!(
            migration
                .sql
                .contains("USING hnsw (embedding vector_cosine_ops)"),
            "migration must create cosine HNSW index"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn pgvector_extension_and_hnsw_index_are_installed(pool: PgPool) {
        let has_vector: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'vector')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(has_vector, "pgvector extension must be enabled");

        let has_hnsw_index: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1
                FROM pg_indexes
                WHERE tablename = 'document_chunks'
                  AND indexname = 'idx_document_chunks_embedding_hnsw'
                  AND indexdef ILIKE '%USING hnsw%'
            )",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(has_hnsw_index, "HNSW index on document_chunks must exist");
    }
}
