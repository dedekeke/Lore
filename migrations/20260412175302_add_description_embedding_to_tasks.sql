ALTER TABLE ai_memory.tasks ADD COLUMN IF NOT EXISTS description_embedding vector(384);
