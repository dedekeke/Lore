mod common;

use lore::db::projects;

#[tokio::test]
async fn test_create_and_get_project() {
    let (pool, _container) = common::setup_db().await;

    let id = projects::create_project(&pool, "test-proj", "/tmp/test")
        .await
        .unwrap();
    let project = projects::get_project(&pool, id).await.unwrap().unwrap();

    assert_eq!(project.name, "test-proj");
    assert_eq!(project.root_path, "/tmp/test");
}

#[tokio::test]
async fn test_get_project_by_name() {
    let (pool, _container) = common::setup_db().await;

    projects::create_project(&pool, "named-proj", "/tmp")
        .await
        .unwrap();
    let project = projects::get_project_by_name(&pool, "named-proj")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(project.name, "named-proj");
}

#[tokio::test]
async fn test_get_or_create_idempotent() {
    let (pool, _container) = common::setup_db().await;

    let id1 = projects::get_or_create_project(&pool, "idem", "/a")
        .await
        .unwrap();
    let id2 = projects::get_or_create_project(&pool, "idem", "/b")
        .await
        .unwrap();

    assert_eq!(id1, id2);
}

#[tokio::test]
async fn test_list_projects() {
    let (pool, _container) = common::setup_db().await;

    projects::create_project(&pool, "p1", "/a").await.unwrap();
    projects::create_project(&pool, "p2", "/b").await.unwrap();

    let list = projects::list_projects(&pool).await.unwrap();
    assert_eq!(list.len(), 2);
}

#[tokio::test]
async fn test_delete_project() {
    let (pool, _container) = common::setup_db().await;

    let id = projects::create_project(&pool, "doomed", "/tmp")
        .await
        .unwrap();
    assert!(projects::delete_project(&pool, id).await.unwrap());
    assert!(projects::get_project(&pool, id).await.unwrap().is_none());
}

#[tokio::test]
async fn test_delete_nonexistent() {
    let (pool, _container) = common::setup_db().await;

    assert!(!projects::delete_project(&pool, uuid::Uuid::new_v4())
        .await
        .unwrap());
}
