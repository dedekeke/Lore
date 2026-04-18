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
    pub summary: Option<String>,
    pub behavior_version: i32,
    pub chunk_name: Option<String>,
    pub chunk_kind: Option<String>,
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
    for batch in chunks.chunks(100) {
        let mut project_ids = Vec::with_capacity(batch.len());
        let mut paths = Vec::with_capacity(batch.len());
        let mut starts = Vec::with_capacity(batch.len());
        let mut ends = Vec::with_capacity(batch.len());
        let mut langs: Vec<Option<String>> = Vec::with_capacity(batch.len());
        let mut contents = Vec::with_capacity(batch.len());
        let mut embeddings: Vec<Option<Vector>> = Vec::with_capacity(batch.len());
        let mut hashes = Vec::with_capacity(batch.len());
        let mut versions = Vec::with_capacity(batch.len());
        let mut names: Vec<Option<&str>> = Vec::with_capacity(batch.len());
        let mut kinds: Vec<Option<&str>> = Vec::with_capacity(batch.len());

        for c in batch {
            project_ids.push(project_id);
            paths.push(c.file_path.as_str());
            starts.push(c.start_line);
            ends.push(c.end_line);
            langs.push(c.language.clone());
            contents.push(c.content.as_str());
            embeddings.push(c.embedding.as_ref().map(|e| Vector::from(e.to_vec())));
            hashes.push(c.file_hash.as_str());
            versions.push(c.behavior_version);
            names.push(c.chunk_name.as_deref());
            kinds.push(c.chunk_kind.as_deref());
        }

        let result = sqlx::query(
            "INSERT INTO ai_memory.code_chunks \
             (project_id, file_path, start_line, end_line, language, content, embedding, file_hash, behavior_version, chunk_name, chunk_kind) \
             SELECT * FROM UNNEST($1::uuid[], $2::text[], $3::int[], $4::int[], $5::text[], $6::text[], $7::vector[], $8::text[], $9::int[], $10::text[], $11::text[]) \
             ON CONFLICT (project_id, file_path, start_line) DO UPDATE \
             SET content = EXCLUDED.content, \
                 embedding = COALESCE(EXCLUDED.embedding, ai_memory.code_chunks.embedding), \
                 file_hash = EXCLUDED.file_hash, end_line = EXCLUDED.end_line, \
                 language = EXCLUDED.language, behavior_version = EXCLUDED.behavior_version, \
                 chunk_name = EXCLUDED.chunk_name, chunk_kind = EXCLUDED.chunk_kind, \
                 indexed_at = NOW()",
        )
        .bind(&project_ids)
        .bind(&paths)
        .bind(&starts)
        .bind(&ends)
        .bind(&langs)
        .bind(&contents)
        .bind(&embeddings)
        .bind(&hashes)
        .bind(&versions)
        .bind(&names)
        .bind(&kinds)
        .execute(pool)
        .await?;
        total += result.rows_affected();
    }
    Ok(total)
}

/// Hybrid search: vector + BM25 RRF on code chunks, with optional MMR re-ranking.
/// `diversity` controls MMR: 0.0 = pure relevance, 1.0 = max diversity. None skips MMR.
pub async fn search_chunks(
    pool: &PgPool,
    project_id: Uuid,
    embedding: &[f32],
    query: &str,
    limit: i64,
    file_pattern: Option<&str>,
    diversity: Option<f32>,
) -> Result<Vec<CodeChunk>, sqlx::Error> {
    let emb = Vector::from(embedding.to_vec());
    let file_filter = "AND ($4::text IS NULL OR file_path LIKE $4)";

    let use_mmr = diversity.is_some_and(|d| d > 0.0);
    let fetch_limit = if use_mmr { limit * 4 } else { limit };

    let has_words = query.split_whitespace().any(|w| {
        !w.chars()
            .all(|c| matches!(c, '\'' | '\\' | ':' | '&' | '|' | '!' | '(' | ')' | '*'))
    });

    let candidates = if !has_words {
        let sql = format!(
            "SELECT id, project_id, file_path, start_line, end_line, language, content, \
             embedding, file_hash, indexed_at, summary, behavior_version, chunk_name, chunk_kind \
             FROM ai_memory.code_chunks \
             WHERE project_id = $1 AND embedding IS NOT NULL {file_filter} \
             ORDER BY embedding <=> $2::vector LIMIT $3"
        );
        sqlx::query_as(&sql)
            .bind(project_id)
            .bind(&emb)
            .bind(fetch_limit)
            .bind(file_pattern)
            .fetch_all(pool)
            .await?
    } else {
        let candidate_limit = fetch_limit * 3;
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
                   s.content, s.embedding, s.file_hash, s.indexed_at, s.summary, s.behavior_version, \
                   s.chunk_name, s.chunk_kind
            FROM fused
            JOIN ai_memory.code_chunks s ON s.id = fused.id
            ORDER BY fused.rrf_score DESC
            LIMIT $3"
        );
        sqlx::query_as(&sql)
            .bind(project_id)
            .bind(&emb)
            .bind(fetch_limit)
            .bind(file_pattern)
            .bind(candidate_limit)
            .bind(query)
            .fetch_all(pool)
            .await?
    };

    if use_mmr {
        Ok(mmr_rerank(
            candidates,
            embedding,
            limit as usize,
            diversity.unwrap(),
        ))
    } else {
        Ok(candidates)
    }
}

/// Maximal Marginal Relevance: greedily select results balancing relevance vs diversity.
/// lambda=0.0 pure diversity, lambda=1.0 pure relevance (diversity param is inverted: 0.3 -> lambda=0.7)
fn mmr_rerank(
    candidates: Vec<CodeChunk>,
    query_emb: &[f32],
    k: usize,
    diversity: f32,
) -> Vec<CodeChunk> {
    if candidates.len() <= k {
        return candidates;
    }

    let lambda = 1.0 - diversity.clamp(0.0, 1.0);

    // Pre-compute cosine similarities to query
    let query_sims: Vec<f32> = candidates
        .iter()
        .map(|c| {
            c.embedding
                .as_ref()
                .map(|e| cosine_sim(e.as_slice(), query_emb))
                .unwrap_or(0.0)
        })
        .collect();

    let mut selected: Vec<usize> = Vec::with_capacity(k);
    let mut remaining: Vec<usize> = (0..candidates.len()).collect();

    // First pick: highest query similarity
    let first = remaining
        .iter()
        .copied()
        .max_by(|&a, &b| {
            query_sims[a]
                .partial_cmp(&query_sims[b])
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap();
    selected.push(first);
    remaining.retain(|&i| i != first);

    while selected.len() < k && !remaining.is_empty() {
        let best = remaining
            .iter()
            .copied()
            .max_by(|&a, &b| {
                let score_a = mmr_score(a, &selected, &candidates, &query_sims, lambda);
                let score_b = mmr_score(b, &selected, &candidates, &query_sims, lambda);
                score_a
                    .partial_cmp(&score_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap();
        selected.push(best);
        remaining.retain(|&i| i != best);
    }

    selected
        .into_iter()
        .map(|i| candidates[i].clone())
        .collect()
}

fn mmr_score(
    candidate_idx: usize,
    selected: &[usize],
    candidates: &[CodeChunk],
    query_sims: &[f32],
    lambda: f32,
) -> f32 {
    let relevance = query_sims[candidate_idx];
    let max_sim_to_selected = selected
        .iter()
        .map(|&s| {
            match (
                &candidates[candidate_idx].embedding,
                &candidates[s].embedding,
            ) {
                (Some(a), Some(b)) => cosine_sim(a.as_slice(), b.as_slice()),
                _ => 0.0,
            }
        })
        .fold(0.0f32, f32::max);
    lambda * relevance - (1.0 - lambda) * max_sim_to_selected
}

fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
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

/// Get index stats: file count, chunk count, last indexed time, behavior version info
pub async fn get_index_stats(pool: &PgPool, project_id: Uuid) -> Result<IndexStats, sqlx::Error> {
    let row: (i64, i64, Option<DateTime<Utc>>, i64) = sqlx::query_as(
        "SELECT COUNT(DISTINCT file_path), COUNT(*), MAX(indexed_at), \
         COUNT(*) FILTER (WHERE summary IS NOT NULL) \
         FROM ai_memory.code_chunks WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await?;
    Ok(IndexStats {
        file_count: row.0,
        chunk_count: row.1,
        last_indexed_at: row.2,
        summarized_chunks: row.3,
    })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexStats {
    pub file_count: i64,
    pub chunk_count: i64,
    pub last_indexed_at: Option<DateTime<Utc>>,
    pub summarized_chunks: i64,
}

/// Get chunks that need LLM-generated summaries
pub async fn get_chunks_needing_summary(
    pool: &PgPool,
    project_id: Uuid,
    limit: i64,
) -> Result<Vec<CodeChunk>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, project_id, file_path, start_line, end_line, language, content, \
         embedding, file_hash, indexed_at, summary, behavior_version \
         FROM ai_memory.code_chunks \
         WHERE project_id = $1 AND summary IS NULL \
         ORDER BY indexed_at DESC LIMIT $2",
    )
    .bind(project_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Batch update summaries for code chunks
pub async fn update_summaries(
    pool: &PgPool,
    updates: &[(Uuid, String)],
) -> Result<u64, sqlx::Error> {
    let mut total = 0u64;
    for (id, summary) in updates {
        let result = sqlx::query("UPDATE ai_memory.code_chunks SET summary = $1 WHERE id = $2")
            .bind(summary)
            .bind(id)
            .execute(pool)
            .await?;
        total += result.rows_affected();
    }
    Ok(total)
}

/// Nullify embeddings for chunks with old behavior_version (triggers re-embedding on next index)
pub async fn mark_stale_embeddings(
    pool: &PgPool,
    project_id: Uuid,
    current_version: i32,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE ai_memory.code_chunks SET embedding = NULL, summary = NULL \
         WHERE project_id = $1 AND behavior_version < $2",
    )
    .bind(project_id)
    .bind(current_version)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Get file hashes with behavior_version for incremental indexing
pub async fn get_file_hashes_versioned(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<(String, String, i32)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT DISTINCT ON (file_path) file_path, file_hash, behavior_version \
         FROM ai_memory.code_chunks WHERE project_id = $1 ORDER BY file_path, indexed_at DESC",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

pub struct NewCodeChunk {
    pub file_path: String,
    pub start_line: i32,
    pub end_line: i32,
    pub language: Option<String>,
    pub content: String,
    pub embedding: Option<Vec<f32>>,
    pub file_hash: String,
    pub behavior_version: i32,
    pub chunk_name: Option<String>,
    pub chunk_kind: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_chunk(id_byte: u8, emb: Vec<f32>) -> CodeChunk {
        CodeChunk {
            id: Uuid::from_bytes([id_byte; 16]),
            project_id: Uuid::nil(),
            file_path: format!("file_{id_byte}.rs"),
            start_line: 1,
            end_line: 10,
            language: Some("rust".into()),
            content: format!("chunk {id_byte}"),
            embedding: Some(Vector::from(emb)),
            file_hash: "abc".into(),
            indexed_at: Utc::now(),
            summary: None,
            behavior_version: 1,
            chunk_name: None,
            chunk_kind: None,
        }
    }

    #[test]
    fn test_cosine_sim_identical() {
        let a = vec![1.0, 0.0, 0.0];
        assert!((cosine_sim(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_sim_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!(cosine_sim(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_sim_zero_vector() {
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 0.0];
        assert_eq!(cosine_sim(&a, &b), 0.0);
    }

    #[test]
    fn test_mmr_returns_k_results() {
        let candidates = vec![
            make_chunk(1, vec![1.0, 0.0, 0.0]),
            make_chunk(2, vec![0.9, 0.1, 0.0]),
            make_chunk(3, vec![0.0, 1.0, 0.0]),
            make_chunk(4, vec![0.0, 0.0, 1.0]),
        ];
        let query = vec![1.0, 0.0, 0.0];
        let result = mmr_rerank(candidates, &query, 3, 0.3);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_mmr_diversity_promotes_dissimilar() {
        // 4 candidates, select top-2: MMR should prefer the diverse chunk over the near-duplicate
        let candidates = vec![
            make_chunk(1, vec![1.0, 0.0, 0.0]),
            make_chunk(2, vec![0.99, 0.01, 0.0]), // near-duplicate of 1
            make_chunk(3, vec![0.0, 1.0, 0.0]),   // very different
            make_chunk(4, vec![0.98, 0.02, 0.0]), // another near-duplicate
        ];
        let query = vec![1.0, 0.0, 0.0];

        let result = mmr_rerank(candidates, &query, 2, 0.8);
        assert_eq!(result[0].id, Uuid::from_bytes([1; 16])); // most relevant
        assert_eq!(result[1].id, Uuid::from_bytes([3; 16])); // diverse pick over near-duplicates
    }

    #[test]
    fn test_mmr_fewer_candidates_than_k() {
        let candidates = vec![make_chunk(1, vec![1.0, 0.0])];
        let result = mmr_rerank(candidates.clone(), &[1.0, 0.0], 5, 0.3);
        assert_eq!(result.len(), 1);
    }
}
