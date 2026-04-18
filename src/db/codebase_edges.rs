use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::tree_sitter_chunker::CodeEdge;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct CodebaseEdge {
    pub id: Uuid,
    pub project_id: Uuid,
    pub source_entity: String,
    pub target_entity: String,
    pub edge_type: String,
    pub source_file: Option<String>,
    pub target_file: Option<String>,
    pub created_at: DateTime<Utc>,
}

pub async fn insert_edges(
    pool: &PgPool,
    project_id: Uuid,
    edges: &[CodeEdge],
) -> Result<u64, sqlx::Error> {
    if edges.is_empty() {
        return Ok(0);
    }

    let mut total = 0u64;
    for batch in edges.chunks(100) {
        let mut pids = Vec::with_capacity(batch.len());
        let mut sources = Vec::with_capacity(batch.len());
        let mut targets = Vec::with_capacity(batch.len());
        let mut types = Vec::with_capacity(batch.len());
        let mut src_files: Vec<Option<&str>> = Vec::with_capacity(batch.len());
        let mut tgt_files: Vec<Option<&str>> = Vec::with_capacity(batch.len());

        for e in batch {
            pids.push(project_id);
            sources.push(e.source_entity.as_str());
            targets.push(e.target_entity.as_str());
            types.push(e.edge_type.as_str());
            src_files.push(e.source_file.as_deref());
            tgt_files.push(e.target_file.as_deref());
        }

        let result = sqlx::query(
            "INSERT INTO ai_memory.codebase_edges \
             (project_id, source_entity, target_entity, edge_type, source_file, target_file) \
             SELECT * FROM UNNEST($1::uuid[], $2::text[], $3::text[], $4::text[], $5::text[], $6::text[]) \
             ON CONFLICT (project_id, source_entity, target_entity, edge_type) DO NOTHING",
        )
        .bind(&pids)
        .bind(&sources)
        .bind(&targets)
        .bind(&types)
        .bind(&src_files)
        .bind(&tgt_files)
        .execute(pool)
        .await?;
        total += result.rows_affected();
    }
    Ok(total)
}

pub async fn get_callers(
    pool: &PgPool,
    project_id: Uuid,
    entity: &str,
) -> Result<Vec<CodebaseEdge>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, project_id, source_entity, target_entity, edge_type, \
         source_file, target_file, created_at \
         FROM ai_memory.codebase_edges \
         WHERE project_id = $1 AND target_entity = $2",
    )
    .bind(project_id)
    .bind(entity)
    .fetch_all(pool)
    .await
}

pub async fn get_callees(
    pool: &PgPool,
    project_id: Uuid,
    entity: &str,
) -> Result<Vec<CodebaseEdge>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, project_id, source_entity, target_entity, edge_type, \
         source_file, target_file, created_at \
         FROM ai_memory.codebase_edges \
         WHERE project_id = $1 AND source_entity = $2",
    )
    .bind(project_id)
    .bind(entity)
    .fetch_all(pool)
    .await
}

/// BFS shortest path between two entities through the edge graph.
/// Returns the path as a sequence of edges, or empty if no path found.
pub async fn find_shortest_path(
    pool: &PgPool,
    project_id: Uuid,
    from: &str,
    to: &str,
    max_depth: i32,
) -> Result<Vec<CodebaseEdge>, sqlx::Error> {
    // BFS via iterative SQL: each level expands one hop from the frontier.
    // We track visited nodes and the parent edge that got us there.
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    // (current_node, edges_to_reach_it)
    let mut frontier: Vec<(String, Vec<CodebaseEdge>)> = vec![(from.to_string(), vec![])];
    visited.insert(from.to_string());

    for _depth in 0..max_depth {
        if frontier.is_empty() {
            break;
        }

        let current_nodes: Vec<String> = frontier.iter().map(|(n, _)| n.clone()).collect();

        let neighbors: Vec<CodebaseEdge> = sqlx::query_as(
            "SELECT id, project_id, source_entity, target_entity, edge_type, \
             source_file, target_file, created_at \
             FROM ai_memory.codebase_edges \
             WHERE project_id = $1 AND source_entity = ANY($2)",
        )
        .bind(project_id)
        .bind(&current_nodes)
        .fetch_all(pool)
        .await?;

        // Index frontier by node for fast lookup
        let frontier_map: std::collections::HashMap<&str, &Vec<CodebaseEdge>> = frontier
            .iter()
            .map(|(n, path)| (n.as_str(), path))
            .collect();

        let mut next_frontier = Vec::new();
        for edge in &neighbors {
            let target = &edge.target_entity;
            if target == to {
                let mut path = frontier_map[edge.source_entity.as_str()].clone();
                path.push(edge.clone());
                return Ok(path);
            }
            if !visited.contains(target) {
                visited.insert(target.clone());
                let mut path = frontier_map[edge.source_entity.as_str()].clone();
                path.push(edge.clone());
                next_frontier.push((target.clone(), path));
            }
        }
        frontier = next_frontier;
    }

    Ok(vec![])
}

pub async fn get_all_edges(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<CodebaseEdge>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, project_id, source_entity, target_entity, edge_type, \
         source_file, target_file, created_at \
         FROM ai_memory.codebase_edges \
         WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

pub async fn delete_project_edges(pool: &PgPool, project_id: Uuid) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM ai_memory.codebase_edges WHERE project_id = $1")
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn delete_file_edges(
    pool: &PgPool,
    project_id: Uuid,
    file_path: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM ai_memory.codebase_edges \
         WHERE project_id = $1 AND (source_file = $2 OR target_file = $2)",
    )
    .bind(project_id)
    .bind(file_path)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}
