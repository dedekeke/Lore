use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub root_path: String,
    pub created_at: DateTime<Utc>,
    pub ticket_url_template: Option<String>,
}

#[allow(dead_code)]
pub async fn create_project(
    pool: &PgPool,
    name: &str,
    root_path: &str,
) -> Result<Uuid, sqlx::Error> {
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
    sqlx::query_as("SELECT id, name, root_path, created_at, ticket_url_template FROM ai_memory.projects WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}

#[allow(dead_code)]
pub async fn get_project_by_name(
    pool: &PgPool,
    name: &str,
) -> Result<Option<Project>, sqlx::Error> {
    sqlx::query_as("SELECT id, name, root_path, created_at, ticket_url_template FROM ai_memory.projects WHERE name = $1")
        .bind(name)
        .fetch_optional(pool)
        .await
}

#[allow(dead_code)]
pub async fn list_projects(pool: &PgPool) -> Result<Vec<Project>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, name, root_path, created_at, ticket_url_template FROM ai_memory.projects ORDER BY created_at",
    )
    .fetch_all(pool)
    .await
}

/// Upsert: insert or return existing project by name (race-safe).
pub async fn get_or_create_project(
    pool: &PgPool,
    name: &str,
    root_path: &str,
) -> Result<Uuid, sqlx::Error> {
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

/// Upsert by root_path: find existing project or create one using the directory name.
pub async fn get_or_create_project_by_path(
    pool: &PgPool,
    root_path: &str,
) -> Result<(Uuid, String), sqlx::Error> {
    if let Some(project) = get_project_by_root_path(pool, root_path).await? {
        return Ok((project.id, project.name));
    }
    let name = std::path::Path::new(root_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string());
    let id = get_or_create_project(pool, &name, root_path).await?;
    Ok((id, name))
}

pub async fn get_project_by_root_path(
    pool: &PgPool,
    root_path: &str,
) -> Result<Option<Project>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, name, root_path, created_at, ticket_url_template FROM ai_memory.projects WHERE root_path = $1",
    )
    .bind(root_path)
    .fetch_optional(pool)
    .await
}

/// Set or clear the per-project ticket-URL template (e.g.
/// `https://jira.example.com/browse/{ticket}`). Pass `None` to clear.
/// The CHECK constraint enforces the `{ticket}` placeholder when set.
pub async fn update_ticket_url_template(
    pool: &PgPool,
    id: Uuid,
    template: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let result =
        sqlx::query("UPDATE ai_memory.projects SET ticket_url_template = $2 WHERE id = $1")
            .bind(id)
            .bind(template)
            .execute(pool)
            .await?;
    Ok(result.rows_affected() > 0)
}

#[allow(dead_code)]
pub async fn delete_project(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM ai_memory.projects WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
