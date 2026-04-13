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
        .start_task("implement feature X".into(), None, None, None)
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

#[tokio::test]
async fn test_get_task_stats() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(Some("stats-test".into()), Some("/tmp".into()))
        .await
        .unwrap();

    // Empty stats
    let result = server.get_task_stats(None).await.unwrap();
    let json = extract_json(&result);
    assert_eq!(json["summary"]["total_tasks"], 0);

    // Create task with attempts
    let task = server
        .start_task("stats task".into(), None, None, None)
        .await
        .unwrap();
    let task_id = extract_json(&task)["task_id"].as_str().unwrap().to_string();

    let attempt = server
        .propose_attempt(task_id.clone(), "approach A".into(), None)
        .await
        .unwrap();
    let attempt_id = extract_json(&attempt)["attempt_id"]
        .as_str()
        .unwrap()
        .to_string();

    server
        .log_outcome(attempt_id, "rejected".into(), "nope".into(), None, None)
        .await
        .unwrap();

    let result = server.get_task_stats(None).await.unwrap();
    let json = extract_json(&result);
    assert_eq!(json["summary"]["total_tasks"], 1);
    assert_eq!(json["summary"]["total_rejected"], 1);

    // Complete and verify resolution_minutes appears
    server
        .complete_task(task_id.clone(), None, None)
        .await
        .unwrap();

    let result = server
        .get_task_stats(Some("completed".into()))
        .await
        .unwrap();
    let json = extract_json(&result);
    assert_eq!(json["tasks"].as_array().unwrap().len(), 1);
    assert!(json["tasks"][0]["resolution_minutes"].is_number());
}

#[tokio::test]
async fn test_abandon_task() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(Some("abandon-test".into()), Some("/tmp".into()))
        .await
        .unwrap();

    let task = server
        .start_task("abandon me".into(), None, None, None)
        .await
        .unwrap();
    let task_id = extract_json(&task)["task_id"].as_str().unwrap().to_string();

    let result = server
        .abandon_task(task_id.clone(), "not needed".into(), Some(true))
        .await
        .unwrap();
    assert!(extract_json(&result)["success"].as_bool().unwrap());

    // Verify task is abandoned via list
    let list = server.list_tasks(Some("abandoned".into())).await.unwrap();
    let tasks = extract_json(&list);
    assert_eq!(tasks.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn test_list_tasks() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(Some("list-test".into()), Some("/tmp".into()))
        .await
        .unwrap();

    server
        .start_task("task A".into(), None, None, None)
        .await
        .unwrap();
    server
        .start_task("task B".into(), None, None, None)
        .await
        .unwrap();

    let list = server.list_tasks(None).await.unwrap();
    let tasks = extract_json(&list);
    assert_eq!(tasks.as_array().unwrap().len(), 2);

    let active = server.list_tasks(Some("active".into())).await.unwrap();
    assert_eq!(extract_json(&active).as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_update_rule() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(Some("update-rule-test".into()), Some("/tmp".into()))
        .await
        .unwrap();

    let result = server
        .remember_rule("fact".into(), "original content".into())
        .await
        .unwrap();
    let rule_id = extract_json(&result)["rule_id"]
        .as_str()
        .unwrap()
        .to_string();

    let updated = server
        .update_rule(
            rule_id.clone(),
            Some("lesson".into()),
            Some("updated content".into()),
        )
        .await
        .unwrap();
    assert!(extract_json(&updated)["updated"].as_bool().unwrap());

    let list = server.list_rules(Some("lesson".into())).await.unwrap();
    let rules = extract_json(&list).as_array().unwrap().clone();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["content"], "updated content");
}

#[tokio::test]
async fn test_find_similar_failures() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(Some("failures-test".into()), Some("/tmp".into()))
        .await
        .unwrap();

    // With no data, should return empty
    let result = server
        .find_similar_failures("compilation error".into(), Some(5), None)
        .await
        .unwrap();
    let json = extract_json(&result);
    assert!(json.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_generate_handoff() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(Some("handoff-test".into()), Some("/tmp".into()))
        .await
        .unwrap();

    server
        .start_task("active task".into(), None, None, None)
        .await
        .unwrap();

    let result = server.generate_handoff(Some(5000)).await.unwrap();
    let text = result
        .content
        .iter()
        .find_map(|c| match &c.raw {
            rmcp::model::RawContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .unwrap();
    assert!(text.contains("Handoff Packet"));
    assert!(text.contains("active task"));
}

#[tokio::test]
async fn test_get_next_steps() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(Some("nextsteps-test".into()), Some("/tmp".into()))
        .await
        .unwrap();

    let result = server.get_next_steps(None).await.unwrap();
    let json = extract_json(&result);
    assert!(json["project"].is_object());
    assert!(json["tasks"].is_array());
}

#[tokio::test]
async fn test_get_next_steps_l0_tier() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(Some("l0-test".into()), Some("/tmp".into()))
        .await
        .unwrap();
    server
        .start_task("active one".into(), None, None, None)
        .await
        .unwrap();

    let result = server.get_next_steps(Some("L0".into())).await.unwrap();
    let json = extract_json(&result);

    // L0 returns counts only — no per-task payload
    assert!(json["project"].is_object());
    assert_eq!(json["active_task_count"].as_i64().unwrap(), 1);
    assert_eq!(json["blocked_task_count"].as_i64().unwrap(), 0);
    assert_eq!(json["lesson_count"].as_i64().unwrap(), 0);
    assert!(json.get("active_tasks").is_none());
    assert!(json.get("recent_attempts").is_none());
}

#[tokio::test]
async fn test_get_next_steps_l0_case_insensitive() {
    let (server, _pool, _c) = setup_server().await;
    server
        .switch_project(Some("l0-ci".into()), Some("/tmp".into()))
        .await
        .unwrap();

    let result = server.get_next_steps(Some("l0".into())).await.unwrap();
    let json = extract_json(&result);
    assert!(json["active_task_count"].is_number());
}

#[tokio::test]
async fn test_log_context_wipe() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(Some("wipe-test".into()), Some("/tmp".into()))
        .await
        .unwrap();

    let task = server
        .start_task("wipe task".into(), None, None, None)
        .await
        .unwrap();
    let task_id = extract_json(&task)["task_id"].as_str().unwrap().to_string();

    let result = server.log_context_wipe(task_id, 10000, None).await.unwrap();
    let json = extract_json(&result);
    assert!(json["snapshot_id"].as_str().is_some());
}

#[tokio::test]
async fn test_get_protocol() {
    let (server, _pool, _c) = setup_server().await;

    let result = server.get_protocol().await.unwrap();
    let text = result
        .content
        .iter()
        .find_map(|c| match &c.raw {
            rmcp::model::RawContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .unwrap();
    assert!(text.contains("CRITICAL OPERATING PROTOCOL"));
}
