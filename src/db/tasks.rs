use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "ai_memory.task_status", rename_all = "snake_case")]
pub enum TaskStatus {
    Active,
    Completed,
    Abandoned,
    Blocked,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub project_id: Uuid,
    pub description: String,
    pub status: TaskStatus,
    pub parent_task_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

pub async fn create_task(
    pool: &PgPool,
    project_id: Uuid,
    description: &str,
    parent_task_id: Option<Uuid>,
) -> Result<Uuid, sqlx::Error> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO ai_memory.tasks (project_id, description, parent_task_id) \
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(project_id)
    .bind(description)
    .bind(parent_task_id)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

#[allow(dead_code)]
pub async fn get_task(pool: &PgPool, id: Uuid) -> Result<Option<Task>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, project_id, description, status, parent_task_id, created_at, completed_at \
         FROM ai_memory.tasks WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn list_tasks(
    pool: &PgPool,
    project_id: Uuid,
    status: Option<TaskStatus>,
) -> Result<Vec<Task>, sqlx::Error> {
    match status {
        Some(s) => sqlx::query_as(
            "SELECT id, project_id, description, status, parent_task_id, created_at, completed_at \
                 FROM ai_memory.tasks WHERE project_id = $1 AND status = $2 ORDER BY created_at",
        )
        .bind(project_id)
        .bind(&s)
        .fetch_all(pool)
        .await,
        None => sqlx::query_as(
            "SELECT id, project_id, description, status, parent_task_id, created_at, completed_at \
                 FROM ai_memory.tasks WHERE project_id = $1 ORDER BY created_at",
        )
        .bind(project_id)
        .fetch_all(pool)
        .await,
    }
}

#[allow(dead_code)]
pub async fn update_task_status(
    pool: &PgPool,
    id: Uuid,
    status: TaskStatus,
) -> Result<bool, sqlx::Error> {
    // Auto-set completed_at when transitioning to Completed, clear it otherwise
    let result = sqlx::query(
        "UPDATE ai_memory.tasks SET status = $2, \
         completed_at = CASE WHEN $2 = 'completed' THEN NOW() ELSE NULL END \
         WHERE id = $1",
    )
    .bind(id)
    .bind(&status)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn abandon_task(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE ai_memory.tasks SET status = 'abandoned', completed_at = NOW() WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn complete_task(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE ai_memory.tasks SET status = 'completed', completed_at = NOW() WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}
