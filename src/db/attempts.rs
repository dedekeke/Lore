use chrono::{DateTime, Utc};
use pgvector::Vector;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "ai_memory.attempt_outcome", rename_all = "snake_case")]
pub enum AttemptOutcome {
    Pending,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct Attempt {
    pub id: Uuid,
    pub task_id: Uuid,
    pub approach_summary: String,
    pub code_snippet: Option<String>,
    pub outcome: AttemptOutcome,
    pub reasoning: String,
    #[serde(skip)]
    #[allow(dead_code)]
    pub reasoning_embedding: Option<Vector>,
    pub git_ref: Option<String>,
    pub token_cost: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

pub async fn create_attempt(
    pool: &PgPool,
    task_id: Uuid,
    approach_summary: &str,
    code_snippet: Option<&str>,
) -> Result<Uuid, sqlx::Error> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO ai_memory.attempts (task_id, approach_summary, code_snippet) \
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(task_id)
    .bind(approach_summary)
    .bind(code_snippet)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

#[allow(dead_code)]
pub async fn get_attempt(pool: &PgPool, id: Uuid) -> Result<Option<Attempt>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, task_id, approach_summary, code_snippet, outcome, reasoning, \
         reasoning_embedding, git_ref, token_cost, created_at, resolved_at \
         FROM ai_memory.attempts WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn list_attempts(
    pool: &PgPool,
    task_id: Uuid,
    outcome: Option<AttemptOutcome>,
) -> Result<Vec<Attempt>, sqlx::Error> {
    match outcome {
        Some(o) => {
            sqlx::query_as(
                "SELECT id, task_id, approach_summary, code_snippet, outcome, reasoning, \
                 reasoning_embedding, git_ref, token_cost, created_at, resolved_at \
                 FROM ai_memory.attempts WHERE task_id = $1 AND outcome = $2 ORDER BY created_at",
            )
            .bind(task_id)
            .bind(&o)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as(
                "SELECT id, task_id, approach_summary, code_snippet, outcome, reasoning, \
                 reasoning_embedding, git_ref, token_cost, created_at, resolved_at \
                 FROM ai_memory.attempts WHERE task_id = $1 ORDER BY created_at",
            )
            .bind(task_id)
            .fetch_all(pool)
            .await
        }
    }
}

pub async fn log_outcome(
    pool: &PgPool,
    id: Uuid,
    outcome: AttemptOutcome,
    reasoning: &str,
    reasoning_embedding: Option<&[f32]>,
    git_ref: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let emb = reasoning_embedding.map(|e| Vector::from(e.to_vec()));
    let result = sqlx::query(
        "UPDATE ai_memory.attempts \
         SET outcome = $2, reasoning = $3, reasoning_embedding = $4, git_ref = $5, resolved_at = NOW() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(&outcome)
    .bind(reasoning)
    .bind(emb.as_ref())
    .bind(git_ref)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn search_similar_failures(
    pool: &PgPool,
    project_id: Uuid,
    embedding: &[f32],
    limit: i64,
) -> Result<Vec<Attempt>, sqlx::Error> {
    let emb = Vector::from(embedding.to_vec());
    sqlx::query_as(
        "SELECT a.id, a.task_id, a.approach_summary, a.code_snippet, a.outcome, a.reasoning, \
         a.reasoning_embedding, a.git_ref, a.token_cost, a.created_at, a.resolved_at \
         FROM ai_memory.attempts a \
         JOIN ai_memory.tasks t ON a.task_id = t.id \
         WHERE a.outcome = 'rejected' AND a.reasoning_embedding IS NOT NULL AND t.project_id = $3 \
         ORDER BY a.reasoning_embedding <=> $1::vector LIMIT $2",
    )
    .bind(&emb)
    .bind(limit)
    .bind(project_id)
    .fetch_all(pool)
    .await
}
