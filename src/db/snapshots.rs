use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct ContextSnapshot {
    pub id: Uuid,
    pub task_id: Uuid,
    pub wiped_at: DateTime<Utc>,
    pub token_count_before: i32,
    pub last_attempt_id: Option<Uuid>,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
}

pub async fn create_snapshot(
    pool: &PgPool,
    task_id: Uuid,
    token_count_before: i32,
    last_attempt_id: Option<Uuid>,
    agent_id: Option<&str>,
    session_id: Option<&str>,
) -> Result<Uuid, sqlx::Error> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO ai_memory.context_snapshots \
         (task_id, token_count_before, last_attempt_id, agent_id, session_id) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(task_id)
    .bind(token_count_before)
    .bind(last_attempt_id)
    .bind(agent_id)
    .bind(session_id)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

pub async fn list_snapshots(
    pool: &PgPool,
    task_id: Uuid,
) -> Result<Vec<ContextSnapshot>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, task_id, wiped_at, token_count_before, last_attempt_id, agent_id, session_id \
         FROM ai_memory.context_snapshots WHERE task_id = $1 ORDER BY wiped_at DESC",
    )
    .bind(task_id)
    .fetch_all(pool)
    .await
}

pub async fn count_snapshots(pool: &PgPool, task_id: Uuid) -> Result<i64, sqlx::Error> {
    let row: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM ai_memory.context_snapshots WHERE task_id = $1")
            .bind(task_id)
            .fetch_one(pool)
            .await?;
    Ok(row.0)
}
