mod common;

use lore::db::semantic::RuleCategory;
use lore::db::{projects, semantic};

#[tokio::test]
async fn test_create_and_get_rule() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let emb = vec![0.5_f32; 384];
    let id = semantic::create_rule(
        &pool,
        pid,
        RuleCategory::Fact,
        "the sky is blue",
        Some(&emb),
    )
    .await
    .unwrap();

    let rule = semantic::get_rule(&pool, id).await.unwrap().unwrap();
    assert_eq!(rule.content, "the sky is blue");
    assert_eq!(rule.category, RuleCategory::Fact);
}

#[tokio::test]
async fn test_list_rules_by_category() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    semantic::create_rule(&pool, pid, RuleCategory::Fact, "fact1", None)
        .await
        .unwrap();
    semantic::create_rule(&pool, pid, RuleCategory::Preference, "pref1", None)
        .await
        .unwrap();

    let facts = semantic::list_rules(&pool, pid, Some(RuleCategory::Fact))
        .await
        .unwrap();
    let all = semantic::list_rules(&pool, pid, None).await.unwrap();

    assert_eq!(facts.len(), 1);
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn test_delete_rule() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let id = semantic::create_rule(&pool, pid, RuleCategory::Constraint, "no nulls", None)
        .await
        .unwrap();

    assert!(semantic::delete_rule(&pool, id).await.unwrap());
    assert!(semantic::get_rule(&pool, id).await.unwrap().is_none());
}

#[tokio::test]
async fn test_search_rules_by_embedding() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let emb_a = vec![0.1_f32; 384];
    let emb_b = vec![0.9_f32; 384];
    semantic::create_rule(&pool, pid, RuleCategory::Lesson, "lesson A", Some(&emb_a))
        .await
        .unwrap();
    semantic::create_rule(&pool, pid, RuleCategory::Lesson, "lesson B", Some(&emb_b))
        .await
        .unwrap();

    let query = vec![0.1_f32; 384];
    let results = semantic::search_rules_by_embedding(&pool, pid, &query, 10, None)
        .await
        .unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].content, "lesson A");
}

#[tokio::test]
async fn test_search_rules_with_category_filter() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let emb = vec![0.5_f32; 384];
    semantic::create_rule(&pool, pid, RuleCategory::Fact, "fact", Some(&emb))
        .await
        .unwrap();
    semantic::create_rule(&pool, pid, RuleCategory::Lesson, "lesson", Some(&emb))
        .await
        .unwrap();

    let results =
        semantic::search_rules_by_embedding(&pool, pid, &emb, 10, Some(RuleCategory::Lesson))
            .await
            .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].content, "lesson");
}
