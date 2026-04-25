use chrono::{DateTime, Utc};
use pgvector::Vector;
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
    pub ticket_number: Option<String>,
    #[serde(skip)]
    #[allow(dead_code)]
    pub description_embedding: Option<Vector>,
}

#[allow(clippy::too_many_arguments)]
pub async fn create_task(
    pool: &PgPool,
    project_id: Uuid,
    description: &str,
    parent_task_id: Option<Uuid>,
    priority: Option<&str>,
    task_type: Option<&str>,
    ticket_number: Option<&str>,
    description_embedding: Option<&[f32]>,
) -> Result<Uuid, sqlx::Error> {
    let summary = generate_summary(description);
    let emb = description_embedding.map(|e| Vector::from(e.to_vec()));
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO ai_memory.tasks (project_id, description, parent_task_id, summary, priority, task_type, ticket_number, description_embedding) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(project_id)
    .bind(description)
    .bind(parent_task_id)
    .bind(&summary)
    .bind(priority)
    .bind(task_type)
    .bind(ticket_number)
    .bind(emb.as_ref())
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

pub async fn get_task(pool: &PgPool, id: Uuid) -> Result<Option<Task>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, project_id, description, status, parent_task_id, resolved_attempt_id, created_at, completed_at, priority, task_type, summary, ticket_number, description_embedding \
         FROM ai_memory.tasks WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Partial update set for `apply_task_update`.
///
/// Each field is `Option<Option<_>>` with three states:
/// - `None` — don't touch (leave existing value).
/// - `Some(None)` — clear (write SQL NULL).
/// - `Some(Some(v))` — set to `v`.
///
/// Constructed as a struct literal with `..Default::default()`, so call
/// sites only mention the fields they actually mutate.
#[derive(Debug, Default)]
pub struct TaskUpdate<'a> {
    pub priority: Option<Option<&'a str>>,
    pub task_type: Option<Option<&'a str>>,
    pub description: Option<&'a str>,
    pub description_embedding: Option<&'a [f32]>,
    pub parent_task_id: Option<Option<Uuid>>,
    pub ticket_number: Option<Option<&'a str>>,
}

impl TaskUpdate<'_> {
    fn is_empty(&self) -> bool {
        self.priority.is_none()
            && self.task_type.is_none()
            && self.description.is_none()
            && self.description_embedding.is_none()
            && self.parent_task_id.is_none()
            && self.ticket_number.is_none()
    }
}

pub async fn apply_task_update(
    pool: &PgPool,
    id: Uuid,
    fields: TaskUpdate<'_>,
) -> Result<bool, sqlx::Error> {
    if fields.is_empty() {
        return Ok(false);
    }

    let mut set_clauses = Vec::new();
    let mut param_idx = 2u32;

    if fields.priority.is_some() {
        set_clauses.push(format!("priority = ${param_idx}"));
        param_idx += 1;
    }
    if fields.task_type.is_some() {
        set_clauses.push(format!("task_type = ${param_idx}"));
        param_idx += 1;
    }
    if fields.description.is_some() {
        set_clauses.push(format!(
            "description = ${param_idx}, summary = ${}",
            param_idx + 1
        ));
        param_idx += 2;
    }
    if fields.description_embedding.is_some() {
        set_clauses.push(format!("description_embedding = ${param_idx}"));
        param_idx += 1;
    }
    if fields.parent_task_id.is_some() {
        set_clauses.push(format!("parent_task_id = ${param_idx}"));
        param_idx += 1;
    }
    if fields.ticket_number.is_some() {
        set_clauses.push(format!("ticket_number = ${param_idx}"));
        param_idx += 1;
    }
    // Suppress unused-assignment warning; the trailing increment exists so
    // adding a new field after `ticket_number` doesn't silently reuse the
    // previous index and corrupt the bind order.
    let _ = param_idx;

    let sql = format!(
        "UPDATE ai_memory.tasks SET {} WHERE id = $1",
        set_clauses.join(", ")
    );

    let emb = fields
        .description_embedding
        .map(|e| Vector::from(e.to_vec()));
    let mut query = sqlx::query(&sql).bind(id);
    if let Some(p) = fields.priority {
        query = query.bind(p);
    }
    if let Some(tt) = fields.task_type {
        query = query.bind(tt);
    }
    if let Some(desc) = fields.description {
        let summary = generate_summary(desc);
        query = query.bind(desc);
        query = query.bind(summary);
    }
    if emb.is_some() {
        query = query.bind(emb.as_ref());
    }
    if let Some(pid) = fields.parent_task_id {
        query = query.bind(pid);
    }
    if let Some(tn) = fields.ticket_number {
        query = query.bind(tn);
    }

    let result = query.execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

pub async fn delete_task(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM ai_memory.tasks WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Look up tasks by their external tracker reference within a project.
/// Returns Vec because uniqueness is not enforced — a single ticket can
/// legitimately map to multiple Lore tasks (e.g. a refactor task + a
/// follow-up bug). Hits the partial index `idx_tasks_ticket_number`.
pub async fn find_by_ticket_number(
    pool: &PgPool,
    project_id: Uuid,
    ticket_number: &str,
) -> Result<Vec<Task>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, project_id, description, status, parent_task_id, resolved_attempt_id, \
         created_at, completed_at, priority, task_type, summary, ticket_number, description_embedding \
         FROM ai_memory.tasks \
         WHERE project_id = $1 AND ticket_number = $2 \
         ORDER BY created_at DESC",
    )
    .bind(project_id)
    .bind(ticket_number)
    .fetch_all(pool)
    .await
}

pub async fn list_tasks(
    pool: &PgPool,
    project_id: Uuid,
    status: Option<TaskStatus>,
) -> Result<Vec<Task>, sqlx::Error> {
    match status {
        Some(s) => sqlx::query_as(
            "SELECT id, project_id, description, status, parent_task_id, resolved_attempt_id, created_at, completed_at, priority, task_type, summary, ticket_number, description_embedding \
                 FROM ai_memory.tasks WHERE project_id = $1 AND status = $2 ORDER BY created_at",
        )
        .bind(project_id)
        .bind(&s)
        .fetch_all(pool)
        .await,
        None => sqlx::query_as(
            "SELECT id, project_id, description, status, parent_task_id, resolved_attempt_id, created_at, completed_at, priority, task_type, summary, ticket_number, description_embedding \
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
    pub priority: Option<String>,
    pub ticket_number: Option<String>,
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
        "SELECT t.id, t.description, t.status, t.created_at, t.priority, t.ticket_number, \
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

pub fn generate_summary(description: &str) -> String {
    if description.chars().count() <= 120 {
        description.to_string()
    } else {
        let mut s: String = description.chars().take(117).collect();
        s.push_str("...");
        s
    }
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

pub async fn list_tasks_paginated(
    pool: &PgPool,
    project_id: Uuid,
    status: Option<TaskStatus>,
    sort_col: &str,
    sort_dir: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<Task>, sqlx::Error> {
    let col = match sort_col {
        "summary" | "priority" | "task_type" | "status" | "created_at" | "ticket_number" => {
            sort_col
        }
        _ => "created_at",
    };
    let dir = if sort_dir.eq_ignore_ascii_case("asc") {
        "ASC"
    } else {
        "DESC"
    };

    // When sorting by created_at, priority is primary and created_at becomes secondary
    let order = if col == "created_at" {
        format!("priority ASC NULLS LAST, {col} {dir} NULLS LAST")
    } else {
        format!("{col} {dir} NULLS LAST")
    };

    let sql = match status {
        Some(_) => format!(
            "SELECT id, project_id, description, status, parent_task_id, resolved_attempt_id, \
             created_at, completed_at, priority, task_type, summary, ticket_number, description_embedding \
             FROM ai_memory.tasks WHERE project_id = $1 AND status = $2 \
             ORDER BY {order} LIMIT $3 OFFSET $4"
        ),
        None => format!(
            "SELECT id, project_id, description, status, parent_task_id, resolved_attempt_id, \
             created_at, completed_at, priority, task_type, summary, ticket_number, description_embedding \
             FROM ai_memory.tasks WHERE project_id = $1 \
             ORDER BY {order} LIMIT $2 OFFSET $3"
        ),
    };

    match status {
        Some(s) => {
            sqlx::query_as(&sql)
                .bind(project_id)
                .bind(&s)
                .bind(limit)
                .bind(offset)
                .fetch_all(pool)
                .await
        }
        None => {
            sqlx::query_as(&sql)
                .bind(project_id)
                .bind(limit)
                .bind(offset)
                .fetch_all(pool)
                .await
        }
    }
}

/// Direct children only (depth=1), not recursive.
pub async fn list_subtasks(pool: &PgPool, parent_task_id: Uuid) -> Result<Vec<Task>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, project_id, description, status, parent_task_id, resolved_attempt_id, \
         created_at, completed_at, priority, task_type, summary, ticket_number, description_embedding \
         FROM ai_memory.tasks WHERE parent_task_id = $1 ORDER BY created_at",
    )
    .bind(parent_task_id)
    .fetch_all(pool)
    .await
}

/// Returns true if parent has subtasks AND all are completed/abandoned
pub async fn all_subtasks_done(pool: &PgPool, parent_task_id: Uuid) -> Result<bool, sqlx::Error> {
    let row: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), \
         COUNT(*) FILTER (WHERE status IN ('completed', 'abandoned')) \
         FROM ai_memory.tasks WHERE parent_task_id = $1",
    )
    .bind(parent_task_id)
    .fetch_one(pool)
    .await?;
    Ok(row.0 > 0 && row.0 == row.1)
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
         completed_at = CASE WHEN $2 IN ('completed', 'abandoned') THEN NOW() ELSE NULL END \
         WHERE id = ANY($1)",
    )
    .bind(ids)
    .bind(&status)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn complete_task(
    pool: &PgPool,
    id: Uuid,
    resolved_attempt_id: Option<Uuid>,
) -> Result<bool, sqlx::Error> {
    // Guard: only transition non-completed tasks (prevents TOCTOU race on concurrent rollup)
    let result = sqlx::query(
        "UPDATE ai_memory.tasks SET status = 'completed', completed_at = NOW(), \
         resolved_attempt_id = COALESCE($2, resolved_attempt_id) \
         WHERE id = $1 AND status != 'completed'",
    )
    .bind(id)
    .bind(resolved_attempt_id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Ok(false);
    }

    // Auto-resolve attempts only after successful status transition
    let auto_accepted = super::attempts::resolve_attempts_on_complete(pool, id).await?;
    if resolved_attempt_id.is_none() {
        if let Some(attempt_id) = auto_accepted {
            sqlx::query(
                "UPDATE ai_memory.tasks SET resolved_attempt_id = $2 \
                 WHERE id = $1 AND resolved_attempt_id IS NULL",
            )
            .bind(id)
            .bind(attempt_id)
            .execute(pool)
            .await?;
        }
    }

    Ok(true)
}

/// Walk up the parent chain, auto-completing each ancestor whose subtasks are all done.
/// Returns the number of parents rolled up. Caps at 10 levels.
pub async fn try_rollup_parents(pool: &PgPool, task_id: Uuid) -> u32 {
    let mut current_id = task_id;
    let mut rolled = 0u32;
    for _ in 0..10 {
        let task = match get_task(pool, current_id).await {
            Ok(Some(t)) => t,
            _ => break,
        };
        let parent_id = match task.parent_task_id {
            Some(pid) => pid,
            None => break,
        };
        match all_subtasks_done(pool, parent_id).await {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                tracing::warn!(error = %e, parent_id = %parent_id, "rollup check failed");
                break;
            }
        }
        if let Err(e) = complete_task(pool, parent_id, None).await {
            tracing::warn!(error = %e, parent_id = %parent_id, "rollup complete failed");
            break;
        }
        rolled += 1;
        current_id = parent_id;
    }
    rolled
}
