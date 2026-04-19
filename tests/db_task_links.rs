mod common;

use lore::db::{projects, task_links, tasks};

#[tokio::test]
async fn test_create_and_get_links() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let t1 = tasks::create_task(&pool, pid, "task 1", None, None, None, None)
        .await
        .unwrap();
    let t2 = tasks::create_task(&pool, pid, "task 2", None, None, None, None)
        .await
        .unwrap();

    let link_id = task_links::create_link(&pool, t1, t2, "blocks")
        .await
        .unwrap();

    let links = task_links::get_links_for_task(&pool, t1).await.unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].id, link_id);
    assert_eq!(links[0].link_type, "blocks");

    // Also visible from target side
    let target_links = task_links::get_links_for_task(&pool, t2).await.unwrap();
    assert_eq!(target_links.len(), 1);
}

#[tokio::test]
async fn test_delete_link() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let t1 = tasks::create_task(&pool, pid, "a", None, None, None, None)
        .await
        .unwrap();
    let t2 = tasks::create_task(&pool, pid, "b", None, None, None, None)
        .await
        .unwrap();

    let link_id = task_links::create_link(&pool, t1, t2, "related_to")
        .await
        .unwrap();

    assert!(task_links::delete_link(&pool, link_id).await.unwrap());
    let links = task_links::get_links_for_task(&pool, t1).await.unwrap();
    assert!(links.is_empty());
}

#[tokio::test]
async fn test_cascade_on_task_delete() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let t1 = tasks::create_task(&pool, pid, "a", None, None, None, None)
        .await
        .unwrap();
    let t2 = tasks::create_task(&pool, pid, "b", None, None, None, None)
        .await
        .unwrap();

    task_links::create_link(&pool, t1, t2, "caused_by")
        .await
        .unwrap();

    // Delete source task — link should cascade
    sqlx::query("DELETE FROM ai_memory.tasks WHERE id = $1")
        .bind(t1)
        .execute(&pool)
        .await
        .unwrap();

    let links = task_links::get_links_for_task(&pool, t2).await.unwrap();
    assert!(links.is_empty());
}

#[tokio::test]
async fn test_duplicate_link_rejected() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let t1 = tasks::create_task(&pool, pid, "a", None, None, None, None)
        .await
        .unwrap();
    let t2 = tasks::create_task(&pool, pid, "b", None, None, None, None)
        .await
        .unwrap();

    task_links::create_link(&pool, t1, t2, "blocks")
        .await
        .unwrap();

    // Duplicate insert should fail (ON CONFLICT DO NOTHING + RETURNING = no rows)
    let result = task_links::create_link(&pool, t1, t2, "blocks").await;
    assert!(result.is_err());
}
