ALTER TABLE ai_memory.code_chunks ADD COLUMN IF NOT EXISTS chunk_name TEXT;
ALTER TABLE ai_memory.code_chunks ADD COLUMN IF NOT EXISTS chunk_kind TEXT;
