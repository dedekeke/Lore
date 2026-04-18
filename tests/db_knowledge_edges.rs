mod common;

use lore::db::{knowledge_edges, projects};

#[tokio::test]
async fn test_create_edge() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let id = knowledge_edges::create_edge(
        &pool,
        pid,
        "auth_module",
        "user_table",
        "depends_on",
        None,
        None,
    )
    .await
    .unwrap();
    assert!(!id.is_nil());
}

#[tokio::test]
async fn test_query_neighbors_depth_1() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    knowledge_edges::create_edge(&pool, pid, "A", "B", "uses", None, None)
        .await
        .unwrap();
    knowledge_edges::create_edge(&pool, pid, "A", "C", "uses", None, None)
        .await
        .unwrap();
    knowledge_edges::create_edge(&pool, pid, "B", "D", "uses", None, None)
        .await
        .unwrap();

    let edges = knowledge_edges::query_neighbors_bfs(&pool, pid, "A", None, 1)
        .await
        .unwrap();
    assert_eq!(edges.len(), 2);

    let neighbors: Vec<&str> = edges
        .iter()
        .map(|e| {
            if e.source_entity == "A" {
                e.target_entity.as_str()
            } else {
                e.source_entity.as_str()
            }
        })
        .collect();
    assert!(neighbors.contains(&"B"));
    assert!(neighbors.contains(&"C"));
}

#[tokio::test]
async fn test_query_neighbors_depth_2() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    knowledge_edges::create_edge(&pool, pid, "A", "B", "uses", None, None)
        .await
        .unwrap();
    knowledge_edges::create_edge(&pool, pid, "B", "C", "uses", None, None)
        .await
        .unwrap();
    knowledge_edges::create_edge(&pool, pid, "C", "D", "uses", None, None)
        .await
        .unwrap();

    let edges = knowledge_edges::query_neighbors_bfs(&pool, pid, "A", None, 2)
        .await
        .unwrap();
    // depth=2: A->B (depth 1), B->C (depth 2). C->D is depth 3, excluded.
    assert_eq!(edges.len(), 2);
}

#[tokio::test]
async fn test_query_neighbors_with_edge_type_filter() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    knowledge_edges::create_edge(&pool, pid, "A", "B", "uses", None, None)
        .await
        .unwrap();
    knowledge_edges::create_edge(&pool, pid, "A", "C", "depends_on", None, None)
        .await
        .unwrap();

    let edges = knowledge_edges::query_neighbors_bfs(&pool, pid, "A", Some("uses"), 1)
        .await
        .unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].target_entity, "B");
}

#[tokio::test]
async fn test_find_path_direct() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    knowledge_edges::create_edge(&pool, pid, "A", "B", "uses", None, None)
        .await
        .unwrap();

    let path = knowledge_edges::find_path(&pool, pid, "A", "B", 5)
        .await
        .unwrap();
    assert_eq!(path.len(), 1);
    assert_eq!(path[0].source_entity, "A");
    assert_eq!(path[0].target_entity, "B");
}

#[tokio::test]
async fn test_find_path_multi_hop() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    knowledge_edges::create_edge(&pool, pid, "A", "B", "uses", None, None)
        .await
        .unwrap();
    knowledge_edges::create_edge(&pool, pid, "B", "C", "uses", None, None)
        .await
        .unwrap();
    knowledge_edges::create_edge(&pool, pid, "C", "D", "uses", None, None)
        .await
        .unwrap();

    let path = knowledge_edges::find_path(&pool, pid, "A", "D", 5)
        .await
        .unwrap();
    assert_eq!(path.len(), 3);
}

#[tokio::test]
async fn test_find_path_no_path() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    knowledge_edges::create_edge(&pool, pid, "A", "B", "uses", None, None)
        .await
        .unwrap();
    knowledge_edges::create_edge(&pool, pid, "C", "D", "uses", None, None)
        .await
        .unwrap();

    let path = knowledge_edges::find_path(&pool, pid, "A", "D", 5)
        .await
        .unwrap();
    assert!(path.is_empty());
}

#[tokio::test]
async fn test_find_path_same_entity() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let path = knowledge_edges::find_path(&pool, pid, "A", "A", 5)
        .await
        .unwrap();
    assert!(path.is_empty());
}

#[tokio::test]
async fn test_unique_constraint() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    knowledge_edges::create_edge(&pool, pid, "A", "B", "uses", None, None)
        .await
        .unwrap();
    let result = knowledge_edges::create_edge(&pool, pid, "A", "B", "uses", None, None).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_cascade_on_project_delete() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    knowledge_edges::create_edge(&pool, pid, "A", "B", "uses", None, None)
        .await
        .unwrap();

    sqlx::query("DELETE FROM ai_memory.projects WHERE id = $1")
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();

    let edges = knowledge_edges::get_neighbors(&pool, pid, "A", None)
        .await
        .unwrap();
    assert!(edges.is_empty());
}
