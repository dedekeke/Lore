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
    assert!(
        !rule.is_always_injected,
        "is_always_injected must default to false"
    );
}

#[tokio::test]
async fn test_create_instruction_rule() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let id = semantic::create_rule(
        &pool,
        pid,
        RuleCategory::Instruction,
        "always prefer explicit error handling",
        None,
        &[],
    )
    .await
    .unwrap();

    let rule = semantic::get_rule(&pool, id).await.unwrap().unwrap();
    assert_eq!(rule.category, RuleCategory::Instruction);
    assert!(!rule.is_always_injected);

    let listed = semantic::list_rules(&pool, pid, Some(RuleCategory::Instruction), None)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
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
async fn test_count_rules_by_category() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    semantic::create_rule(&pool, pid, RuleCategory::Lesson, "l1", None, &[])
        .await
        .unwrap();
    semantic::create_rule(&pool, pid, RuleCategory::Lesson, "l2", None, &[])
        .await
        .unwrap();
    semantic::create_rule(&pool, pid, RuleCategory::Fact, "f1", None, &[])
        .await
        .unwrap();

    let lessons = semantic::count_rules_by_category(&pool, pid, RuleCategory::Lesson)
        .await
        .unwrap();
    let prefs = semantic::count_rules_by_category(&pool, pid, RuleCategory::Preference)
        .await
        .unwrap();
    assert_eq!(lessons, 2);
    assert_eq!(prefs, 0);
}

#[tokio::test]
async fn test_count_rules_by_category_scopes_to_project() {
    let (pool, _c) = common::setup_db().await;
    let pa = projects::create_project(&pool, "a", "/a").await.unwrap();
    let pb = projects::create_project(&pool, "b", "/b").await.unwrap();

    semantic::create_rule(&pool, pa, RuleCategory::Lesson, "la", None, &[])
        .await
        .unwrap();
    semantic::create_rule(&pool, pb, RuleCategory::Lesson, "lb", None, &[])
        .await
        .unwrap();

    let a = semantic::count_rules_by_category(&pool, pa, RuleCategory::Lesson)
        .await
        .unwrap();
    assert_eq!(a, 1);
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
async fn test_supersede_rule_hides_from_list() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let emb = vec![0.5_f32; 384];
    let id = semantic::create_rule(&pool, pid, RuleCategory::Fact, "old fact", Some(&emb), &[])
        .await
        .unwrap();

    // Before supersede: visible in list and search
    let all = semantic::list_rules(&pool, pid, None, None).await.unwrap();
    assert_eq!(all.len(), 1);

    let results = semantic::search_rules_by_embedding(&pool, Some(pid), &emb, 10, None, None, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);

    // Supersede
    assert!(semantic::supersede_rule(&pool, id).await.unwrap());

    // After supersede: hidden from list and search
    let all = semantic::list_rules(&pool, pid, None, None).await.unwrap();
    assert_eq!(all.len(), 0);

    let results = semantic::search_rules_by_embedding(&pool, Some(pid), &emb, 10, None, None, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 0);

    // Rule still exists in DB (get_rule returns it)
    let rule = semantic::get_rule(&pool, id).await.unwrap().unwrap();
    assert!(rule.valid_until.is_some());

    // Superseding again is a no-op
    assert!(!semantic::supersede_rule(&pool, id).await.unwrap());
}

#[tokio::test]
async fn test_supersede_excludes_from_count() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let id = semantic::create_rule(&pool, pid, RuleCategory::Fact, "f1", None, &[])
        .await
        .unwrap();
    semantic::create_rule(&pool, pid, RuleCategory::Fact, "f2", None, &[])
        .await
        .unwrap();

    assert_eq!(semantic::count_rules(&pool, pid).await.unwrap(), 2);
    assert_eq!(
        semantic::count_rules_by_category(&pool, pid, RuleCategory::Fact)
            .await
            .unwrap(),
        2
    );

    semantic::supersede_rule(&pool, id).await.unwrap();

    assert_eq!(semantic::count_rules(&pool, pid).await.unwrap(), 1);
    assert_eq!(
        semantic::count_rules_by_category(&pool, pid, RuleCategory::Fact)
            .await
            .unwrap(),
        1
    );
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
    let results =
        semantic::search_rules_by_embedding(&pool, Some(pid), &query, 10, None, None, None)
            .await
            .unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].rule.content, "lesson A");
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

    let results = semantic::search_rules_by_embedding(
        &pool,
        Some(pid),
        &emb,
        10,
        Some(RuleCategory::Lesson),
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].rule.content, "lesson");
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

    let rust_filter = vec!["rust".to_string()];
    let rust_matches = semantic::list_rules(&pool, pid, None, Some(&rust_filter))
        .await
        .unwrap();
    assert_eq!(rust_matches.len(), 2);

    let both = vec!["rust".to_string(), "async".to_string()];
    let and_matches = semantic::list_rules(&pool, pid, None, Some(&both))
        .await
        .unwrap();
    assert_eq!(and_matches.len(), 1);
    assert_eq!(and_matches[0].content, "r1");

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
    let results =
        semantic::search_rules_by_embedding(&pool, Some(pid), &emb, 10, None, Some(&filter), None)
            .await
            .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].rule.content, "tagged");
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
        Some(pid),
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
    assert_eq!(results[0].rule.tags, vec!["keep".to_string()]);
}

#[tokio::test]
async fn test_search_cross_project_none_returns_all_projects() {
    let (pool, _c) = common::setup_db().await;
    let pa = projects::create_project(&pool, "a", "/a").await.unwrap();
    let pb = projects::create_project(&pool, "b", "/b").await.unwrap();

    let emb = vec![0.5_f32; 384];
    semantic::create_rule(&pool, pa, RuleCategory::Fact, "from a", Some(&emb), &[])
        .await
        .unwrap();
    semantic::create_rule(&pool, pb, RuleCategory::Fact, "from b", Some(&emb), &[])
        .await
        .unwrap();

    // project_id = None -> search all projects
    let all = semantic::search_rules_by_embedding(&pool, None, &emb, 10, None, None, None)
        .await
        .unwrap();
    assert_eq!(all.len(), 2);

    // project_id = Some(pa) -> only pa
    let a_only = semantic::search_rules_by_embedding(&pool, Some(pa), &emb, 10, None, None, None)
        .await
        .unwrap();
    assert_eq!(a_only.len(), 1);
    assert_eq!(a_only[0].rule.content, "from a");
}

#[tokio::test]
async fn test_search_populates_project_name() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "named-proj", "/x")
        .await
        .unwrap();
    let emb = vec![0.1_f32; 384];
    semantic::create_rule(&pool, pid, RuleCategory::Fact, "x", Some(&emb), &[])
        .await
        .unwrap();

    let results = semantic::search_rules_by_embedding(&pool, None, &emb, 10, None, None, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].rule.project_name.as_deref(), Some("named-proj"));
}

#[tokio::test]
async fn test_search_hybrid_current_project_boost() {
    let (pool, _c) = common::setup_db().await;
    let pa = projects::create_project(&pool, "a", "/a").await.unwrap();
    let pb = projects::create_project(&pool, "b", "/b").await.unwrap();

    let emb = vec![0.5_f32; 384];
    semantic::create_rule(
        &pool,
        pa,
        RuleCategory::Fact,
        "shared fact",
        Some(&emb),
        &[],
    )
    .await
    .unwrap();
    semantic::create_rule(
        &pool,
        pb,
        RuleCategory::Fact,
        "shared fact",
        Some(&emb),
        &[],
    )
    .await
    .unwrap();

    // current_project_id = pb -> pb rule ranks first via 2x weight boost
    let results =
        semantic::search_rules_hybrid(&pool, None, pb, &emb, "shared fact", 10, None, None, None)
            .await
            .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].rule.project_id, pb);
}

#[tokio::test]
async fn test_find_duplicate_clusters() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    // Two near-identical embeddings (high cosine similarity)
    let emb_a = vec![0.5_f32; 384];
    let mut emb_b = vec![0.5_f32; 384];
    emb_b[0] = 0.51;
    // Orthogonal vector: first half positive, second half negative
    let mut emb_c = vec![1.0_f32; 384];
    for item in emb_c.iter_mut().take(384).skip(192) {
        *item = -1.0;
    }

    semantic::create_rule(&pool, pid, RuleCategory::Fact, "rule A", Some(&emb_a), &[])
        .await
        .unwrap();
    semantic::create_rule(&pool, pid, RuleCategory::Fact, "rule B", Some(&emb_b), &[])
        .await
        .unwrap();
    semantic::create_rule(&pool, pid, RuleCategory::Fact, "rule C", Some(&emb_c), &[])
        .await
        .unwrap();

    let pairs = semantic::find_duplicate_clusters(&pool, pid, 10)
        .await
        .unwrap();
    // A and B should be a duplicate pair, C should not match
    assert_eq!(pairs.len(), 1);
    assert!(pairs[0].similarity >= 0.88);
}

async fn flag_always_inject(pool: &sqlx::PgPool, id: uuid::Uuid) {
    sqlx::query("UPDATE ai_memory.semantic_rules SET is_always_injected = true WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_list_always_injected_filters_flag_and_scope() {
    let (pool, _c) = common::setup_db().await;
    let pa = projects::create_project(&pool, "a", "/a").await.unwrap();
    let pb = projects::create_project(&pool, "b", "/b").await.unwrap();

    let flagged_a =
        semantic::create_rule(&pool, pa, RuleCategory::Instruction, "flag me", None, &[])
            .await
            .unwrap();
    let _unflagged_a =
        semantic::create_rule(&pool, pa, RuleCategory::Instruction, "skip me", None, &[])
            .await
            .unwrap();
    let flagged_b = semantic::create_rule(
        &pool,
        pb,
        RuleCategory::Instruction,
        "other proj",
        None,
        &[],
    )
    .await
    .unwrap();
    flag_always_inject(&pool, flagged_a).await;
    flag_always_inject(&pool, flagged_b).await;

    let rows = semantic::list_always_injected_rules(&pool, pa, 20)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, flagged_a);
    assert!(rows[0].is_always_injected);
}

#[tokio::test]
async fn test_list_always_injected_respects_limit() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    for i in 0..5 {
        let id = semantic::create_rule(
            &pool,
            pid,
            RuleCategory::Instruction,
            &format!("rule-{i}"),
            None,
            &[],
        )
        .await
        .unwrap();
        flag_always_inject(&pool, id).await;
    }
    let rows = semantic::list_always_injected_rules(&pool, pid, 3)
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
}

#[tokio::test]
async fn test_list_always_injected_excludes_superseded() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let id = semantic::create_rule(&pool, pid, RuleCategory::Instruction, "live", None, &[])
        .await
        .unwrap();
    flag_always_inject(&pool, id).await;
    assert_eq!(
        semantic::list_always_injected_rules(&pool, pid, 20)
            .await
            .unwrap()
            .len(),
        1
    );
    semantic::supersede_rule(&pool, id).await.unwrap();
    assert_eq!(
        semantic::list_always_injected_rules(&pool, pid, 20)
            .await
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn test_list_always_injected_excludes_future_dated() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let id = semantic::create_rule(&pool, pid, RuleCategory::Instruction, "future", None, &[])
        .await
        .unwrap();
    flag_always_inject(&pool, id).await;
    sqlx::query(
        "UPDATE ai_memory.semantic_rules SET valid_from = NOW() + INTERVAL '1 hour' WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        semantic::list_always_injected_rules(&pool, pid, 20)
            .await
            .unwrap()
            .len(),
        0,
        "future-dated rules (valid_from > NOW()) must not surface"
    );
}

#[tokio::test]
async fn test_list_always_injected_excludes_expired() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let id = semantic::create_rule(&pool, pid, RuleCategory::Instruction, "timed", None, &[])
        .await
        .unwrap();
    flag_always_inject(&pool, id).await;
    sqlx::query(
        "UPDATE ai_memory.semantic_rules SET expires_at = NOW() - INTERVAL '1 hour' WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        semantic::list_always_injected_rules(&pool, pid, 20)
            .await
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn test_create_rule_with_flag_persists_is_always_injected() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let id = semantic::create_rule_with_flag(
        &pool,
        pid,
        RuleCategory::Constraint,
        "no mocks in integration tests",
        None,
        &[],
        true,
    )
    .await
    .unwrap();
    let fetched = semantic::get_rule(&pool, id).await.unwrap().unwrap();
    assert!(fetched.is_always_injected);
}

#[tokio::test]
async fn test_count_always_injected_rules_scope_and_temporal_filters() {
    let (pool, _c) = common::setup_db().await;
    let pa = projects::create_project(&pool, "pa", "/a").await.unwrap();
    let pb = projects::create_project(&pool, "pb", "/b").await.unwrap();

    // 3 flagged in pa (one expired, one future-dated, one live).
    let live = semantic::create_rule_with_flag(
        &pool,
        pa,
        RuleCategory::Instruction,
        "live",
        None,
        &[],
        true,
    )
    .await
    .unwrap();
    let expired = semantic::create_rule_with_flag(
        &pool,
        pa,
        RuleCategory::Instruction,
        "expired",
        None,
        &[],
        true,
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE ai_memory.semantic_rules SET expires_at = NOW() - INTERVAL '1 hour' WHERE id = $1",
    )
    .bind(expired)
    .execute(&pool)
    .await
    .unwrap();
    let future = semantic::create_rule_with_flag(
        &pool,
        pa,
        RuleCategory::Instruction,
        "future",
        None,
        &[],
        true,
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE ai_memory.semantic_rules SET valid_from = NOW() + INTERVAL '1 day' WHERE id = $1",
    )
    .bind(future)
    .execute(&pool)
    .await
    .unwrap();

    // Unflagged in pa (must not count).
    semantic::create_rule_with_flag(&pool, pa, RuleCategory::Fact, "plain", None, &[], false)
        .await
        .unwrap();
    // Flagged in pb (different project — must not count for pa).
    semantic::create_rule_with_flag(
        &pool,
        pb,
        RuleCategory::Instruction,
        "other",
        None,
        &[],
        true,
    )
    .await
    .unwrap();

    assert_eq!(
        semantic::count_always_injected_rules(&pool, pa)
            .await
            .unwrap(),
        1,
        "only the one live flagged rule in pa counts"
    );
    // sanity: the live rule should still be retrievable
    let fetched = semantic::get_rule(&pool, live).await.unwrap().unwrap();
    assert!(fetched.is_always_injected);
}

#[tokio::test]
async fn test_update_rule_toggles_is_always_injected() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let id = semantic::create_rule(&pool, pid, RuleCategory::Fact, "plain", None, &[])
        .await
        .unwrap();
    assert!(
        !semantic::get_rule(&pool, id)
            .await
            .unwrap()
            .unwrap()
            .is_always_injected
    );

    // Turn the flag on; leave every other field untouched.
    let updated = semantic::update_rule(&pool, id, None, None, None, None, Some(true))
        .await
        .unwrap();
    assert!(updated);
    assert!(
        semantic::get_rule(&pool, id)
            .await
            .unwrap()
            .unwrap()
            .is_always_injected
    );

    // None preserves the existing value (no regression when flag is absent).
    semantic::update_rule(&pool, id, None, Some("renamed"), None, None, None)
        .await
        .unwrap();
    let after = semantic::get_rule(&pool, id).await.unwrap().unwrap();
    assert!(after.is_always_injected, "None must not clear the flag");
    assert_eq!(after.content, "renamed");
}
