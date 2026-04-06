mod common;

use lore::db::attempts::AttemptOutcome;
use lore::db::{attempts, projects, tasks};

#[tokio::test]
async fn test_create_and_get_attempt() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let tid = tasks::create_task(&pool, pid, "task", None, None, None).await.unwrap();

    let aid = attempts::create_attempt(&pool, tid, "try X", None, None)
        .await
        .unwrap();
    let attempt = attempts::get_attempt(&pool, aid).await.unwrap().unwrap();

    assert_eq!(attempt.approach_summary, "try X");
    assert_eq!(attempt.outcome, AttemptOutcome::Pending);
}

#[tokio::test]
async fn test_log_outcome() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let tid = tasks::create_task(&pool, pid, "task", None, None, None).await.unwrap();
    let aid = attempts::create_attempt(&pool, tid, "try", None, None)
        .await
        .unwrap();

    let emb = vec![0.5_f32; 384];
    attempts::log_outcome(
        &pool,
        aid,
        AttemptOutcome::Rejected,
        "didn't work",
        Some(&emb),
        Some("abc123"),
        Some("fn main() {}"),
    )
    .await
    .unwrap();

    let attempt = attempts::get_attempt(&pool, aid).await.unwrap().unwrap();
    assert_eq!(attempt.outcome, AttemptOutcome::Rejected);
    assert_eq!(attempt.reasoning, "didn't work");
    assert_eq!(attempt.git_ref.as_deref(), Some("abc123"));
    assert_eq!(attempt.code_snippet.as_deref(), Some("fn main() {}"));
    assert!(attempt.resolved_at.is_some());
}

#[tokio::test]
async fn test_list_attempts_with_filter() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let tid = tasks::create_task(&pool, pid, "task", None, None, None).await.unwrap();

    let a1 = attempts::create_attempt(&pool, tid, "try1", None, None)
        .await
        .unwrap();
    attempts::create_attempt(&pool, tid, "try2", None, None)
        .await
        .unwrap();
    attempts::log_outcome(
        &pool,
        a1,
        AttemptOutcome::Rejected,
        "nope",
        None,
        None,
        None,
    )
    .await
    .unwrap();

    let rejected = attempts::list_attempts(&pool, tid, Some(AttemptOutcome::Rejected))
        .await
        .unwrap();
    let all = attempts::list_attempts(&pool, tid, None).await.unwrap();

    assert_eq!(rejected.len(), 1);
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn test_search_similar_failures() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let tid = tasks::create_task(&pool, pid, "task", None, None, None).await.unwrap();

    let a1 = attempts::create_attempt(&pool, tid, "approach A", None, None)
        .await
        .unwrap();
    let a2 = attempts::create_attempt(&pool, tid, "approach B", None, None)
        .await
        .unwrap();

    let emb1 = vec![0.1_f32; 384];
    let emb2 = vec![0.9_f32; 384];
    attempts::log_outcome(
        &pool,
        a1,
        AttemptOutcome::Rejected,
        "error A",
        Some(&emb1),
        None,
        None,
    )
    .await
    .unwrap();
    attempts::log_outcome(
        &pool,
        a2,
        AttemptOutcome::Rejected,
        "error B",
        Some(&emb2),
        None,
        None,
    )
    .await
    .unwrap();

    // Search with vector close to emb1
    let query = vec![0.1_f32; 384];
    let results = attempts::search_similar_failures(&pool, Some(pid), &query, 5)
        .await
        .unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].approach_summary, "approach A");
}
