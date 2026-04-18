use chrono::{DateTime, Utc};
use pgvector::Vector;
use sqlx::PgPool;
use uuid::Uuid;

pub const SIMILARITY_DEDUP_THRESHOLD: f64 = 0.95;
pub const SIMILARITY_CONTRADICTION_FLOOR: f64 = 0.80;
pub const SIMILARITY_CLUSTER_THRESHOLD: f64 = 0.88;

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
    pub tags: Vec<String>,
    pub valid_from: DateTime<Utc>,
    pub valid_until: Option<DateTime<Utc>>,
    pub project_name: Option<String>,
}

pub async fn create_rule(
    pool: &PgPool,
    project_id: Uuid,
    category: RuleCategory,
    content: &str,
    embedding: Option<&[f32]>,
    tags: &[String],
) -> Result<Uuid, sqlx::Error> {
    let emb = embedding.map(|e| Vector::from(e.to_vec()));
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO ai_memory.semantic_rules (project_id, category, content, embedding, tags) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(project_id)
    .bind(&category)
    .bind(content)
    .bind(emb.as_ref())
    .bind(tags)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

pub async fn get_rule(pool: &PgPool, id: Uuid) -> Result<Option<SemanticRule>, sqlx::Error> {
    sqlx::query_as(
        "SELECT s.id, s.project_id, s.category, s.content, s.embedding, s.source_task_id, s.created_at, s.expires_at, s.hit_count, s.last_used_at, s.weight, s.task_type_affinity, s.tags, s.valid_from, s.valid_until, p.name AS project_name \
         FROM ai_memory.semantic_rules s LEFT JOIN ai_memory.projects p ON p.id = s.project_id WHERE s.id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn count_rules(pool: &PgPool, project_id: Uuid) -> Result<i64, sqlx::Error> {
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM ai_memory.semantic_rules WHERE project_id = $1 AND valid_until IS NULL",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

pub async fn count_rules_by_category(
    pool: &PgPool,
    project_id: Uuid,
    category: RuleCategory,
) -> Result<i64, sqlx::Error> {
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM ai_memory.semantic_rules WHERE project_id = $1 AND category = $2 AND valid_until IS NULL",
    )
    .bind(project_id)
    .bind(&category)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

pub async fn list_rules(
    pool: &PgPool,
    project_id: Uuid,
    category: Option<RuleCategory>,
    tags: Option<&[String]>,
) -> Result<Vec<SemanticRule>, sqlx::Error> {
    sqlx::query_as(
        "SELECT s.id, s.project_id, s.category, s.content, s.embedding, s.source_task_id, s.created_at, s.expires_at, s.hit_count, s.last_used_at, s.weight, s.task_type_affinity, s.tags, s.valid_from, s.valid_until, p.name AS project_name \
         FROM ai_memory.semantic_rules s LEFT JOIN ai_memory.projects p ON p.id = s.project_id \
         WHERE s.project_id = $1 AND s.valid_until IS NULL \
         AND ($2::ai_memory.rule_category IS NULL OR s.category = $2) \
         AND ($3::text[] IS NULL OR s.tags @> $3) \
         ORDER BY s.created_at",
    )
    .bind(project_id)
    .bind(category.as_ref())
    .bind(tags)
    .fetch_all(pool)
    .await
}

pub async fn update_rule(
    pool: &PgPool,
    id: Uuid,
    category: Option<RuleCategory>,
    content: Option<&str>,
    embedding: Option<&[f32]>,
    tags: Option<&[String]>,
) -> Result<bool, sqlx::Error> {
    let emb = embedding.map(|e| Vector::from(e.to_vec()));
    let result = sqlx::query(
        "UPDATE ai_memory.semantic_rules SET \
         category = COALESCE($2, category), \
         content = COALESCE($3, content), \
         embedding = COALESCE($4, embedding), \
         tags = COALESCE($5, tags) \
         WHERE id = $1",
    )
    .bind(id)
    .bind(category.as_ref())
    .bind(content)
    .bind(emb.as_ref())
    .bind(tags)
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

/// Mark a rule as superseded instead of deleting it
pub async fn supersede_rule(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE ai_memory.semantic_rules SET valid_until = NOW() WHERE id = $1 AND valid_until IS NULL",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Weighted hybrid search: RRF (vector + BM25) + category priority + recency decay.
/// Falls back to vector-only if no query text (scoring degraded to cosine-only).
#[allow(clippy::too_many_arguments)]
pub async fn search_rules_hybrid(
    pool: &PgPool,
    project_id: Option<Uuid>,
    current_project_id: Uuid,
    embedding: &[f32],
    query: &str,
    limit: i64,
    category: Option<RuleCategory>,
    task_type: Option<&str>,
    tags: Option<&[String]>,
) -> Result<Vec<ScoredRule>, sqlx::Error> {
    let ts_query = to_tsquery_safe(query);
    if ts_query.is_empty() {
        let results = search_rules_by_embedding(
            pool,
            project_id,
            embedding,
            limit,
            category,
            tags,
            Some(current_project_id),
        )
        .await?;
        increment_hit_counts_scored(pool, &results);
        return Ok(results);
    }
    let limit = limit.max(1);
    let candidate_limit = limit * 3;
    let emb = Vector::from(embedding.to_vec());

    let project_filter = "AND ($1::uuid IS NULL OR project_id = $1)";
    let cat_filter = "AND ($6::ai_memory.rule_category IS NULL OR category = $6)";
    let affinity_filter =
        "AND (task_type_affinity IS NULL OR $7::text IS NULL OR $7 = ANY(task_type_affinity))";
    let tags_filter = "AND ($8::text[] IS NULL OR tags @> $8)";

    // The fused_score column is computed as the full ranking formula and exposed
    // alongside the rule so callers can use it for progressive disclosure.
    let sql = format!(
        "WITH vector_ranked AS (
            SELECT id, ROW_NUMBER() OVER (ORDER BY embedding <=> $2::vector) AS v_rank
            FROM ai_memory.semantic_rules
            WHERE embedding IS NOT NULL AND valid_until IS NULL
              AND (expires_at IS NULL OR expires_at > NOW()) {project_filter} {cat_filter} {affinity_filter} {tags_filter}
            LIMIT $3
        ),
        fts_ranked AS (
            SELECT id, ROW_NUMBER() OVER (ORDER BY ts_rank_cd(content_tsv, to_tsquery('english', $4)) DESC) AS f_rank
            FROM ai_memory.semantic_rules
            WHERE content_tsv @@ to_tsquery('english', $4) AND valid_until IS NULL
              AND (expires_at IS NULL OR expires_at > NOW()) {project_filter} {cat_filter} {affinity_filter} {tags_filter}
            LIMIT $3
        ),
        max_hits AS (
            SELECT GREATEST(MAX(hit_count), 1) AS val FROM ai_memory.semantic_rules WHERE ($1::uuid IS NULL OR project_id = $1)
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
               s.hit_count, s.last_used_at, s.weight, s.task_type_affinity, s.tags,
               s.valid_from, s.valid_until, p.name AS project_name,
               ((
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
               ) * COALESCE(s.weight, 1.0)
                 * CASE WHEN s.project_id = $9 THEN 2.0 ELSE 1.0 END
               )::float8 AS fused_score
        FROM fused
        JOIN ai_memory.semantic_rules s ON s.id = fused.id
        LEFT JOIN ai_memory.projects p ON p.id = s.project_id
        CROSS JOIN max_hits mh
        ORDER BY fused_score DESC
        LIMIT $5"
    );

    #[derive(sqlx::FromRow)]
    struct RuleWithScore {
        #[sqlx(flatten)]
        rule: SemanticRule,
        fused_score: f64,
    }

    let rows: Vec<RuleWithScore> = sqlx::query_as(&sql)
        .bind(project_id) // $1 (nullable)
        .bind(&emb) // $2
        .bind(candidate_limit) // $3
        .bind(&ts_query) // $4
        .bind(limit) // $5
        .bind(category.as_ref()) // $6 (nullable enum)
        .bind(task_type) // $7 (nullable)
        .bind(tags) // $8 (nullable)
        .bind(current_project_id) // $9
        .fetch_all(pool)
        .await?;

    let results: Vec<ScoredRule> = rows
        .into_iter()
        .map(|r| ScoredRule {
            rule: r.rule,
            score: r.fused_score,
        })
        .collect();

    increment_hit_counts_scored(pool, &results);
    Ok(results)
}

/// Fire-and-forget increment of hit_count and last_used_at for given rule IDs.
pub fn increment_hit_counts(pool: &PgPool, ids: &[Uuid]) {
    if ids.is_empty() {
        return;
    }
    let ids = ids.to_vec();
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

fn increment_hit_counts_scored(pool: &PgPool, rules: &[ScoredRule]) {
    let ids: Vec<Uuid> = rules.iter().map(|r| r.rule.id).collect();
    increment_hit_counts(pool, &ids);
}

/// Vector-only search (used when no text query available).
/// When `current_project_id` is provided, results from that project get a 2x boost.
/// Returns scored results where score = 1 - cosine_distance (higher is more similar).
pub async fn search_rules_by_embedding(
    pool: &PgPool,
    project_id: Option<Uuid>,
    embedding: &[f32],
    limit: i64,
    category: Option<RuleCategory>,
    tags: Option<&[String]>,
    current_project_id: Option<Uuid>,
) -> Result<Vec<ScoredRule>, sqlx::Error> {
    let emb = Vector::from(embedding.to_vec());
    let order_clause = "ORDER BY (s.embedding <=> $2::vector) / \
        CASE WHEN $6::uuid IS NOT NULL AND s.project_id = $6 THEN 2.0 ELSE 1.0 END";
    let sql = format!(
        "SELECT s.id, s.project_id, s.category, s.content, s.embedding, s.source_task_id, \
         s.created_at, s.expires_at, s.hit_count, s.last_used_at, s.weight, s.task_type_affinity, \
         s.tags, s.valid_from, s.valid_until, p.name AS project_name, \
         ((1.0 - (s.embedding <=> $2::vector)) * CASE WHEN $6::uuid IS NOT NULL AND s.project_id = $6 THEN 2.0 ELSE 1.0 END)::float8 AS cosine_score \
         FROM ai_memory.semantic_rules s LEFT JOIN ai_memory.projects p ON p.id = s.project_id \
         WHERE ($1::uuid IS NULL OR s.project_id = $1) AND s.embedding IS NOT NULL \
         AND s.valid_until IS NULL \
         AND ($4::ai_memory.rule_category IS NULL OR s.category = $4) \
         AND ($5::text[] IS NULL OR s.tags @> $5) \
         AND (s.expires_at IS NULL OR s.expires_at > NOW()) \
         {order_clause} LIMIT $3"
    );

    #[derive(sqlx::FromRow)]
    struct RuleWithCosine {
        #[sqlx(flatten)]
        rule: SemanticRule,
        cosine_score: f64,
    }

    let rows: Vec<RuleWithCosine> = sqlx::query_as(&sql)
        .bind(project_id)
        .bind(&emb)
        .bind(limit)
        .bind(category.as_ref())
        .bind(tags)
        .bind(current_project_id)
        .fetch_all(pool)
        .await?;

    Ok(rows
        .into_iter()
        .map(|r| ScoredRule {
            rule: r.rule,
            score: r.cosine_score,
        })
        .collect())
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
        "SELECT s.id, s.project_id, s.category, s.content, s.embedding, s.source_task_id, s.created_at, s.expires_at, s.hit_count, s.last_used_at, s.weight, s.task_type_affinity, s.tags, s.valid_from, s.valid_until, p.name AS project_name \
         FROM ai_memory.semantic_rules s LEFT JOIN ai_memory.projects p ON p.id = s.project_id \
         WHERE s.project_id = $1 AND s.embedding IS NOT NULL AND s.valid_until IS NULL \
         AND (1.0 - (s.embedding <=> $2::vector)) >= $3 \
         ORDER BY s.embedding <=> $2::vector LIMIT 5",
    )
    .bind(project_id)
    .bind(&emb)
    .bind(threshold)
    .fetch_all(pool)
    .await
}

/// Find rules in the contradiction band (SIMILARITY_CONTRADICTION_FLOOR to SIMILARITY_DEDUP_THRESHOLD)
pub async fn find_potential_contradictions(
    pool: &PgPool,
    project_id: Uuid,
    embedding: &[f32],
) -> Result<Vec<SemanticRule>, sqlx::Error> {
    let emb = Vector::from(embedding.to_vec());
    // TODO: combine with find_duplicates into single scan (fetch >= CONTRADICTION_FLOOR, partition in app code)
    sqlx::query_as(
        "SELECT s.id, s.project_id, s.category, s.content, s.embedding, s.source_task_id, s.created_at, s.expires_at, s.hit_count, s.last_used_at, s.weight, s.task_type_affinity, s.tags, s.valid_from, s.valid_until, p.name AS project_name \
         FROM ai_memory.semantic_rules s LEFT JOIN ai_memory.projects p ON p.id = s.project_id \
         WHERE s.project_id = $1 AND s.embedding IS NOT NULL AND s.valid_until IS NULL \
         AND (1.0 - (s.embedding <=> $2::vector)) >= $3 \
         AND (1.0 - (s.embedding <=> $2::vector)) < $4 \
         ORDER BY s.embedding <=> $2::vector LIMIT 5",
    )
    .bind(project_id)
    .bind(&emb)
    .bind(SIMILARITY_CONTRADICTION_FLOOR)
    .bind(SIMILARITY_DEDUP_THRESHOLD)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct DuplicatePair {
    pub rule_a_id: Uuid,
    pub rule_a_content: String,
    pub rule_b_id: Uuid,
    pub rule_b_content: String,
    pub similarity: f64,
}

/// Find pairs of rules with cosine similarity >= SIMILARITY_CLUSTER_THRESHOLD.
/// NOTE: self-join is O(n²) on active rules per project — acceptable for <1k rules,
/// revisit with ANN index or chunked scans if rule count grows significantly.
pub async fn find_duplicate_clusters(
    pool: &PgPool,
    project_id: Uuid,
    limit: i64,
) -> Result<Vec<DuplicatePair>, sqlx::Error> {
    sqlx::query_as(
        "SELECT a.id AS rule_a_id, a.content AS rule_a_content, \
         b.id AS rule_b_id, b.content AS rule_b_content, \
         (1.0 - (a.embedding <=> b.embedding))::float8 AS similarity \
         FROM ai_memory.semantic_rules a \
         JOIN ai_memory.semantic_rules b ON a.id < b.id \
         WHERE a.project_id = $1 AND b.project_id = $1 \
         AND a.embedding IS NOT NULL AND b.embedding IS NOT NULL \
         AND a.valid_until IS NULL AND b.valid_until IS NULL \
         AND (1.0 - (a.embedding <=> b.embedding)) >= $2 \
         ORDER BY similarity DESC LIMIT $3",
    )
    .bind(project_id)
    .bind(SIMILARITY_CLUSTER_THRESHOLD)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Rule with its fused relevance score from hybrid search.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScoredRule {
    #[serde(flatten)]
    pub rule: SemanticRule,
    pub score: f64,
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
