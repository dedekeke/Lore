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
        &[],
    )
    .await
    .unwrap();

    let rule = semantic::get_rule(&pool, id).await.unwrap().unwrap();
    assert_eq!(rule.content, "the sky is blue");
    assert_eq!(rule.category, RuleCategory::Fact);
    assert!(rule.tags.is_empty());
}

#[tokio::test]
async fn test_list_rules_by_category() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    semantic::create_rule(&pool, pid, RuleCategory::Fact, "fact1", None, &[])
        .await
        .unwrap();
    semantic::create_rule(&pool, pid, RuleCategory::Preference, "pref1", None, &[])
        .await
        .unwrap();

    let facts = semantic::list_rules(&pool, pid, Some(RuleCategory::Fact), None)
        .await
        .unwrap();
    let all = semantic::list_rules(&pool, pid, None, None).await.unwrap();

    assert_eq!(facts.len(), 1);
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn test_delete_rule() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let id = semantic::create_rule(&pool, pid, RuleCategory::Constraint, "no nulls", None, &[])
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
    semantic::create_rule(
        &pool,
        pid,
        RuleCategory::Lesson,
        "lesson A",
        Some(&emb_a),
        &[],
    )
    .await
    .unwrap();
    semantic::create_rule(
        &pool,
        pid,
        RuleCategory::Lesson,
        "lesson B",
        Some(&emb_b),
        &[],
    )
    .await
    .unwrap();

    let query = vec![0.1_f32; 384];
    let results = semantic::search_rules_by_embedding(&pool, pid, &query, 10, None, None)
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
    semantic::create_rule(&pool, pid, RuleCategory::Fact, "fact", Some(&emb), &[])
        .await
        .unwrap();
    semantic::create_rule(&pool, pid, RuleCategory::Lesson, "lesson", Some(&emb), &[])
        .await
        .unwrap();

    let results =
        semantic::search_rules_by_embedding(&pool, pid, &emb, 10, Some(RuleCategory::Lesson), None)
            .await
            .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].content, "lesson");
}

#[tokio::test]
async fn test_create_rule_persists_tags() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let tags = vec!["rust".to_string(), "async".to_string()];
    let id = semantic::create_rule(&pool, pid, RuleCategory::Fact, "rule", None, &tags)
        .await
        .unwrap();

    let rule = semantic::get_rule(&pool, id).await.unwrap().unwrap();
    assert_eq!(rule.tags, tags);
}

#[tokio::test]
async fn test_list_rules_filters_by_tags_and_semantics() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let rust_async = vec!["rust".to_string(), "async".to_string()];
    let rust_only = vec!["rust".to_string()];
    let other = vec!["python".to_string()];

    semantic::create_rule(&pool, pid, RuleCategory::Fact, "r1", None, &rust_async)
        .await
        .unwrap();
    semantic::create_rule(&pool, pid, RuleCategory::Fact, "r2", None, &rust_only)
        .await
        .unwrap();
    semantic::create_rule(&pool, pid, RuleCategory::Fact, "r3", None, &other)
        .await
        .unwrap();

    // @> is contains: r1 and r2 both contain ["rust"]
    let rust_filter = vec!["rust".to_string()];
    let rust_matches = semantic::list_rules(&pool, pid, None, Some(&rust_filter))
        .await
        .unwrap();
    assert_eq!(rust_matches.len(), 2);

    // AND semantics: only r1 contains both rust + async
    let both = vec!["rust".to_string(), "async".to_string()];
    let and_matches = semantic::list_rules(&pool, pid, None, Some(&both))
        .await
        .unwrap();
    assert_eq!(and_matches.len(), 1);
    assert_eq!(and_matches[0].content, "r1");

    // None returns all
    let all = semantic::list_rules(&pool, pid, None, None).await.unwrap();
    assert_eq!(all.len(), 3);
}

#[tokio::test]
async fn test_search_by_embedding_filters_tags() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let emb = vec![0.5_f32; 384];
    semantic::create_rule(
        &pool,
        pid,
        RuleCategory::Fact,
        "tagged",
        Some(&emb),
        &["t1".to_string()],
    )
    .await
    .unwrap();
    semantic::create_rule(&pool, pid, RuleCategory::Fact, "untagged", Some(&emb), &[])
        .await
        .unwrap();

    let filter = vec!["t1".to_string()];
    let results = semantic::search_rules_by_embedding(&pool, pid, &emb, 10, None, Some(&filter))
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].content, "tagged");
}

#[tokio::test]
async fn test_search_hybrid_filters_tags() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let emb = vec![0.5_f32; 384];
    semantic::create_rule(
        &pool,
        pid,
        RuleCategory::Fact,
        "shared content",
        Some(&emb),
        &["keep".to_string()],
    )
    .await
    .unwrap();
    semantic::create_rule(
        &pool,
        pid,
        RuleCategory::Fact,
        "shared content",
        Some(&emb),
        &["drop".to_string()],
    )
    .await
    .unwrap();

    let filter = vec!["keep".to_string()];
    let results = semantic::search_rules_hybrid(
        &pool,
        pid,
        &emb,
        "shared content",
        10,
        None,
        None,
        Some(&filter),
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tags, vec!["keep".to_string()]);
}
