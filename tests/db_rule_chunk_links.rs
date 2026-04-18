mod common;

use lore::db::{codebase, projects, rule_chunk_links, semantic};

#[tokio::test]
async fn test_create_and_get_link() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let emb = vec![0.5_f32; 384];
    let rule_id = semantic::create_rule(
        &pool,
        pid,
        semantic::RuleCategory::Lesson,
        "use async",
        Some(&emb),
        &[],
    )
    .await
    .unwrap();

    let chunk_id = insert_test_chunk(&pool, pid, "src/main.rs", &emb).await;

    let link_id = rule_chunk_links::create_link(&pool, rule_id, chunk_id, 0.92)
        .await
        .unwrap();
    assert_ne!(link_id, uuid::Uuid::nil());

    let rules = rule_chunk_links::get_rules_for_chunk(&pool, chunk_id)
        .await
        .unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].rule_id, rule_id);
    assert!((rules[0].similarity - 0.92).abs() < 0.001);
}

#[tokio::test]
async fn test_unique_constraint_upserts() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let emb = vec![0.5_f32; 384];
    let rule_id = semantic::create_rule(
        &pool,
        pid,
        semantic::RuleCategory::Fact,
        "fact",
        Some(&emb),
        &[],
    )
    .await
    .unwrap();
    let chunk_id = insert_test_chunk(&pool, pid, "src/lib.rs", &emb).await;

    rule_chunk_links::create_link(&pool, rule_id, chunk_id, 0.85)
        .await
        .unwrap();
    // Upsert with higher similarity
    rule_chunk_links::create_link(&pool, rule_id, chunk_id, 0.95)
        .await
        .unwrap();

    let rules = rule_chunk_links::get_rules_for_chunk(&pool, chunk_id)
        .await
        .unwrap();
    assert_eq!(rules.len(), 1);
    assert!((rules[0].similarity - 0.95).abs() < 0.001);
}

#[tokio::test]
async fn test_get_rules_for_file() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let emb = vec![0.5_f32; 384];
    let rule_id = semantic::create_rule(
        &pool,
        pid,
        semantic::RuleCategory::Constraint,
        "no unwrap",
        Some(&emb),
        &[],
    )
    .await
    .unwrap();
    let chunk_id = insert_test_chunk(&pool, pid, "src/server.rs", &emb).await;

    rule_chunk_links::create_link(&pool, rule_id, chunk_id, 0.88)
        .await
        .unwrap();

    let rules = rule_chunk_links::get_rules_for_file(&pool, "src/server.rs", pid)
        .await
        .unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].content, "no unwrap");
    assert_eq!(rules[0].file_path, "src/server.rs");

    // Different file returns empty
    let empty = rule_chunk_links::get_rules_for_file(&pool, "src/other.rs", pid)
        .await
        .unwrap();
    assert!(empty.is_empty());
}

#[tokio::test]
async fn test_cascade_delete_rule() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let emb = vec![0.5_f32; 384];
    let rule_id = semantic::create_rule(
        &pool,
        pid,
        semantic::RuleCategory::Lesson,
        "lesson",
        Some(&emb),
        &[],
    )
    .await
    .unwrap();
    let chunk_id = insert_test_chunk(&pool, pid, "src/a.rs", &emb).await;

    rule_chunk_links::create_link(&pool, rule_id, chunk_id, 0.90)
        .await
        .unwrap();

    // Deleting the rule should cascade-delete the link
    semantic::delete_rule(&pool, rule_id).await.unwrap();
    let rules = rule_chunk_links::get_rules_for_chunk(&pool, chunk_id)
        .await
        .unwrap();
    assert!(rules.is_empty());
}

#[tokio::test]
async fn test_cascade_delete_chunk() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let emb = vec![0.5_f32; 384];
    let rule_id = semantic::create_rule(
        &pool,
        pid,
        semantic::RuleCategory::Lesson,
        "lesson",
        Some(&emb),
        &[],
    )
    .await
    .unwrap();
    let chunk_id = insert_test_chunk(&pool, pid, "src/b.rs", &emb).await;

    rule_chunk_links::create_link(&pool, rule_id, chunk_id, 0.90)
        .await
        .unwrap();

    // Deleting the chunk should cascade-delete the link
    codebase::delete_file_chunks(&pool, pid, "src/b.rs")
        .await
        .unwrap();
    let rules = rule_chunk_links::get_rules_for_chunk(&pool, chunk_id)
        .await
        .unwrap();
    assert!(rules.is_empty());
}

async fn insert_test_chunk(
    pool: &sqlx::PgPool,
    project_id: uuid::Uuid,
    file_path: &str,
    emb: &[f32],
) -> uuid::Uuid {
    let chunk = codebase::NewCodeChunk {
        file_path: file_path.to_string(),
        start_line: 1,
        end_line: 10,
        language: Some("rust".to_string()),
        content: "fn main() {}".to_string(),
        embedding: Some(emb.to_vec()),
        file_hash: "abc123".to_string(),
        behavior_version: 1,
    };
    codebase::insert_chunks(pool, project_id, &[chunk])
        .await
        .unwrap();

    // Retrieve the chunk ID
    let row: (uuid::Uuid,) = sqlx::query_as(
        "SELECT id FROM ai_memory.code_chunks WHERE project_id = $1 AND file_path = $2 LIMIT 1",
    )
    .bind(project_id)
    .bind(file_path)
    .fetch_one(pool)
    .await
    .unwrap();
    row.0
}
