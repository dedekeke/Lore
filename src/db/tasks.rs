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
    pub resolved_attempt_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub priority: Option<String>,
    pub task_type: Option<String>,
    pub summary: Option<String>,
}

pub async fn create_task(
    pool: &PgPool,
    project_id: Uuid,
    description: &str,
    parent_task_id: Option<Uuid>,
) -> Result<Uuid, sqlx::Error> {
    // Auto-generate summary from first sentence or first 120 chars
    let summary = generate_summary(description);
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO ai_memory.tasks (project_id, description, parent_task_id, summary) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(project_id)
    .bind(description)
    .bind(parent_task_id)
    .bind(&summary)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

fn generate_summary(description: &str) -> String {
    let first_line = description.lines().next().unwrap_or(description);
    if first_line.len() <= 120 {
        first_line.to_string()
    } else {
        let mut s: String = first_line.chars().take(117).collect();
        s.push_str("...");
        s
    }
}

#[allow(dead_code)]
pub async fn get_task(pool: &PgPool, id: Uuid) -> Result<Option<Task>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, project_id, description, status, parent_task_id, resolved_attempt_id, created_at, completed_at, priority, task_type, summary \
         FROM ai_memory.tasks WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn delete_task(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM ai_memory.tasks WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn list_tasks(
    pool: &PgPool,
    project_id: Uuid,
    status: Option<TaskStatus>,
) -> Result<Vec<Task>, sqlx::Error> {
    match status {
        Some(s) => sqlx::query_as(
            "SELECT id, project_id, description, status, parent_task_id, resolved_attempt_id, created_at, completed_at, priority, task_type, summary \
                 FROM ai_memory.tasks WHERE project_id = $1 AND status = $2 ORDER BY created_at",
        )
        .bind(project_id)
        .bind(&s)
        .fetch_all(pool)
        .await,
        None => sqlx::query_as(
            "SELECT id, project_id, description, status, parent_task_id, resolved_attempt_id, created_at, completed_at, priority, task_type, summary \
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

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct TaskSummary {
    pub id: Uuid,
    pub description: String,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub total_attempts: i64,
    pub pending_attempts: i64,
    pub rejected_attempts: i64,
    pub accepted_attempts: i64,
}

pub async fn get_task_summaries(
    pool: &PgPool,
    project_id: Uuid,
    statuses: &[TaskStatus],
) -> Result<Vec<TaskSummary>, sqlx::Error> {
    let status_strs: Vec<String> = statuses
        .iter()
        .map(|s| match s {
            TaskStatus::Active => "active".into(),
            TaskStatus::Completed => "completed".into(),
            TaskStatus::Abandoned => "abandoned".into(),
            TaskStatus::Blocked => "blocked".into(),
        })
        .collect();

    sqlx::query_as(
        "SELECT t.id, t.description, t.status, t.created_at, \
         COUNT(a.id) AS total_attempts, \
         COUNT(a.id) FILTER (WHERE a.outcome = 'pending') AS pending_attempts, \
         COUNT(a.id) FILTER (WHERE a.outcome = 'rejected') AS rejected_attempts, \
         COUNT(a.id) FILTER (WHERE a.outcome = 'accepted') AS accepted_attempts \
         FROM ai_memory.tasks t \
         LEFT JOIN ai_memory.attempts a ON a.task_id = t.id \
         WHERE t.project_id = $1 AND t.status::text = ANY($2) \
         GROUP BY t.id ORDER BY t.created_at DESC",
    )
    .bind(project_id)
    .bind(&status_strs)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct TaskStats {
    pub id: Uuid,
    pub description: String,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub total_attempts: i64,
    pub pending_attempts: i64,
    pub rejected_attempts: i64,
    pub accepted_attempts: i64,
    pub unknown_attempts: i64,
    /// Minutes from task creation to completion (NULL if not completed)
    pub resolution_minutes: Option<f64>,
}

pub async fn get_task_stats(
    pool: &PgPool,
    project_id: Uuid,
    status: Option<TaskStatus>,
) -> Result<Vec<TaskStats>, sqlx::Error> {
    match status {
        Some(s) => {
            sqlx::query_as(
                "SELECT t.id, t.description, t.status, t.created_at, t.completed_at, \
                 COUNT(a.id) AS total_attempts, \
                 COUNT(a.id) FILTER (WHERE a.outcome = 'pending') AS pending_attempts, \
                 COUNT(a.id) FILTER (WHERE a.outcome = 'rejected') AS rejected_attempts, \
                 COUNT(a.id) FILTER (WHERE a.outcome = 'accepted') AS accepted_attempts, \
                 COUNT(a.id) FILTER (WHERE a.outcome = 'unknown') AS unknown_attempts, \
                 (EXTRACT(EPOCH FROM (t.completed_at - t.created_at)) / 60.0)::FLOAT8 AS resolution_minutes \
                 FROM ai_memory.tasks t \
                 LEFT JOIN ai_memory.attempts a ON a.task_id = t.id \
                 WHERE t.project_id = $1 AND t.status = $2 \
                 GROUP BY t.id ORDER BY t.created_at DESC",
            )
            .bind(project_id)
            .bind(&s)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as(
                "SELECT t.id, t.description, t.status, t.created_at, t.completed_at, \
                 COUNT(a.id) AS total_attempts, \
                 COUNT(a.id) FILTER (WHERE a.outcome = 'pending') AS pending_attempts, \
                 COUNT(a.id) FILTER (WHERE a.outcome = 'rejected') AS rejected_attempts, \
                 COUNT(a.id) FILTER (WHERE a.outcome = 'accepted') AS accepted_attempts, \
                 COUNT(a.id) FILTER (WHERE a.outcome = 'unknown') AS unknown_attempts, \
                 (EXTRACT(EPOCH FROM (t.completed_at - t.created_at)) / 60.0)::FLOAT8 AS resolution_minutes \
                 FROM ai_memory.tasks t \
                 LEFT JOIN ai_memory.attempts a ON a.task_id = t.id \
                 WHERE t.project_id = $1 \
                 GROUP BY t.id ORDER BY t.created_at DESC",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
    }
}

pub async fn complete_task(
    pool: &PgPool,
    id: Uuid,
    resolved_attempt_id: Option<Uuid>,
) -> Result<bool, sqlx::Error> {
    // Auto-resolve attempts: accept latest pending, reject the rest
    let auto_resolved = super::attempts::resolve_attempts_on_complete(pool, id).await?;
    let final_resolved = resolved_attempt_id.or(auto_resolved);

    let result = sqlx::query(
        "UPDATE ai_memory.tasks SET status = 'completed', completed_at = NOW(), \
         resolved_attempt_id = COALESCE($2, resolved_attempt_id) WHERE id = $1",
    )
    .bind(id)
    .bind(final_resolved)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn count_tasks(
    pool: &PgPool,
    project_id: Uuid,
    status: Option<TaskStatus>,
) -> Result<i64, sqlx::Error> {
    let row: (i64,) = match status {
        Some(s) => {
            sqlx::query_as(
                "SELECT COUNT(*) FROM ai_memory.tasks WHERE project_id = $1 AND status = $2",
            )
            .bind(project_id)
            .bind(&s)
            .fetch_one(pool)
            .await?
        }
        None => {
            sqlx::query_as("SELECT COUNT(*) FROM ai_memory.tasks WHERE project_id = $1")
                .bind(project_id)
                .fetch_one(pool)
                .await?
        }
    };
    Ok(row.0)
}

/// Paginated + sorted task list for dashboard
pub async fn list_tasks_paginated(
    pool: &PgPool,
    project_id: Uuid,
    status: Option<TaskStatus>,
    sort_col: &str,
    sort_dir: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<Task>, sqlx::Error> {
    // Whitelist sort columns to prevent SQL injection
    let col = match sort_col {
        "status" => "status",
        "priority" => "priority",
        "task_type" => "task_type",
        "summary" => "summary",
        _ => "created_at",
    };
    let dir = if sort_dir == "asc" { "ASC" } else { "DESC" };
    // NULLS LAST for ascending, NULLS FIRST for descending (default pg behavior is fine)
    let nulls = if dir == "ASC" {
        "NULLS LAST"
    } else {
        "NULLS FIRST"
    };

    let q = match status {
        Some(ref s) => {
            let sql = format!(
                "SELECT id, project_id, description, status, parent_task_id, resolved_attempt_id, \
                 created_at, completed_at, priority, task_type, summary \
                 FROM ai_memory.tasks WHERE project_id = $1 AND status = $2 \
                 ORDER BY {col} {dir} {nulls} LIMIT $3 OFFSET $4"
            );
            sqlx::query_as(&sql)
                .bind(project_id)
                .bind(s)
                .bind(limit)
                .bind(offset)
                .fetch_all(pool)
                .await
        }
        None => {
            let sql = format!(
                "SELECT id, project_id, description, status, parent_task_id, resolved_attempt_id, \
                 created_at, completed_at, priority, task_type, summary \
                 FROM ai_memory.tasks WHERE project_id = $1 \
                 ORDER BY {col} {dir} {nulls} LIMIT $2 OFFSET $3"
            );
            sqlx::query_as(&sql)
                .bind(project_id)
                .bind(limit)
                .bind(offset)
                .fetch_all(pool)
                .await
        }
    };
    q
}

pub async fn batch_delete_tasks(pool: &PgPool, ids: &[Uuid]) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM ai_memory.tasks WHERE id = ANY($1)")
        .bind(ids)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn batch_update_task_status(
    pool: &PgPool,
    ids: &[Uuid],
    status: TaskStatus,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE ai_memory.tasks SET status = $2, \
         completed_at = CASE WHEN $2 = 'completed' THEN NOW() \
                             WHEN $2 = 'abandoned' THEN NOW() \
                             ELSE NULL END \
         WHERE id = ANY($1)",
    )
    .bind(ids)
    .bind(&status)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}
