CREATE SCHEMA IF NOT EXISTS ai_memory;
CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE ai_memory.projects (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    root_path TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
