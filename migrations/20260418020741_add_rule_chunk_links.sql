CREATE TABLE IF NOT EXISTS ai_memory.rule_chunk_links (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    rule_id UUID NOT NULL REFERENCES ai_memory.semantic_rules(id) ON DELETE CASCADE,
    chunk_id UUID NOT NULL REFERENCES ai_memory.code_chunks(id) ON DELETE CASCADE,
    similarity FLOAT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(rule_id, chunk_id)
);
CREATE INDEX idx_rule_chunk_links_rule ON ai_memory.rule_chunk_links(rule_id);
CREATE INDEX idx_rule_chunk_links_chunk ON ai_memory.rule_chunk_links(chunk_id);
