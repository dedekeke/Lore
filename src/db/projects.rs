use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub root_path: String,
    pub created_at: DateTime<Utc>,
}

pub async fn create_project(pool: &PgPool, name: &str, root_path: &str) -> Result<Uuid, sqlx::Error> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO ai_memory.projects (name, root_path) VALUES ($1, $2) RETURNING id",
    )
    .bind(name)
    .bind(root_path)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

pub async fn get_project(pool: &PgPool, id: Uuid) -> Result<Option<Project>, sqlx::Error> {
    sqlx::query_as("SELECT id, name, root_path, created_at FROM ai_memory.projects WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}

pub async fn get_project_by_name(pool: &PgPool, name: &str) -> Result<Option<Project>, sqlx::Error> {
    sqlx::query_as("SELECT id, name, root_path, created_at FROM ai_memory.projects WHERE name = $1")
        .bind(name)
        .fetch_optional(pool)
        .await
}

pub async fn list_projects(pool: &PgPool) -> Result<Vec<Project>, sqlx::Error> {
    sqlx::query_as("SELECT id, name, root_path, created_at FROM ai_memory.projects ORDER BY created_at")
        .fetch_all(pool)
        .await
}

/// Upsert: insert or return existing project by name (race-safe).
pub async fn get_or_create_project(pool: &PgPool, name: &str, root_path: &str) -> Result<Uuid, sqlx::Error> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO ai_memory.projects (name, root_path) VALUES ($1, $2) \
         ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name \
         RETURNING id",
    )
    .bind(name)
    .bind(root_path)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

pub async fn delete_project(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM ai_memory.projects WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
