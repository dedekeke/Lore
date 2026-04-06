use chrono::{DateTime, Utc};
use pgvector::Vector;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct CodeChunk {
    pub id: Uuid,
    pub project_id: Uuid,
    pub file_path: String,
    pub start_line: i32,
    pub end_line: i32,
    pub language: Option<String>,
    pub content: String,
    #[serde(skip)]
    #[allow(dead_code)]
    pub embedding: Option<Vector>,
    pub file_hash: String,
    pub indexed_at: DateTime<Utc>,
}

/// Get existing file hashes for a project (for incremental indexing)
pub async fn get_file_hashes(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT DISTINCT file_path, file_hash FROM ai_memory.code_chunks WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// Delete all chunks for a specific file (before re-indexing)
pub async fn delete_file_chunks(
    pool: &PgPool,
    project_id: Uuid,
    file_path: &str,
) -> Result<u64, sqlx::Error> {
    let result =
        sqlx::query("DELETE FROM ai_memory.code_chunks WHERE project_id = $1 AND file_path = $2")
            .bind(project_id)
            .bind(file_path)
            .execute(pool)
            .await?;
    Ok(result.rows_affected())
}

/// Delete all chunks for files no longer present in the project
pub async fn delete_stale_files(
    pool: &PgPool,
    project_id: Uuid,
    current_files: &[String],
) -> Result<u64, sqlx::Error> {
    if current_files.is_empty() {
        return Ok(0);
    }
    let result = sqlx::query(
        "DELETE FROM ai_memory.code_chunks WHERE project_id = $1 AND file_path != ALL($2)",
    )
    .bind(project_id)
    .bind(current_files)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Delete stale chunks for a file (start_lines that no longer exist after re-chunking)
pub async fn delete_stale_start_lines(
    pool: &PgPool,
    project_id: Uuid,
    file_path: &str,
    valid_start_lines: &[i32],
) -> Result<u64, sqlx::Error> {
    if valid_start_lines.is_empty() {
        return Ok(0);
    }
    let result = sqlx::query(
        "DELETE FROM ai_memory.code_chunks \
         WHERE project_id = $1 AND file_path = $2 AND start_line != ALL($3)",
    )
    .bind(project_id)
    .bind(file_path)
    .bind(valid_start_lines)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Batch insert code chunks
pub async fn insert_chunks(
    pool: &PgPool,
    project_id: Uuid,
    chunks: &[NewCodeChunk],
) -> Result<u64, sqlx::Error> {
    if chunks.is_empty() {
        return Ok(0);
    }

    let mut total = 0u64;
    // Insert in batches of 100 to avoid parameter limits
    for batch in chunks.chunks(100) {
        let mut project_ids = Vec::with_capacity(batch.len());
        let mut paths = Vec::with_capacity(batch.len());
        let mut starts = Vec::with_capacity(batch.len());
        let mut ends = Vec::with_capacity(batch.len());
        let mut langs: Vec<Option<String>> = Vec::with_capacity(batch.len());
        let mut contents = Vec::with_capacity(batch.len());
        let mut embeddings: Vec<Option<Vector>> = Vec::with_capacity(batch.len());
        let mut hashes = Vec::with_capacity(batch.len());

        for c in batch {
            project_ids.push(project_id);
            paths.push(c.file_path.as_str());
            starts.push(c.start_line);
            ends.push(c.end_line);
            langs.push(c.language.clone());
            contents.push(c.content.as_str());
            embeddings.push(c.embedding.as_ref().map(|e| Vector::from(e.to_vec())));
            hashes.push(c.file_hash.as_str());
        }

        let result = sqlx::query(
            "INSERT INTO ai_memory.code_chunks \
             (project_id, file_path, start_line, end_line, language, content, embedding, file_hash) \
             SELECT * FROM UNNEST($1::uuid[], $2::text[], $3::int[], $4::int[], $5::text[], $6::text[], $7::vector[], $8::text[]) \
             ON CONFLICT (project_id, file_path, start_line) DO UPDATE \
             SET content = EXCLUDED.content, \
                 embedding = COALESCE(EXCLUDED.embedding, ai_memory.code_chunks.embedding), \
                 file_hash = EXCLUDED.file_hash, end_line = EXCLUDED.end_line, \
                 language = EXCLUDED.language, indexed_at = NOW()",
        )
        .bind(&project_ids)
        .bind(&paths)
        .bind(&starts)
        .bind(&ends)
        .bind(&langs)
        .bind(&contents)
        .bind(&embeddings)
        .bind(&hashes)
        .execute(pool)
        .await?;
        total += result.rows_affected();
    }
    Ok(total)
}

/// Hybrid search: vector + BM25 RRF on code chunks
/// Uses nullable param pattern: always bind file_pattern, filter with `$N::text IS NULL OR ...`
pub async fn search_chunks(
    pool: &PgPool,
    project_id: Uuid,
    embedding: &[f32],
    query: &str,
    limit: i64,
    file_pattern: Option<&str>,
) -> Result<Vec<CodeChunk>, sqlx::Error> {
    let emb = Vector::from(embedding.to_vec());
    let file_filter = "AND ($4::text IS NULL OR file_path LIKE $4)";

    // If no usable text query, fall back to vector-only
    let has_words = query.split_whitespace().any(|w| {
        !w.chars()
            .all(|c| matches!(c, '\'' | '\\' | ':' | '&' | '|' | '!' | '(' | ')' | '*'))
    });
    if !has_words {
        let sql = format!(
            "SELECT id, project_id, file_path, start_line, end_line, language, content, \
             embedding, file_hash, indexed_at \
             FROM ai_memory.code_chunks \
             WHERE project_id = $1 AND embedding IS NOT NULL {file_filter} \
             ORDER BY embedding <=> $2::vector LIMIT $3"
        );
        return sqlx::query_as(&sql)
            .bind(project_id)
            .bind(&emb)
            .bind(limit)
            .bind(file_pattern)
            .fetch_all(pool)
            .await;
    }

    let candidate_limit = limit * 3;

    let sql = format!(
        "WITH vector_ranked AS (
            SELECT id, ROW_NUMBER() OVER (ORDER BY embedding <=> $2::vector) AS v_rank
            FROM ai_memory.code_chunks
            WHERE project_id = $1 AND embedding IS NOT NULL {file_filter}
            LIMIT $5
        ),
        fts_ranked AS (
            SELECT id, ROW_NUMBER() OVER (ORDER BY ts_rank_cd(content_tsv, plainto_tsquery('english', $6)) DESC) AS f_rank
            FROM ai_memory.code_chunks
            WHERE project_id = $1 AND content_tsv @@ plainto_tsquery('english', $6) {file_filter}
            LIMIT $5
        ),
        fused AS (
            SELECT COALESCE(v.id, f.id) AS id,
                   COALESCE(1.0 / (60 + v.v_rank), 0) + COALESCE(1.0 / (60 + f.f_rank), 0) AS rrf_score
            FROM vector_ranked v
            FULL OUTER JOIN fts_ranked f ON v.id = f.id
        )
        SELECT s.id, s.project_id, s.file_path, s.start_line, s.end_line, s.language,
               s.content, s.embedding, s.file_hash, s.indexed_at
        FROM fused
        JOIN ai_memory.code_chunks s ON s.id = fused.id
        ORDER BY fused.rrf_score DESC
        LIMIT $3"
    );

    sqlx::query_as(&sql)
        .bind(project_id) // $1
        .bind(&emb) // $2
        .bind(limit) // $3
        .bind(file_pattern) // $4
        .bind(candidate_limit) // $5
        .bind(query) // $6 — plainto_tsquery handles raw input safely
        .fetch_all(pool)
        .await
}

/// Count indexed chunks for a project
pub async fn count_chunks(pool: &PgPool, project_id: Uuid) -> Result<i64, sqlx::Error> {
    let row: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM ai_memory.code_chunks WHERE project_id = $1")
            .bind(project_id)
            .fetch_one(pool)
            .await?;
    Ok(row.0)
}

/// Get index stats: file count, chunk count, last indexed time
pub async fn get_index_stats(pool: &PgPool, project_id: Uuid) -> Result<IndexStats, sqlx::Error> {
    let row: (i64, i64, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT COUNT(DISTINCT file_path), COUNT(*), MAX(indexed_at) \
         FROM ai_memory.code_chunks WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await?;
    Ok(IndexStats {
        file_count: row.0,
        chunk_count: row.1,
        last_indexed_at: row.2,
    })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexStats {
    pub file_count: i64,
    pub chunk_count: i64,
    pub last_indexed_at: Option<DateTime<Utc>>,
}

pub struct NewCodeChunk {
    pub file_path: String,
    pub start_line: i32,
    pub end_line: i32,
    pub language: Option<String>,
    pub content: String,
    pub embedding: Option<Vec<f32>>,
    pub file_hash: String,
}
