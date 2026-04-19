mod common;

use lore::db::{projects, scratchpad, tasks};

async fn make_project(pool: &sqlx::PgPool, name: &str, path: &str) -> uuid::Uuid {
    projects::create_project(pool, name, path).await.unwrap()
}

#[tokio::test]
async fn test_write_and_read_project_scope() {
    let (pool, _container) = common::setup_db().await;
    let pid = make_project(&pool, "scratch-p1", "/tmp/scratch-p1").await;

    let e = scratchpad::write_scratch(&pool, pid, None, "current_focus", "refactor auth", None)
        .await
        .unwrap();
    assert_eq!(e.value, "refactor auth");
    assert!(e.expires_at.is_none());

    let got = scratchpad::read_scratch(&pool, pid, None, "current_focus")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.id, e.id);
    assert_eq!(got.value, "refactor auth");
}

#[tokio::test]
async fn test_upsert_updates_value() {
    let (pool, _container) = common::setup_db().await;
    let pid = make_project(&pool, "scratch-p2", "/tmp/scratch-p2").await;

    let first = scratchpad::write_scratch(&pool, pid, None, "k", "v1", None)
        .await
        .unwrap();
    let second = scratchpad::write_scratch(&pool, pid, None, "k", "v2", None)
        .await
        .unwrap();

    assert_eq!(first.id, second.id, "upsert must reuse the row");
    assert_eq!(second.value, "v2");
    assert!(second.updated_at >= first.updated_at);
}

#[tokio::test]
async fn test_task_scope_distinct_from_project() {
    let (pool, _container) = common::setup_db().await;
    let pid = make_project(&pool, "scratch-p3", "/tmp/scratch-p3").await;
    let tid = tasks::create_task(&pool, pid, "a task", None, None, None, None)
        .await
        .unwrap();

    scratchpad::write_scratch(&pool, pid, None, "note", "proj-note", None)
        .await
        .unwrap();
    scratchpad::write_scratch(&pool, pid, Some(tid), "note", "task-note", None)
        .await
        .unwrap();

    let proj = scratchpad::read_scratch(&pool, pid, None, "note")
        .await
        .unwrap()
        .unwrap();
    let task = scratchpad::read_scratch(&pool, pid, Some(tid), "note")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(proj.value, "proj-note");
    assert_eq!(task.value, "task-note");
    assert_ne!(proj.id, task.id);
}

#[tokio::test]
async fn test_ttl_expired_filtered() {
    let (pool, _container) = common::setup_db().await;
    let pid = make_project(&pool, "scratch-p4", "/tmp/scratch-p4").await;

    // Write with negative-offset expires_at via direct SQL to simulate past expiry.
    scratchpad::write_scratch(&pool, pid, None, "tmp", "soon-expired", Some(1))
        .await
        .unwrap();
    sqlx::query("UPDATE ai_memory.scratchpad SET expires_at = NOW() - INTERVAL '1 second' WHERE project_id = $1 AND key = 'tmp'")
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();

    let got = scratchpad::read_scratch(&pool, pid, None, "tmp")
        .await
        .unwrap();
    assert!(got.is_none(), "expired entries must be filtered from reads");

    let listed = scratchpad::list_scratch(&pool, pid, None, 10)
        .await
        .unwrap();
    assert!(listed.iter().all(|e| e.key != "tmp"));
}

#[tokio::test]
async fn test_list_newest_first_and_limit() {
    let (pool, _container) = common::setup_db().await;
    let pid = make_project(&pool, "scratch-p5", "/tmp/scratch-p5").await;

    for i in 0..5 {
        scratchpad::write_scratch(&pool, pid, None, &format!("k{i}"), &format!("v{i}"), None)
            .await
            .unwrap();
    }

    let all = scratchpad::list_scratch(&pool, pid, None, 10)
        .await
        .unwrap();
    assert_eq!(all.len(), 5);
    // Newest (k4) first.
    assert_eq!(all[0].key, "k4");
    assert_eq!(all[4].key, "k0");

    let limited = scratchpad::list_scratch(&pool, pid, None, 2).await.unwrap();
    assert_eq!(limited.len(), 2);
    assert_eq!(limited[0].key, "k4");
}

#[tokio::test]
async fn test_delete_scratch() {
    let (pool, _container) = common::setup_db().await;
    let pid = make_project(&pool, "scratch-p6", "/tmp/scratch-p6").await;

    scratchpad::write_scratch(&pool, pid, None, "kill", "bye", None)
        .await
        .unwrap();
    assert!(scratchpad::delete_scratch(&pool, pid, None, "kill")
        .await
        .unwrap());
    assert!(!scratchpad::delete_scratch(&pool, pid, None, "kill")
        .await
        .unwrap());
    assert!(scratchpad::read_scratch(&pool, pid, None, "kill")
        .await
        .unwrap()
        .is_none());
}
