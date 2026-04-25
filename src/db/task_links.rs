use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct TaskLink {
    pub id: Uuid,
    pub source_task_id: Uuid,
    pub target_task_id: Uuid,
    pub link_type: String,
    pub created_at: DateTime<Utc>,
}

pub async fn create_link(
    pool: &PgPool,
    source_task_id: Uuid,
    target_task_id: Uuid,
    link_type: &str,
) -> Result<Uuid, sqlx::Error> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO ai_memory.task_links (source_task_id, target_task_id, link_type) \
         VALUES ($1, $2, $3) \
         RETURNING id",
    )
    .bind(source_task_id)
    .bind(target_task_id)
    .bind(link_type)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// Idempotent variant for implicit writers (e.g. complete_task). Returns
/// `Ok(None)` when the (source, target, link_type) edge already exists, so
/// callers don't conflate "duplicate" with a real DB error. The user-facing
/// `link_tasks` MCP tool keeps using `create_link`, where a duplicate is a
/// real input error worth surfacing.
pub async fn upsert_link(
    pool: &PgPool,
    source_task_id: Uuid,
    target_task_id: Uuid,
    link_type: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO ai_memory.task_links (source_task_id, target_task_id, link_type) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (source_task_id, target_task_id, link_type) DO NOTHING \
         RETURNING id",
    )
    .bind(source_task_id)
    .bind(target_task_id)
    .bind(link_type)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}

pub async fn delete_link(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM ai_memory.task_links WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Get all links where the task is either source or target
pub async fn get_links_for_task(
    pool: &PgPool,
    task_id: Uuid,
) -> Result<Vec<TaskLink>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, source_task_id, target_task_id, link_type, created_at \
         FROM ai_memory.task_links \
         WHERE source_task_id = $1 OR target_task_id = $1 \
         ORDER BY created_at",
    )
    .bind(task_id)
    .fetch_all(pool)
    .await
}
