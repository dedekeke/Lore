CREATE TABLE ai_memory.code_chunks (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id  UUID NOT NULL REFERENCES ai_memory.projects(id) ON DELETE CASCADE,
    file_path   TEXT NOT NULL,
    start_line  INTEGER NOT NULL,
    end_line    INTEGER NOT NULL,
    language    TEXT,
    content     TEXT NOT NULL,
    embedding   vector(384),
    file_hash   TEXT NOT NULL,
    content_tsv tsvector GENERATED ALWAYS AS (to_tsvector('english', content)) STORED,
    indexed_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX idx_code_chunks_unique ON ai_memory.code_chunks(project_id, file_path, start_line);
CREATE INDEX idx_code_chunks_project ON ai_memory.code_chunks(project_id);
CREATE INDEX idx_code_chunks_file ON ai_memory.code_chunks(project_id, file_path);
CREATE INDEX idx_code_chunks_embedding ON ai_memory.code_chunks USING hnsw (embedding vector_cosine_ops);
CREATE INDEX idx_code_chunks_tsv ON ai_memory.code_chunks USING gin (content_tsv);
