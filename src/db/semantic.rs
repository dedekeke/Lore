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
    #[allow(dead_code)]
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

#[allow(dead_code)]
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

pub async fn count_rules(
    pool: &PgPool,
    project_id: Uuid,
    category: Option<RuleCategory>,
) -> Result<i64, sqlx::Error> {
    let row: (i64,) = match category {
        Some(cat) => {
            sqlx::query_as(
                "SELECT COUNT(*) FROM ai_memory.semantic_rules WHERE project_id = $1 AND category = $2",
            )
            .bind(project_id)
            .bind(&cat)
            .fetch_one(pool)
            .await?
        }
        None => {
            sqlx::query_as("SELECT COUNT(*) FROM ai_memory.semantic_rules WHERE project_id = $1")
                .bind(project_id)
                .fetch_one(pool)
                .await?
        }
    };
    Ok(row.0)
}

pub async fn update_rule(
    pool: &PgPool,
    id: Uuid,
    category: Option<RuleCategory>,
    content: Option<&str>,
    embedding: Option<&[f32]>,
) -> Result<bool, sqlx::Error> {
    let emb = embedding.map(|e| Vector::from(e.to_vec()));
    let result = sqlx::query(
        "UPDATE ai_memory.semantic_rules SET \
         category = COALESCE($2, category), \
         content = COALESCE($3, content), \
         embedding = COALESCE($4, embedding) \
         WHERE id = $1",
    )
    .bind(id)
    .bind(category.as_ref())
    .bind(content)
    .bind(emb.as_ref())
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn delete_rule(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM ai_memory.semantic_rules WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn batch_delete_rules(pool: &PgPool, ids: &[Uuid]) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM ai_memory.semantic_rules WHERE id = ANY($1)")
        .bind(ids)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Hybrid search: combines vector similarity (cosine) with full-text keyword search (BM25)
/// using Reciprocal Rank Fusion (RRF). Falls back to vector-only if no query text is provided.
pub async fn search_rules_hybrid(
    pool: &PgPool,
    project_id: Uuid,
    embedding: &[f32],
    query: &str,
    limit: i64,
    category: Option<RuleCategory>,
) -> Result<Vec<SemanticRule>, sqlx::Error> {
    let ts_query = to_tsquery_safe(query);
    if ts_query.is_empty() {
        return search_rules_by_embedding(pool, project_id, embedding, limit, category).await;
    }
    let limit = limit.max(1);
    let candidate_limit = limit * 3;
    let emb = Vector::from(embedding.to_vec());

    // RRF: 1/(k+rank_vector) + 1/(k+rank_fts), k=60
    let (cat_filter, cat_val) = match &category {
        Some(cat) => ("AND category = $6", Some(cat)),
        None => ("", None),
    };

    let sql = format!(
        "WITH vector_ranked AS (
            SELECT id, ROW_NUMBER() OVER (ORDER BY embedding <=> $2::vector) AS v_rank
            FROM ai_memory.semantic_rules
            WHERE project_id = $1 AND embedding IS NOT NULL
              AND (expires_at IS NULL OR expires_at > NOW()) {cat_filter}
            LIMIT $3
        ),
        fts_ranked AS (
            SELECT id, ROW_NUMBER() OVER (ORDER BY ts_rank_cd(content_tsv, to_tsquery('english', $4)) DESC) AS f_rank
            FROM ai_memory.semantic_rules
            WHERE project_id = $1 AND content_tsv @@ to_tsquery('english', $4)
              AND (expires_at IS NULL OR expires_at > NOW()) {cat_filter}
            LIMIT $3
        ),
        fused AS (
            SELECT COALESCE(v.id, f.id) AS id,
                   COALESCE(1.0 / (60 + v.v_rank), 0) + COALESCE(1.0 / (60 + f.f_rank), 0) AS rrf_score
            FROM vector_ranked v
            FULL OUTER JOIN fts_ranked f ON v.id = f.id
            ORDER BY rrf_score DESC
            LIMIT $5
        )
        SELECT s.id, s.project_id, s.category, s.content, s.embedding,
               s.source_task_id, s.created_at, s.expires_at
        FROM fused
        JOIN ai_memory.semantic_rules s ON s.id = fused.id
        ORDER BY fused.rrf_score DESC"
    );

    if let Some(cat) = cat_val {
        sqlx::query_as(&sql)
            .bind(project_id)
            .bind(&emb)
            .bind(candidate_limit)
            .bind(&ts_query)
            .bind(limit)
            .bind(cat)
            .fetch_all(pool)
            .await
    } else {
        sqlx::query_as(&sql)
            .bind(project_id)
            .bind(&emb)
            .bind(candidate_limit)
            .bind(&ts_query)
            .bind(limit)
            .fetch_all(pool)
            .await
    }
}

/// Vector-only search (used when no text query available)
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

/// Find rules with cosine similarity above threshold (for dedup detection)
pub async fn find_duplicates(
    pool: &PgPool,
    project_id: Uuid,
    embedding: &[f32],
    threshold: f64,
) -> Result<Vec<SemanticRule>, sqlx::Error> {
    let emb = Vector::from(embedding.to_vec());
    // cosine distance <=> returns distance (0 = identical), so similarity = 1 - distance
    sqlx::query_as(
        "SELECT id, project_id, category, content, embedding, source_task_id, created_at, expires_at \
         FROM ai_memory.semantic_rules \
         WHERE project_id = $1 AND embedding IS NOT NULL \
         AND (1.0 - (embedding <=> $2::vector)) >= $3 \
         ORDER BY embedding <=> $2::vector LIMIT 5",
    )
    .bind(project_id)
    .bind(&emb)
    .bind(threshold)
    .fetch_all(pool)
    .await
}

/// Sanitize user input into a safe tsquery string
fn to_tsquery_safe(input: &str) -> String {
    input
        .split_whitespace()
        .filter(|w| !w.is_empty())
        .map(|w| w.replace(['\'', '\\', ':', '&', '|', '!', '(', ')'], ""))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" & ")
}
