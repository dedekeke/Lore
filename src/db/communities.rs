use sqlx::PgPool;
use uuid::Uuid;

/// Batch update community_id on code_chunks by matching (chunk_name, file_path).
/// Uses file_path when available for unique matching — common names like "new" or "build"
/// can appear in multiple files.
pub async fn update_chunk_communities(
    pool: &PgPool,
    project_id: Uuid,
    assignments: &[(String, Option<String>, i32)],
) -> Result<u64, sqlx::Error> {
    if assignments.is_empty() {
        return Ok(0);
    }

    let mut total = 0u64;
    for batch in assignments.chunks(500) {
        let names: Vec<&str> = batch.iter().map(|(n, _, _)| n.as_str()).collect();
        let files: Vec<Option<&str>> = batch.iter().map(|(_, f, _)| f.as_deref()).collect();
        let ids: Vec<i32> = batch.iter().map(|(_, _, c)| *c).collect();

        let result = sqlx::query(
            "UPDATE ai_memory.code_chunks AS c \
             SET community_id = u.community_id \
             FROM UNNEST($2::text[], $3::text[], $4::int[]) AS u(chunk_name, file_path, community_id) \
             WHERE c.project_id = $1 AND c.chunk_name = u.chunk_name \
             AND (u.file_path IS NULL OR c.file_path = u.file_path)",
        )
        .bind(project_id)
        .bind(&names)
        .bind(&files)
        .bind(&ids)
        .execute(pool)
        .await?;
        total += result.rows_affected();
    }
    Ok(total)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CommunityStat {
    pub community_id: i32,
    pub member_count: i64,
}

/// Get community_id -> count for a project
pub async fn get_communities(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<CommunityStat>, sqlx::Error> {
    sqlx::query_as::<_, (i32, i64)>(
        "SELECT community_id, COUNT(*) as cnt \
         FROM ai_memory.code_chunks \
         WHERE project_id = $1 AND community_id IS NOT NULL \
         GROUP BY community_id \
         ORDER BY cnt DESC",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(|(community_id, member_count)| CommunityStat {
                community_id,
                member_count,
            })
            .collect()
    })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CommunityMember {
    pub chunk_name: String,
    pub file_path: String,
    pub chunk_kind: Option<String>,
    pub start_line: i32,
    pub end_line: i32,
}

/// Get all chunks in a specific community
pub async fn get_community_members(
    pool: &PgPool,
    project_id: Uuid,
    community_id: i32,
) -> Result<Vec<CommunityMember>, sqlx::Error> {
    sqlx::query_as::<_, (Option<String>, String, Option<String>, i32, i32)>(
        "SELECT chunk_name, file_path, chunk_kind, start_line, end_line \
         FROM ai_memory.code_chunks \
         WHERE project_id = $1 AND community_id = $2 AND chunk_name IS NOT NULL \
         ORDER BY file_path, start_line",
    )
    .bind(project_id)
    .bind(community_id)
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(
                |(name, file_path, chunk_kind, start_line, end_line)| CommunityMember {
                    chunk_name: name.unwrap_or_default(),
                    file_path,
                    chunk_kind,
                    start_line,
                    end_line,
                },
            )
            .collect()
    })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AffectedCommunity {
    pub community_id: i32,
    pub member_count: i64,
}

/// Get distinct communities affected by changes to given file paths
pub async fn get_affected_communities(
    pool: &PgPool,
    project_id: Uuid,
    file_paths: &[String],
) -> Result<Vec<AffectedCommunity>, sqlx::Error> {
    if file_paths.is_empty() {
        return Ok(vec![]);
    }

    sqlx::query_as::<_, (i32, i64)>(
        "SELECT c.community_id, total.cnt \
         FROM ( \
             SELECT DISTINCT community_id \
             FROM ai_memory.code_chunks \
             WHERE project_id = $1 AND file_path = ANY($2) AND community_id IS NOT NULL \
         ) c \
         JOIN LATERAL ( \
             SELECT COUNT(*) as cnt FROM ai_memory.code_chunks \
             WHERE project_id = $1 AND community_id = c.community_id \
         ) total ON true \
         ORDER BY total.cnt DESC",
    )
    .bind(project_id)
    .bind(file_paths)
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(|(community_id, member_count)| AffectedCommunity {
                community_id,
                member_count,
            })
            .collect()
    })
}
