-- Enable pgvector extension for high-dimensional vector similarity search.
CREATE EXTENSION IF NOT EXISTS vector;

-- Indexed document chunks with structured provenance metadata and 1536-dim embeddings
-- (compatible with OpenAI text-embedding-3-small / ada-002 and dimension-aligned models).
CREATE TABLE document_chunks (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    document_id  TEXT NOT NULL,
    chunk_index  INT NOT NULL CHECK (chunk_index >= 0),
    content      TEXT NOT NULL,
    metadata     JSONB NOT NULL DEFAULT '{}'::jsonb,
    content_hash TEXT NOT NULL,
    token_count  INT NOT NULL DEFAULT 0 CHECK (token_count >= 0),
    embedding    vector(1536) NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- Supports idempotent ON CONFLICT (document_id, content_hash) DO UPDATE upserts
    -- and Blake3-based incremental garbage collection per document.
    CONSTRAINT uq_document_chunks_doc_hash UNIQUE (document_id, content_hash)
);

-- Fast ordered lookup and stale-chunk cleanup scoped to a single document.
CREATE INDEX idx_document_chunks_document_id
    ON document_chunks (document_id, chunk_index);

-- Fast containment queries against chunk metadata (file_path, language, symbol_name, etc.).
CREATE INDEX idx_document_chunks_metadata_gin
    ON document_chunks USING gin (metadata jsonb_path_ops);

-- HNSW (Hierarchical Navigable Small World) approximate nearest-neighbor index
-- using cosine distance (<=>). m=16, ef_construction=64 are balanced production defaults.
CREATE INDEX idx_document_chunks_embedding_hnsw
    ON document_chunks
    USING hnsw (embedding vector_cosine_ops)
    WITH (m = 16, ef_construction = 64);
