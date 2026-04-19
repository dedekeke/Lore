mod common;

use lore::config::Config;
use lore::embeddings::fake::FakeEmbeddingProvider;
use lore::embeddings::AnyEmbeddingProvider;
use lore::server::{
    AbandonTaskParams, CompleteTaskParams, ExportMemoryParams, FindSimilarFailuresParams,
    ForgetRuleParams, GenerateHandoffParams, GetNextStepsParams, GetTaskStatsParams,
    ListRulesParams, ListTasksParams, LogContextWipeParams, LogOutcomeParams, LoreServer,
    ProposeAttemptParams, RecallRulesParams, RememberRuleParams, ReviewLedgerParams,
    StartTaskParams, SwitchProjectParams, UpdateRuleParams,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::RawContent;

async fn setup_server() -> (
    LoreServer,
    sqlx::PgPool,
    testcontainers::ContainerAsync<testcontainers::GenericImage>,
) {
    setup_server_with(|_| {}).await
}

async fn setup_server_with<F: FnOnce(&mut Config)>(
    tweak: F,
) -> (
    LoreServer,
    sqlx::PgPool,
    testcontainers::ContainerAsync<testcontainers::GenericImage>,
) {
    let (pool, container) = common::setup_db().await;
    let mut config = Config::from_env();
    tweak(&mut config);
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

fn switch_params(name: &str, path: &str) -> Parameters<SwitchProjectParams> {
    Parameters(SwitchProjectParams {
        name: Some(name.into()),
        root_path: Some(path.into()),
    })
}

fn start_task_params(description: &str) -> Parameters<StartTaskParams> {
    Parameters(StartTaskParams {
        description: description.into(),
        parent_task_id: None,
        priority: None,
        task_type: None,
    })
}

#[tokio::test]
async fn test_switch_project_and_get_context() {
    let (server, _pool, _c) = setup_server().await;

    let result = server
        .switch_project(switch_params("integration-test", "/tmp"))
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
        .switch_project(switch_params("lifecycle", "/tmp"))
        .await
        .unwrap();

    let task_result = server
        .start_task(start_task_params("implement feature X"))
        .await
        .unwrap();
    let task_id = extract_json(&task_result)["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    let attempt_result = server
        .propose_attempt_impl(ProposeAttemptParams {
            task_id: task_id.clone(),
            approach_summary: "try approach A".into(),
            agent_id: None,
            request_confirmation: None,
        })
        .await
        .unwrap();
    let attempt_id = extract_json(&attempt_result)["attempt_id"]
        .as_str()
        .unwrap()
        .to_string();

    let outcome = server
        .log_outcome(Parameters(LogOutcomeParams {
            attempt_id,
            outcome: "rejected".into(),
            reasoning: "didn't compile".into(),
            git_ref: None,
            code_snippet: None,
        }))
        .await
        .unwrap();
    assert!(extract_json(&outcome)["success"].as_bool().unwrap());

    let ledger = server
        .review_ledger(Parameters(ReviewLedgerParams {
            task_id: task_id.clone(),
            outcome_filter: None,
        }))
        .await
        .unwrap();
    let ledger_json = extract_json(&ledger);
    assert_eq!(ledger_json["attempts"].as_array().unwrap().len(), 1);
    assert!(ledger_json["task_links"].as_array().unwrap().is_empty());

    let complete = server
        .complete_task(Parameters(CompleteTaskParams {
            task_id,
            lesson: Some("always check compilation first".into()),
            resolved_attempt_id: None,
        }))
        .await
        .unwrap();
    assert!(extract_json(&complete)["success"].as_bool().unwrap());
}

#[tokio::test]
async fn test_remember_and_recall_rules() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(switch_params("rules-test", "/tmp"))
        .await
        .unwrap();

    server
        .remember_rule(Parameters(RememberRuleParams {
            category: "fact".into(),
            content: "Rust is a systems language".into(),
            tags: None,
        }))
        .await
        .unwrap();

    let recalled = server
        .recall_rules(Parameters(RecallRulesParams {
            query: "systems language".into(),
            limit: Some(10),
            category: None,
            tags: None,
            cross_project: None,
            compact: None,
        }))
        .await
        .unwrap();
    let rules = extract_json(&recalled);
    assert!(!rules.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_recall_rules_cross_project() {
    let (server, pool, _c) = setup_server().await;

    let other_pid = lore::db::projects::create_project(&pool, "other-proj", "/other")
        .await
        .unwrap();
    let emb = vec![0.5_f32; 384];
    lore::db::semantic::create_rule(
        &pool,
        other_pid,
        lore::db::RuleCategory::Fact,
        "rust memory safety",
        Some(&emb),
        &[],
    )
    .await
    .unwrap();

    server
        .switch_project(switch_params("current-proj", "/cur"))
        .await
        .unwrap();

    let local = server
        .recall_rules(Parameters(RecallRulesParams {
            query: "rust memory safety".into(),
            limit: Some(10),
            category: None,
            tags: None,
            cross_project: Some(false),
            compact: None,
        }))
        .await
        .unwrap();
    let local_rules = extract_json(&local);
    assert!(local_rules.as_array().unwrap().is_empty());

    let cross = server
        .recall_rules(Parameters(RecallRulesParams {
            query: "rust memory safety".into(),
            limit: Some(10),
            category: None,
            tags: None,
            cross_project: Some(true),
            compact: None,
        }))
        .await
        .unwrap();
    let cross_rules = extract_json(&cross);
    assert!(!cross_rules.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_forget_rule() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(switch_params("forget-test", "/tmp"))
        .await
        .unwrap();

    let result = server
        .remember_rule(Parameters(RememberRuleParams {
            category: "preference".into(),
            content: "use tabs".into(),
            tags: None,
        }))
        .await
        .unwrap();
    let rule_id = extract_json(&result)["rule_id"]
        .as_str()
        .unwrap()
        .to_string();

    server
        .forget_rule_impl(ForgetRuleParams {
            rule_id,
            supersede: None,
            force: None,
        })
        .await
        .unwrap();

    let list = server
        .list_rules(Parameters(ListRulesParams {
            category: None,
            tags: None,
        }))
        .await
        .unwrap();
    assert!(extract_json(&list).as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_export_memory() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(switch_params("export-test", "/tmp"))
        .await
        .unwrap();
    server
        .remember_rule(Parameters(RememberRuleParams {
            category: "fact".into(),
            content: "test fact".into(),
            tags: None,
        }))
        .await
        .unwrap();

    let export = server
        .export_memory(Parameters(ExportMemoryParams {
            format: "json".into(),
        }))
        .await
        .unwrap();
    let json = extract_json(&export);

    assert!(!json["rules"].as_array().unwrap().is_empty());
    assert!(json["tasks"].is_array());
    assert!(json["attempts"].is_array());
}

#[tokio::test]
async fn test_get_task_stats() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(switch_params("stats-test", "/tmp"))
        .await
        .unwrap();

    let result = server
        .get_task_stats(Parameters(GetTaskStatsParams { status: None }))
        .await
        .unwrap();
    let json = extract_json(&result);
    assert_eq!(json["summary"]["total_tasks"], 0);

    let task = server
        .start_task(start_task_params("stats task"))
        .await
        .unwrap();
    let task_id = extract_json(&task)["task_id"].as_str().unwrap().to_string();

    let attempt = server
        .propose_attempt_impl(ProposeAttemptParams {
            task_id: task_id.clone(),
            approach_summary: "approach A".into(),
            agent_id: None,
            request_confirmation: None,
        })
        .await
        .unwrap();
    let attempt_id = extract_json(&attempt)["attempt_id"]
        .as_str()
        .unwrap()
        .to_string();

    server
        .log_outcome(Parameters(LogOutcomeParams {
            attempt_id,
            outcome: "rejected".into(),
            reasoning: "nope".into(),
            git_ref: None,
            code_snippet: None,
        }))
        .await
        .unwrap();

    let result = server
        .get_task_stats(Parameters(GetTaskStatsParams { status: None }))
        .await
        .unwrap();
    let json = extract_json(&result);
    assert_eq!(json["summary"]["total_tasks"], 1);
    assert_eq!(json["summary"]["total_rejected"], 1);

    server
        .complete_task(Parameters(CompleteTaskParams {
            task_id: task_id.clone(),
            lesson: None,
            resolved_attempt_id: None,
        }))
        .await
        .unwrap();

    let result = server
        .get_task_stats(Parameters(GetTaskStatsParams {
            status: Some("completed".into()),
        }))
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
        .switch_project(switch_params("abandon-test", "/tmp"))
        .await
        .unwrap();

    let task = server
        .start_task(start_task_params("abandon me"))
        .await
        .unwrap();
    let task_id = extract_json(&task)["task_id"].as_str().unwrap().to_string();

    let result = server
        .abandon_task(Parameters(AbandonTaskParams {
            task_id: task_id.clone(),
            reason: "not needed".into(),
            save_lesson: Some(true),
        }))
        .await
        .unwrap();
    assert!(extract_json(&result)["success"].as_bool().unwrap());

    let list = server
        .list_tasks(Parameters(ListTasksParams {
            status: Some("abandoned".into()),
        }))
        .await
        .unwrap();
    let tasks = extract_json(&list);
    assert_eq!(tasks.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn test_list_tasks() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(switch_params("list-test", "/tmp"))
        .await
        .unwrap();

    server
        .start_task(start_task_params("task A"))
        .await
        .unwrap();
    server
        .start_task(start_task_params("task B"))
        .await
        .unwrap();

    let list = server
        .list_tasks(Parameters(ListTasksParams { status: None }))
        .await
        .unwrap();
    let tasks = extract_json(&list);
    assert_eq!(tasks.as_array().unwrap().len(), 2);

    let active = server
        .list_tasks(Parameters(ListTasksParams {
            status: Some("active".into()),
        }))
        .await
        .unwrap();
    assert_eq!(extract_json(&active).as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_update_rule() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(switch_params("update-rule-test", "/tmp"))
        .await
        .unwrap();

    let result = server
        .remember_rule(Parameters(RememberRuleParams {
            category: "fact".into(),
            content: "original content".into(),
            tags: None,
        }))
        .await
        .unwrap();
    let rule_id = extract_json(&result)["rule_id"]
        .as_str()
        .unwrap()
        .to_string();

    let updated = server
        .update_rule(Parameters(UpdateRuleParams {
            rule_id: rule_id.clone(),
            category: Some("lesson".into()),
            content: Some("updated content".into()),
            tags: None,
        }))
        .await
        .unwrap();
    assert!(extract_json(&updated)["updated"].as_bool().unwrap());

    let list = server
        .list_rules(Parameters(ListRulesParams {
            category: Some("lesson".into()),
            tags: None,
        }))
        .await
        .unwrap();
    let rules = extract_json(&list).as_array().unwrap().clone();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["content"], "updated content");
}

#[tokio::test]
async fn test_find_similar_failures() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(switch_params("failures-test", "/tmp"))
        .await
        .unwrap();

    let result = server
        .find_similar_failures(Parameters(FindSimilarFailuresParams {
            error_description: "compilation error".into(),
            limit: Some(5),
            cross_project: None,
        }))
        .await
        .unwrap();
    let json = extract_json(&result);
    assert!(json.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_generate_handoff() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(switch_params("handoff-test", "/tmp"))
        .await
        .unwrap();

    server
        .start_task(start_task_params("active task"))
        .await
        .unwrap();

    let result = server
        .generate_handoff(Parameters(GenerateHandoffParams {
            token_count: Some(5000),
        }))
        .await
        .unwrap();
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
        .switch_project(switch_params("nextsteps-test", "/tmp"))
        .await
        .unwrap();

    let result = server
        .get_next_steps(Parameters(GetNextStepsParams { tier: None }))
        .await
        .unwrap();
    let json = extract_json(&result);
    assert!(json["project"].is_object());
    assert!(json["tasks"].is_array());
}

#[tokio::test]
async fn test_get_next_steps_l0_tier() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(switch_params("l0-test", "/tmp"))
        .await
        .unwrap();
    server
        .start_task(start_task_params("active one"))
        .await
        .unwrap();

    let result = server
        .get_next_steps(Parameters(GetNextStepsParams {
            tier: Some("L0".into()),
        }))
        .await
        .unwrap();
    let json = extract_json(&result);

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
        .switch_project(switch_params("l0-ci", "/tmp"))
        .await
        .unwrap();

    let result = server
        .get_next_steps(Parameters(GetNextStepsParams {
            tier: Some("l0".into()),
        }))
        .await
        .unwrap();
    let json = extract_json(&result);
    assert!(json["active_task_count"].is_number());
}

#[tokio::test]
async fn test_log_context_wipe() {
    let (server, _pool, _c) = setup_server().await;

    server
        .switch_project(switch_params("wipe-test", "/tmp"))
        .await
        .unwrap();

    let task = server
        .start_task(start_task_params("wipe task"))
        .await
        .unwrap();
    let task_id = extract_json(&task)["task_id"].as_str().unwrap().to_string();

    let result = server
        .log_context_wipe(Parameters(LogContextWipeParams {
            task_id,
            token_count: 10000,
            last_attempt_id: None,
        }))
        .await
        .unwrap();
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

async fn remember_then_flag_always_inject(
    server: &LoreServer,
    pool: &sqlx::PgPool,
    content: &str,
) -> String {
    let res = server
        .remember_rule(Parameters(RememberRuleParams {
            category: "instruction".into(),
            content: content.into(),
            tags: None,
        }))
        .await
        .unwrap();
    let rule_id = extract_json(&res)["rule_id"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("remember_rule returned no rule_id — dedup guard fired for content: {content:?}")
        })
        .to_string();
    sqlx::query("UPDATE ai_memory.semantic_rules SET is_always_injected = true WHERE id = $1")
        .bind(uuid::Uuid::parse_str(&rule_id).unwrap())
        .execute(pool)
        .await
        .unwrap();
    rule_id
}

#[tokio::test]
async fn test_get_active_context_procedural_block_surfaces_with_flag() {
    let (server, pool, _c) = setup_server_with(|cfg| cfg.procedural_memory = true).await;
    server
        .switch_project(switch_params("procedural-on", "/tmp/procedural-on"))
        .await
        .unwrap();
    let id =
        remember_then_flag_always_inject(&server, &pool, "prefer explicit error handling").await;

    let ctx = server.get_active_context().await.unwrap();
    let json = extract_json(&ctx);
    let block = json["procedural"].as_object().expect("procedural block");
    assert_eq!(block["truncated"], false);
    assert_eq!(block["limit"], 20);
    assert_eq!(block["byte_budget"], 2000);
    let rules = block["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["id"], id);
    assert_eq!(rules[0]["content"], "prefer explicit error handling");
}

#[tokio::test]
async fn test_get_active_context_procedural_block_absent_without_flag() {
    let (server, pool, _c) = setup_server_with(|cfg| cfg.procedural_memory = false).await;
    server
        .switch_project(switch_params("procedural-off", "/tmp/procedural-off"))
        .await
        .unwrap();
    let _ = remember_then_flag_always_inject(&server, &pool, "always run fmt before commit").await;

    let ctx = server.get_active_context().await.unwrap();
    let json = extract_json(&ctx);
    assert!(
        json.get("procedural").is_none(),
        "procedural block must be absent when LORE_PROCEDURAL_MEMORY is off"
    );
}

#[tokio::test]
async fn test_get_active_context_procedural_excludes_non_always_inject() {
    let (server, _pool, _c) = setup_server_with(|cfg| cfg.procedural_memory = true).await;
    server
        .switch_project(switch_params("procedural-filter", "/tmp/procedural-filter"))
        .await
        .unwrap();
    server
        .remember_rule(Parameters(RememberRuleParams {
            category: "preference".into(),
            content: "don't flag this one".into(),
            tags: None,
        }))
        .await
        .unwrap();

    let ctx = server.get_active_context().await.unwrap();
    let json = extract_json(&ctx);
    let block = json["procedural"].as_object().expect("procedural block");
    assert!(
        block["rules"].as_array().unwrap().is_empty(),
        "non-always-inject rules must not surface in procedural block"
    );
}
