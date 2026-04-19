use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// Short-term scratchpad entry — ephemeral per (project, task?, key) storage.
/// Complements long-term semantic rules. Rows past `expires_at` are filtered
/// out of reads; physical deletion is handled by a background retention sweep.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct ScratchEntry {
    pub id: Uuid,
    pub project_id: Uuid,
    pub task_id: Option<Uuid>,
    pub key: String,
    pub value: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const COLS: &str = "id, project_id, task_id, key, value, expires_at, created_at, updated_at";

/// Upsert by (project_id, task_id, key). `ttl_secs = None` stores no expiry;
/// `ttl_secs = Some(n)` sets `expires_at = NOW() + n seconds`.
pub async fn write_scratch(
    pool: &PgPool,
    project_id: Uuid,
    task_id: Option<Uuid>,
    key: &str,
    value: &str,
    ttl_secs: Option<i64>,
) -> Result<ScratchEntry, sqlx::Error> {
    let expires_at = ttl_secs.map(|s| Utc::now() + chrono::Duration::seconds(s));
    // Unique index is on (project_id, COALESCE(task_id, '0000...'), key) so the
    // ON CONFLICT target must match that expression list exactly.
    let sql = format!(
        "INSERT INTO ai_memory.scratchpad (project_id, task_id, key, value, expires_at) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (project_id, COALESCE(task_id, '00000000-0000-0000-0000-000000000000'::uuid), key) \
         DO UPDATE SET value = EXCLUDED.value, \
                       expires_at = EXCLUDED.expires_at, \
                       updated_at = NOW() \
         RETURNING {COLS}"
    );
    sqlx::query_as(&sql)
        .bind(project_id)
        .bind(task_id)
        .bind(key)
        .bind(value)
        .bind(expires_at)
        .fetch_one(pool)
        .await
}

/// Read a single entry by scope+key. Expired rows are filtered out.
pub async fn read_scratch(
    pool: &PgPool,
    project_id: Uuid,
    task_id: Option<Uuid>,
    key: &str,
) -> Result<Option<ScratchEntry>, sqlx::Error> {
    let sql = format!(
        "SELECT {COLS} FROM ai_memory.scratchpad \
         WHERE project_id = $1 \
           AND task_id IS NOT DISTINCT FROM $2 \
           AND key = $3 \
           AND (expires_at IS NULL OR expires_at > NOW())"
    );
    sqlx::query_as(&sql)
        .bind(project_id)
        .bind(task_id)
        .bind(key)
        .fetch_optional(pool)
        .await
}

/// List non-expired entries for a scope, newest-first.
pub async fn list_scratch(
    pool: &PgPool,
    project_id: Uuid,
    task_id: Option<Uuid>,
    limit: i64,
) -> Result<Vec<ScratchEntry>, sqlx::Error> {
    let sql = format!(
        "SELECT {COLS} FROM ai_memory.scratchpad \
         WHERE project_id = $1 \
           AND task_id IS NOT DISTINCT FROM $2 \
           AND (expires_at IS NULL OR expires_at > NOW()) \
         ORDER BY updated_at DESC \
         LIMIT $3"
    );
    sqlx::query_as(&sql)
        .bind(project_id)
        .bind(task_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

pub async fn delete_scratch(
    pool: &PgPool,
    project_id: Uuid,
    task_id: Option<Uuid>,
    key: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM ai_memory.scratchpad \
         WHERE project_id = $1 \
           AND task_id IS NOT DISTINCT FROM $2 \
           AND key = $3",
    )
    .bind(project_id)
    .bind(task_id)
    .bind(key)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}
