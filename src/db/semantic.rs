use chrono::{DateTime, Utc};
use pgvector::Vector;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "ai_memory.rule_category", rename_all = "snake_case")]
pub enum RuleCategory {
    Preference,
    Fact,
    Constraint,
    Lesson,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct SemanticRule {
    pub id: Uuid,
    pub project_id: Uuid,
    pub category: RuleCategory,
    pub content: String,
    #[serde(skip)]
    pub embedding: Option<Vector>,
    pub source_task_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

pub async fn create_rule(
    pool: &PgPool,
    project_id: Uuid,
    category: RuleCategory,
    content: &str,
    embedding: Option<&[f32]>,
) -> Result<Uuid, sqlx::Error> {
    let emb = embedding.map(|e| Vector::from(e.to_vec()));
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO ai_memory.semantic_rules (project_id, category, content, embedding) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(project_id)
    .bind(&category)
    .bind(content)
    .bind(emb.as_ref())
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

pub async fn get_rule(pool: &PgPool, id: Uuid) -> Result<Option<SemanticRule>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, project_id, category, content, embedding, source_task_id, created_at, expires_at \
         FROM ai_memory.semantic_rules WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn list_rules(
    pool: &PgPool,
    project_id: Uuid,
    category: Option<RuleCategory>,
) -> Result<Vec<SemanticRule>, sqlx::Error> {
    match category {
        Some(cat) => {
            sqlx::query_as(
                "SELECT id, project_id, category, content, embedding, source_task_id, created_at, expires_at \
                 FROM ai_memory.semantic_rules WHERE project_id = $1 AND category = $2 ORDER BY created_at",
            )
            .bind(project_id)
            .bind(&cat)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as(
                "SELECT id, project_id, category, content, embedding, source_task_id, created_at, expires_at \
                 FROM ai_memory.semantic_rules WHERE project_id = $1 ORDER BY created_at",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
    }
}

pub async fn delete_rule(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM ai_memory.semantic_rules WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn search_rules_by_embedding(
    pool: &PgPool,
    project_id: Uuid,
    embedding: &[f32],
    limit: i64,
    category: Option<RuleCategory>,
) -> Result<Vec<SemanticRule>, sqlx::Error> {
    let emb = Vector::from(embedding.to_vec());
    match category {
        Some(cat) => {
            sqlx::query_as(
                "SELECT id, project_id, category, content, embedding, source_task_id, created_at, expires_at \
                 FROM ai_memory.semantic_rules \
                 WHERE project_id = $1 AND embedding IS NOT NULL AND category = $4 \
                 AND (expires_at IS NULL OR expires_at > NOW()) \
                 ORDER BY embedding <=> $2::vector LIMIT $3",
            )
            .bind(project_id)
            .bind(&emb)
            .bind(limit)
            .bind(&cat)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as(
                "SELECT id, project_id, category, content, embedding, source_task_id, created_at, expires_at \
                 FROM ai_memory.semantic_rules \
                 WHERE project_id = $1 AND embedding IS NOT NULL \
                 AND (expires_at IS NULL OR expires_at > NOW()) \
                 ORDER BY embedding <=> $2::vector LIMIT $3",
            )
            .bind(project_id)
            .bind(&emb)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
    }
}
