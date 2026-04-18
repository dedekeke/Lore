use chrono::{DateTime, Utc};
use pgvector::Vector;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct RuleChunkLink {
    pub id: Uuid,
    pub rule_id: Uuid,
    pub chunk_id: Uuid,
    pub similarity: f64,
    pub created_at: DateTime<Utc>,
}

pub async fn create_link(
    pool: &PgPool,
    rule_id: Uuid,
    chunk_id: Uuid,
    similarity: f64,
) -> Result<Uuid, sqlx::Error> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO ai_memory.rule_chunk_links (rule_id, chunk_id, similarity) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (rule_id, chunk_id) DO UPDATE SET similarity = EXCLUDED.similarity \
         RETURNING id",
    )
    .bind(rule_id)
    .bind(chunk_id)
    .bind(similarity)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// Return rules linked to a specific code chunk
pub async fn get_rules_for_chunk(
    pool: &PgPool,
    chunk_id: Uuid,
) -> Result<Vec<LinkedRule>, sqlx::Error> {
    sqlx::query_as(
        "SELECT r.id AS rule_id, r.category::text AS category, r.content, \
                l.similarity, l.created_at AS linked_at \
         FROM ai_memory.rule_chunk_links l \
         JOIN ai_memory.semantic_rules r ON r.id = l.rule_id \
         WHERE l.chunk_id = $1 \
         ORDER BY l.similarity DESC",
    )
    .bind(chunk_id)
    .fetch_all(pool)
    .await
}

/// Return rules linked to code chunks in a given file path
pub async fn get_rules_for_file(
    pool: &PgPool,
    file_path: &str,
    project_id: Uuid,
) -> Result<Vec<FileRule>, sqlx::Error> {
    sqlx::query_as(
        "SELECT DISTINCT ON (r.id) r.id AS rule_id, r.category::text AS category, r.content, \
                c.file_path, l.similarity \
         FROM ai_memory.rule_chunk_links l \
         JOIN ai_memory.semantic_rules r ON r.id = l.rule_id \
         JOIN ai_memory.code_chunks c ON c.id = l.chunk_id \
         WHERE c.file_path = $1 AND c.project_id = $2 \
         ORDER BY r.id, l.similarity DESC",
    )
    .bind(file_path)
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// Auto-link rules to code chunks based on embedding similarity.
/// Given a code snippet from an accepted attempt:
/// 1. Embed the snippet and find similar code_chunks (>= threshold)
/// 2. Find rules from the task (via source_task_id) or all project rules
/// 3. Create links between matched chunks and relevant rules
pub async fn link_rules_from_code(
    pool: &PgPool,
    task_id: Uuid,
    code_embedding: &[f32],
    project_id: Uuid,
    threshold: f64,
) -> Result<u64, sqlx::Error> {
    let emb = Vector::from(code_embedding.to_vec());

    // Find code_chunks similar to the code snippet embedding
    let similar_chunks: Vec<(Uuid, f64)> = sqlx::query_as(
        "SELECT id, (1.0 - (embedding <=> $1::vector))::float8 AS similarity \
         FROM ai_memory.code_chunks \
         WHERE project_id = $2 AND embedding IS NOT NULL \
           AND (1.0 - (embedding <=> $1::vector)) >= $3 \
         ORDER BY embedding <=> $1::vector \
         LIMIT 20",
    )
    .bind(&emb)
    .bind(project_id)
    .bind(threshold)
    .fetch_all(pool)
    .await?;

    if similar_chunks.is_empty() {
        return Ok(0);
    }

    // Find rules linked to this task or project-level rules
    let rules: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM ai_memory.semantic_rules \
         WHERE project_id = $1 AND (source_task_id = $2 OR source_task_id IS NULL)",
    )
    .bind(project_id)
    .bind(task_id)
    .fetch_all(pool)
    .await?;

    if rules.is_empty() {
        return Ok(0);
    }

    // Batch insert links (rule_id, chunk_id pairs)
    let mut total = 0u64;
    let mut rule_ids = Vec::new();
    let mut chunk_ids = Vec::new();
    let mut similarities = Vec::new();

    for (chunk_id, sim) in &similar_chunks {
        for (rule_id,) in &rules {
            rule_ids.push(*rule_id);
            chunk_ids.push(*chunk_id);
            similarities.push(*sim);
        }
    }

    // Batch in groups to avoid parameter limits
    for batch_start in (0..rule_ids.len()).step_by(500) {
        let batch_end = (batch_start + 500).min(rule_ids.len());
        let r = &rule_ids[batch_start..batch_end];
        let c = &chunk_ids[batch_start..batch_end];
        let s = &similarities[batch_start..batch_end];

        let result = sqlx::query(
            "INSERT INTO ai_memory.rule_chunk_links (rule_id, chunk_id, similarity) \
             SELECT * FROM UNNEST($1::uuid[], $2::uuid[], $3::float8[]) \
             ON CONFLICT (rule_id, chunk_id) DO UPDATE SET similarity = EXCLUDED.similarity",
        )
        .bind(r)
        .bind(c)
        .bind(s)
        .execute(pool)
        .await?;
        total += result.rows_affected();
    }

    Ok(total)
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct LinkedRule {
    pub rule_id: Uuid,
    pub category: String,
    pub content: String,
    pub similarity: f64,
    pub linked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct FileRule {
    pub rule_id: Uuid,
    pub category: String,
    pub content: String,
    pub file_path: String,
    pub similarity: f64,
}
