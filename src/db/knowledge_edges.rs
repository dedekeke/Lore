use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeEdge {
    pub id: Uuid,
    pub source_entity: String,
    pub target_entity: String,
    pub edge_type: String,
    pub confidence: Option<f64>,
    pub source_task_id: Option<Uuid>,
    pub project_id: Uuid,
    pub created_at: DateTime<Utc>,
}

pub async fn create_edge(
    pool: &PgPool,
    project_id: Uuid,
    source_entity: &str,
    target_entity: &str,
    edge_type: &str,
    confidence: Option<f64>,
    source_task_id: Option<Uuid>,
) -> Result<Uuid, sqlx::Error> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO ai_memory.knowledge_edges \
         (project_id, source_entity, target_entity, edge_type, confidence, source_task_id) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         RETURNING id",
    )
    .bind(project_id)
    .bind(source_entity)
    .bind(target_entity)
    .bind(edge_type)
    .bind(confidence.unwrap_or(1.0))
    .bind(source_task_id)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

pub async fn get_neighbors(
    pool: &PgPool,
    project_id: Uuid,
    entity: &str,
    edge_type: Option<&str>,
) -> Result<Vec<KnowledgeEdge>, sqlx::Error> {
    match edge_type {
        Some(et) => {
            sqlx::query_as(
                "SELECT id, source_entity, target_entity, edge_type, confidence, \
                 source_task_id, project_id, created_at \
                 FROM ai_memory.knowledge_edges \
                 WHERE project_id = $1 AND (source_entity = $2 OR target_entity = $2) \
                 AND edge_type = $3 \
                 ORDER BY created_at",
            )
            .bind(project_id)
            .bind(entity)
            .bind(et)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as(
                "SELECT id, source_entity, target_entity, edge_type, confidence, \
                 source_task_id, project_id, created_at \
                 FROM ai_memory.knowledge_edges \
                 WHERE project_id = $1 AND (source_entity = $2 OR target_entity = $2) \
                 ORDER BY created_at",
            )
            .bind(project_id)
            .bind(entity)
            .fetch_all(pool)
            .await
        }
    }
}

/// BFS traversal to find all entities within `depth` hops.
/// Returns edges grouped by depth level.
pub async fn query_neighbors_bfs(
    pool: &PgPool,
    project_id: Uuid,
    entity: &str,
    edge_type: Option<&str>,
    depth: u32,
) -> Result<Vec<KnowledgeEdge>, sqlx::Error> {
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(entity.to_string());
    let mut frontier: Vec<String> = vec![entity.to_string()];
    let mut all_edges: Vec<KnowledgeEdge> = Vec::new();
    let mut seen_edge_ids: HashSet<Uuid> = HashSet::new();

    for _ in 0..depth {
        if frontier.is_empty() {
            break;
        }
        let mut next_frontier: Vec<String> = Vec::new();
        for node in &frontier {
            let edges = get_neighbors(pool, project_id, node, edge_type).await?;
            for edge in edges {
                if seen_edge_ids.insert(edge.id) {
                    let neighbor = if edge.source_entity == *node {
                        &edge.target_entity
                    } else {
                        &edge.source_entity
                    };
                    if visited.insert(neighbor.clone()) {
                        next_frontier.push(neighbor.clone());
                    }
                    all_edges.push(edge);
                }
            }
        }
        frontier = next_frontier;
    }

    Ok(all_edges)
}

/// BFS shortest path between two entities. Returns the edge path or empty vec if unreachable.
pub async fn find_path(
    pool: &PgPool,
    project_id: Uuid,
    from_entity: &str,
    to_entity: &str,
    max_depth: u32,
) -> Result<Vec<KnowledgeEdge>, sqlx::Error> {
    if from_entity == to_entity {
        return Ok(vec![]);
    }

    // BFS: track parent edges for path reconstruction
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(from_entity.to_string());
    // Maps entity -> (edge that led to it, previous entity)
    let mut parent_map: HashMap<String, (KnowledgeEdge, String)> = HashMap::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(from_entity.to_string());

    let mut found = false;
    for _ in 0..max_depth {
        if queue.is_empty() || found {
            break;
        }
        let level_size = queue.len();
        for _ in 0..level_size {
            let current = queue.pop_front().unwrap();
            let edges = get_neighbors(pool, project_id, &current, None).await?;
            for edge in edges {
                let neighbor = if edge.source_entity == current {
                    edge.target_entity.clone()
                } else {
                    edge.source_entity.clone()
                };
                if visited.insert(neighbor.clone()) {
                    parent_map.insert(neighbor.clone(), (edge, current.clone()));
                    if neighbor == to_entity {
                        found = true;
                        break;
                    }
                    queue.push_back(neighbor);
                }
            }
            if found {
                break;
            }
        }
    }

    if !found {
        return Ok(vec![]);
    }

    // Reconstruct path from to_entity back to from_entity
    let mut path: Vec<KnowledgeEdge> = Vec::new();
    let mut current = to_entity.to_string();
    while let Some((edge, prev)) = parent_map.remove(&current) {
        path.push(edge);
        current = prev;
    }
    path.reverse();
    Ok(path)
}
