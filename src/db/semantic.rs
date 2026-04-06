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
    pub hit_count: i32,
    pub last_used_at: Option<DateTime<Utc>>,
    pub weight: Option<f64>,
    pub task_type_affinity: Option<Vec<String>>,
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
        "SELECT id, project_id, category, content, embedding, source_task_id, created_at, expires_at, hit_count, last_used_at, weight, task_type_affinity \
         FROM ai_memory.semantic_rules WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn count_rules(pool: &PgPool, project_id: Uuid) -> Result<i64, sqlx::Error> {
    let row: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM ai_memory.semantic_rules WHERE project_id = $1")
            .bind(project_id)
            .fetch_one(pool)
            .await?;
    Ok(row.0)
}

pub async fn list_rules(
    pool: &PgPool,
    project_id: Uuid,
    category: Option<RuleCategory>,
) -> Result<Vec<SemanticRule>, sqlx::Error> {
    match category {
        Some(cat) => {
            sqlx::query_as(
                "SELECT id, project_id, category, content, embedding, source_task_id, created_at, expires_at, hit_count, last_used_at, weight, task_type_affinity \
                 FROM ai_memory.semantic_rules WHERE project_id = $1 AND category = $2 ORDER BY created_at",
            )
            .bind(project_id)
            .bind(&cat)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as(
                "SELECT id, project_id, category, content, embedding, source_task_id, created_at, expires_at, hit_count, last_used_at, weight, task_type_affinity \
                 FROM ai_memory.semantic_rules WHERE project_id = $1 ORDER BY created_at",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
    }
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

pub async fn batch_delete_rules(pool: &PgPool, ids: &[Uuid]) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM ai_memory.semantic_rules WHERE id = ANY($1)")
        .bind(ids)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn delete_rule(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM ai_memory.semantic_rules WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Weighted hybrid search: RRF (vector + BM25) + category priority + recency decay.
/// Falls back to vector-only if no query text (scoring degraded to cosine-only).
pub async fn search_rules_hybrid(
    pool: &PgPool,
    project_id: Uuid,
    embedding: &[f32],
    query: &str,
    limit: i64,
    category: Option<RuleCategory>,
    task_type: Option<&str>,
) -> Result<Vec<SemanticRule>, sqlx::Error> {
    let ts_query = to_tsquery_safe(query);
    if ts_query.is_empty() {
        let results =
            search_rules_by_embedding(pool, project_id, embedding, limit, category).await?;
        increment_hit_counts(pool, &results);
        return Ok(results);
    }
    let limit = limit.max(1);
    let candidate_limit = limit * 3;
    let emb = Vector::from(embedding.to_vec());

    // Nullable params: $6=category, $7=task_type — pushed into CTEs for pre-filtering
    let cat_filter = "AND ($6::text IS NULL OR category::text = $6)";
    let affinity_filter =
        "AND (task_type_affinity IS NULL OR $7::text IS NULL OR $7 = ANY(task_type_affinity))";

    let sql = format!(
        "WITH vector_ranked AS (
            SELECT id, ROW_NUMBER() OVER (ORDER BY embedding <=> $2::vector) AS v_rank
            FROM ai_memory.semantic_rules
            WHERE project_id = $1 AND embedding IS NOT NULL
              AND (expires_at IS NULL OR expires_at > NOW()) {cat_filter} {affinity_filter}
            LIMIT $3
        ),
        fts_ranked AS (
            SELECT id, ROW_NUMBER() OVER (ORDER BY ts_rank_cd(content_tsv, to_tsquery('english', $4)) DESC) AS f_rank
            FROM ai_memory.semantic_rules
            WHERE project_id = $1 AND content_tsv @@ to_tsquery('english', $4)
              AND (expires_at IS NULL OR expires_at > NOW()) {cat_filter} {affinity_filter}
            LIMIT $3
        ),
        max_hits AS (
            SELECT GREATEST(MAX(hit_count), 1) AS val FROM ai_memory.semantic_rules WHERE project_id = $1
        ),
        fused AS (
            SELECT COALESCE(v.id, f.id) AS id,
                   COALESCE(1.0 / (60 + v.v_rank), 0) AS v_score,
                   COALESCE(1.0 / (60 + f.f_rank), 0) AS f_score
            FROM vector_ranked v
            FULL OUTER JOIN fts_ranked f ON v.id = f.id
        )
        SELECT s.id, s.project_id, s.category, s.content, s.embedding,
               s.source_task_id, s.created_at, s.expires_at,
               s.hit_count, s.last_used_at, s.weight, s.task_type_affinity
        FROM fused
        JOIN ai_memory.semantic_rules s ON s.id = fused.id
        CROSS JOIN max_hits mh
        ORDER BY (
            0.35 * fused.v_score
          + 0.25 * fused.f_score
          + 0.20 * CASE s.category::text
                     WHEN 'constraint' THEN 1.0
                     WHEN 'lesson'     THEN 0.75
                     WHEN 'fact'       THEN 0.50
                     WHEN 'preference' THEN 0.25
                     ELSE 0.25 END
          + 0.10 * EXP(
              -1.0 * CASE s.category::text
                       WHEN 'constraint' THEN 0.0
                       WHEN 'lesson'     THEN 0.01
                       WHEN 'fact'       THEN 0.005
                       WHEN 'preference' THEN 0.02
                       ELSE 0.01 END
              * EXTRACT(EPOCH FROM (NOW() - COALESCE(s.last_used_at, s.created_at))) / 86400.0
            )
          + 0.10 * LN(1 + s.hit_count) / NULLIF(LN(1 + mh.val), 0)
        ) * COALESCE(s.weight, 1.0) DESC
        LIMIT $5"
    );

    let cat_str = category.as_ref().map(|c| match c {
        RuleCategory::Preference => "preference",
        RuleCategory::Fact => "fact",
        RuleCategory::Constraint => "constraint",
        RuleCategory::Lesson => "lesson",
    });

    let results: Vec<SemanticRule> = sqlx::query_as(&sql)
        .bind(project_id) // $1
        .bind(&emb) // $2
        .bind(candidate_limit) // $3
        .bind(&ts_query) // $4
        .bind(limit) // $5
        .bind(cat_str) // $6 (nullable)
        .bind(task_type) // $7 (nullable)
        .fetch_all(pool)
        .await?;

    increment_hit_counts(pool, &results);
    Ok(results)
}

/// Async background increment of hit_count and last_used_at (fire-and-forget with 5s timeout)
fn increment_hit_counts(pool: &PgPool, rules: &[SemanticRule]) {
    if rules.is_empty() {
        return;
    }
    let ids: Vec<Uuid> = rules.iter().map(|r| r.id).collect();
    let pool = pool.clone();
    tokio::spawn(async move {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            sqlx::query(
                "UPDATE ai_memory.semantic_rules SET hit_count = hit_count + 1, last_used_at = NOW() \
                 WHERE id = ANY($1)",
            )
            .bind(&ids)
            .execute(&pool),
        )
        .await;
        match result {
            Err(_) => tracing::warn!("hit_count increment timed out"),
            Ok(Err(e)) => tracing::warn!(error = %e, "Failed to increment hit_count"),
            Ok(Ok(_)) => {}
        }
    });
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
                "SELECT id, project_id, category, content, embedding, source_task_id, created_at, expires_at, hit_count, last_used_at, weight, task_type_affinity \
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
                "SELECT id, project_id, category, content, embedding, source_task_id, created_at, expires_at, hit_count, last_used_at, weight, task_type_affinity \
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
        "SELECT id, project_id, category, content, embedding, source_task_id, created_at, expires_at, hit_count, last_used_at, weight, task_type_affinity \
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
