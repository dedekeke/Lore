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
    Unknown,
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
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub resolved_by_agent_id: Option<String>,
    pub resolved_by_session_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

pub async fn create_attempt(
    pool: &PgPool,
    task_id: Uuid,
    approach_summary: &str,
    agent_id: Option<&str>,
    session_id: Option<&str>,
    git_ref: Option<&str>,
) -> Result<Uuid, sqlx::Error> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO ai_memory.attempts (task_id, approach_summary, agent_id, session_id, git_ref) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(task_id)
    .bind(approach_summary)
    .bind(agent_id)
    .bind(session_id)
    .bind(git_ref)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

#[allow(dead_code)]
pub async fn get_attempt(pool: &PgPool, id: Uuid) -> Result<Option<Attempt>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, task_id, approach_summary, code_snippet, outcome, reasoning, \
         reasoning_embedding, git_ref, token_cost, agent_id, session_id, \
         resolved_by_agent_id, resolved_by_session_id, created_at, resolved_at \
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
                 reasoning_embedding, git_ref, token_cost, agent_id, session_id, \
                 resolved_by_agent_id, resolved_by_session_id, created_at, resolved_at \
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
                 reasoning_embedding, git_ref, token_cost, agent_id, session_id, \
                 resolved_by_agent_id, resolved_by_session_id, created_at, resolved_at \
                 FROM ai_memory.attempts WHERE task_id = $1 ORDER BY created_at",
            )
            .bind(task_id)
            .fetch_all(pool)
            .await
        }
    }
}

/// Record an attempt outcome. `resolved_by_agent_id` and `resolved_by_session_id` use
/// `COALESCE`: passing `None` preserves the previously stored value. Once set, this
/// function cannot clear them — a follow-up path would need an explicit clear API.
#[allow(clippy::too_many_arguments)]
pub async fn log_outcome(
    pool: &PgPool,
    id: Uuid,
    outcome: AttemptOutcome,
    reasoning: &str,
    reasoning_embedding: Option<&[f32]>,
    git_ref: Option<&str>,
    code_snippet: Option<&str>,
    resolved_by_agent_id: Option<&str>,
    resolved_by_session_id: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let emb = reasoning_embedding.map(|e| Vector::from(e.to_vec()));
    let result = sqlx::query(
        "UPDATE ai_memory.attempts \
         SET outcome = $2, reasoning = $3, reasoning_embedding = $4, git_ref = $5, \
         code_snippet = COALESCE($6, code_snippet), \
         resolved_by_agent_id = COALESCE($7, resolved_by_agent_id), \
         resolved_by_session_id = COALESCE($8, resolved_by_session_id), \
         resolved_at = NOW() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(&outcome)
    .bind(reasoning)
    .bind(emb.as_ref())
    .bind(git_ref)
    .bind(code_snippet)
    .bind(resolved_by_agent_id)
    .bind(resolved_by_session_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Auto-accept the latest pending attempt and reject all others when a task completes.
pub async fn resolve_attempts_on_complete(
    pool: &PgPool,
    task_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    // Find latest pending attempt
    let latest: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM ai_memory.attempts \
         WHERE task_id = $1 AND outcome = 'pending' \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(task_id)
    .fetch_optional(pool)
    .await?;

    let Some((accepted_id,)) = latest else {
        return Ok(None);
    };

    // Accept the latest (preserve existing reasoning if non-empty)
    sqlx::query(
        "UPDATE ai_memory.attempts SET outcome = 'accepted', \
         reasoning = CASE WHEN reasoning = '' THEN 'Auto-accepted on task completion' ELSE reasoning END, \
         resolved_at = NOW() \
         WHERE id = $1",
    )
    .bind(accepted_id)
    .execute(pool)
    .await?;

    // Reject all other pending attempts for this task
    sqlx::query(
        "UPDATE ai_memory.attempts SET outcome = 'rejected', \
         reasoning = CASE WHEN reasoning = '' THEN 'Auto-rejected: task completed with different attempt' ELSE reasoning END, \
         resolved_at = NOW() \
         WHERE task_id = $1 AND outcome = 'pending' AND id != $2",
    )
    .bind(task_id)
    .bind(accepted_id)
    .execute(pool)
    .await?;

    Ok(Some(accepted_id))
}

pub async fn search_similar_failures(
    pool: &PgPool,
    project_id: Option<Uuid>,
    embedding: &[f32],
    limit: i64,
) -> Result<Vec<Attempt>, sqlx::Error> {
    let emb = Vector::from(embedding.to_vec());
    let project_filter = if project_id.is_some() {
        "AND t.project_id = $3"
    } else {
        ""
    };
    let sql = format!(
        "SELECT a.id, a.task_id, a.approach_summary, a.code_snippet, a.outcome, a.reasoning, \
         a.reasoning_embedding, a.git_ref, a.token_cost, a.agent_id, a.session_id, \
         a.resolved_by_agent_id, a.resolved_by_session_id, a.created_at, a.resolved_at \
         FROM ai_memory.attempts a \
         JOIN ai_memory.tasks t ON a.task_id = t.id \
         WHERE a.outcome = 'rejected' AND a.reasoning_embedding IS NOT NULL {project_filter} \
         ORDER BY a.reasoning_embedding <=> $1::vector LIMIT $2"
    );
    sqlx::query_as(&sql)
        .bind(&emb)
        .bind(limit)
        .bind(project_id)
        .fetch_all(pool)
        .await
}
