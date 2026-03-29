mod common;

use lore::config::Config;
use lore::embeddings::fake::FakeEmbeddingProvider;
use lore::embeddings::AnyEmbeddingProvider;
use lore::server::LoreServer;
use rmcp::model::RawContent;

async fn setup_server() -> (
    LoreServer,
    sqlx::PgPool,
    testcontainers::ContainerAsync<testcontainers::GenericImage>,
) {
    let (pool, container) = common::setup_db().await;
    let config = Config::from_env();
    let embeddings = AnyEmbeddingProvider::Fake(FakeEmbeddingProvider::new(384));
    let server = LoreServer::new(pool.clone(), embeddings, config);
    (server, pool, container)
}

fn extract_json(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    let text = result
        .content
        .iter()
        .find_map(|c| match &c.raw {
            RawContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .expect("No text content in result");
    serde_json::from_str(text).expect("Invalid JSON in result")
}

#[tokio::test]
async fn test_switch_project_and_get_context() {
    let (server, _pool, _c) = setup_server().await;

    let result = server
        .switch_project(Some("integration-test".into()), Some("/tmp".into()))
        .await
        .unwrap();

    let json = extract_json(&result);
    assert_eq!(json["project_name"], "integration-test");
    assert!(json["project_id"].as_str().is_some());

    let ctx = server.get_active_context().await.unwrap();
    let ctx_json = extract_json(&ctx);
    assert_eq!(ctx_json["active_tasks"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_full_task_lifecycle() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(Some("lifecycle".into()), Some("/tmp".into()))
        .await
        .unwrap();

    let task_result = server
        .start_task("implement feature X".into(), None)
        .await
        .unwrap();
    let task_id = extract_json(&task_result)["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    let attempt_result = server
        .propose_attempt(task_id.clone(), "try approach A".into(), None)
        .await
        .unwrap();
    let attempt_id = extract_json(&attempt_result)["attempt_id"]
        .as_str()
        .unwrap()
        .to_string();

    let outcome = server
        .log_outcome(
            attempt_id,
            "rejected".into(),
            "didn't compile".into(),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(extract_json(&outcome)["success"].as_bool().unwrap());

    let ledger = server.review_ledger(task_id.clone(), None).await.unwrap();
    assert_eq!(extract_json(&ledger).as_array().unwrap().len(), 1);

    let complete = server
        .complete_task(task_id, Some("always check compilation first".into()), None)
        .await
        .unwrap();
    assert!(extract_json(&complete)["success"].as_bool().unwrap());
}

#[tokio::test]
async fn test_remember_and_recall_rules() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(Some("rules-test".into()), Some("/tmp".into()))
        .await
        .unwrap();

    server
        .remember_rule("fact".into(), "Rust is a systems language".into())
        .await
        .unwrap();

    let recalled = server
        .recall_rules("systems language".into(), Some(10), None)
        .await
        .unwrap();
    let rules = extract_json(&recalled);
    assert!(!rules.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_forget_rule() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(Some("forget-test".into()), Some("/tmp".into()))
        .await
        .unwrap();

    let result = server
        .remember_rule("preference".into(), "use tabs".into())
        .await
        .unwrap();
    let rule_id = extract_json(&result)["rule_id"]
        .as_str()
        .unwrap()
        .to_string();

    server.forget_rule(rule_id).await.unwrap();

    let list = server.list_rules(None).await.unwrap();
    assert!(extract_json(&list).as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_export_memory() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(Some("export-test".into()), Some("/tmp".into()))
        .await
        .unwrap();
    server
        .remember_rule("fact".into(), "test fact".into())
        .await
        .unwrap();

    let export = server.export_memory("json".into()).await.unwrap();
    let json = extract_json(&export);

    assert!(!json["rules"].as_array().unwrap().is_empty());
    assert!(json["tasks"].is_array());
    assert!(json["attempts"].is_array());
}
