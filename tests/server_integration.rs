mod common;

use lore::config::Config;
use lore::embeddings::fake::FakeEmbeddingProvider;
use lore::embeddings::AnyEmbeddingProvider;
use lore::server::{
    AbandonTaskParams, CompleteTaskParams, DeleteScratchParams, ExportMemoryParams,
    FindSimilarFailuresParams, ForgetRuleParams, GenerateHandoffParams, GetNextStepsParams,
    GetTaskStatsParams, ListRulesParams, ListScratchParams, ListTasksParams, LogContextWipeParams,
    LogOutcomeParams, LoreServer, ProposeAttemptParams, ReadScratchParams, RecallRulesParams,
    RememberRuleParams, ReviewLedgerParams, StartTaskParams, SwitchProjectParams, UpdateRuleParams,
    WriteScratchParams,
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
            always_inject: None,
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
            always_inject: None,
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
            always_inject: None,
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
            always_inject: None,
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
            always_inject: None,
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
            always_inject: None,
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
            always_inject: None,
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

async fn rule_is_flagged(pool: &sqlx::PgPool, rule_id: &str) -> bool {
    let row: (bool,) =
        sqlx::query_as("SELECT is_always_injected FROM ai_memory.semantic_rules WHERE id = $1")
            .bind(uuid::Uuid::parse_str(rule_id).unwrap())
            .fetch_one(pool)
            .await
            .unwrap();
    row.0
}

#[tokio::test]
async fn test_remember_rule_instruction_defaults_to_always_inject_true() {
    let (server, pool, _c) = setup_server().await;
    server
        .switch_project(switch_params("rr-instr-default", "/tmp/rr-instr-default"))
        .await
        .unwrap();

    let res = server
        .remember_rule(Parameters(RememberRuleParams {
            category: "instruction".into(),
            content: "prefix all migrations with timestamp".into(),
            tags: None,
            always_inject: None,
        }))
        .await
        .unwrap();
    let body = extract_json(&res);
    assert_eq!(body["always_inject"], true);
    let rule_id = body["rule_id"].as_str().unwrap();
    assert!(rule_is_flagged(&pool, rule_id).await);
}

#[tokio::test]
async fn test_remember_rule_non_instruction_defaults_to_false() {
    let (server, pool, _c) = setup_server().await;
    server
        .switch_project(switch_params("rr-fact-default", "/tmp/rr-fact-default"))
        .await
        .unwrap();

    let res = server
        .remember_rule(Parameters(RememberRuleParams {
            category: "fact".into(),
            content: "repo uses sqlx 0.8".into(),
            tags: None,
            always_inject: None,
        }))
        .await
        .unwrap();
    let body = extract_json(&res);
    assert_eq!(body["always_inject"], false);
    let rule_id = body["rule_id"].as_str().unwrap();
    assert!(!rule_is_flagged(&pool, rule_id).await);
}

#[tokio::test]
async fn test_remember_rule_explicit_override_wins_for_non_instruction() {
    let (server, pool, _c) = setup_server().await;
    server
        .switch_project(switch_params("rr-override-on", "/tmp/rr-override-on"))
        .await
        .unwrap();

    let res = server
        .remember_rule(Parameters(RememberRuleParams {
            category: "constraint".into(),
            content: "never run untrusted SQL".into(),
            tags: None,
            always_inject: Some(true),
        }))
        .await
        .unwrap();
    let body = extract_json(&res);
    assert_eq!(body["always_inject"], true);
    let rule_id = body["rule_id"].as_str().unwrap();
    assert!(rule_is_flagged(&pool, rule_id).await);
}

#[tokio::test]
async fn test_remember_rule_explicit_override_wins_for_instruction() {
    let (server, pool, _c) = setup_server().await;
    server
        .switch_project(switch_params("rr-override-off", "/tmp/rr-override-off"))
        .await
        .unwrap();

    let res = server
        .remember_rule(Parameters(RememberRuleParams {
            category: "instruction".into(),
            content: "document instruction that shouldn't auto-inject".into(),
            tags: None,
            always_inject: Some(false),
        }))
        .await
        .unwrap();
    let body = extract_json(&res);
    assert_eq!(body["always_inject"], false);
    let rule_id = body["rule_id"].as_str().unwrap();
    assert!(!rule_is_flagged(&pool, rule_id).await);
}

#[tokio::test]
async fn test_remember_rule_near_cap_warning() {
    let (server, pool, _c) = setup_server().await;
    server
        .switch_project(switch_params("rr-near-cap", "/tmp/rr-near-cap"))
        .await
        .unwrap();

    let project_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM ai_memory.projects WHERE name = $1")
            .bind("rr-near-cap")
            .fetch_one(&pool)
            .await
            .unwrap();

    // Seed 17 always-inject rules so the next insert hits exactly 18 (near_cap).
    for i in 0..17 {
        sqlx::query(
            "INSERT INTO ai_memory.semantic_rules (project_id, category, content, is_always_injected) \
             VALUES ($1, 'instruction', $2, true)",
        )
        .bind(project_id)
        .bind(format!("seeded rule {i}"))
        .execute(&pool)
        .await
        .unwrap();
    }

    let res = server
        .remember_rule(Parameters(RememberRuleParams {
            category: "instruction".into(),
            content: "the 18th standing instruction".into(),
            tags: None,
            always_inject: None,
        }))
        .await
        .unwrap();
    let body = extract_json(&res);
    let warn = body["always_inject_cap_warning"]
        .as_object()
        .expect("near-cap warning");
    assert_eq!(warn["level"], "near_cap");
    assert_eq!(warn["count"], 18);
    assert_eq!(warn["limit"], 20);
}

#[tokio::test]
async fn test_remember_rule_at_cap_warning() {
    let (server, pool, _c) = setup_server().await;
    server
        .switch_project(switch_params("rr-at-cap", "/tmp/rr-at-cap"))
        .await
        .unwrap();
    let project_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM ai_memory.projects WHERE name = $1")
            .bind("rr-at-cap")
            .fetch_one(&pool)
            .await
            .unwrap();

    // Seed 19 so the write brings the post-count to exactly 20 = at_cap.
    for i in 0..19 {
        sqlx::query(
            "INSERT INTO ai_memory.semantic_rules (project_id, category, content, is_always_injected) \
             VALUES ($1, 'instruction', $2, true)",
        )
        .bind(project_id)
        .bind(format!("seed {i}"))
        .execute(&pool)
        .await
        .unwrap();
    }

    let res = server
        .remember_rule(Parameters(RememberRuleParams {
            category: "instruction".into(),
            content: "exactly-at-cap instruction".into(),
            tags: None,
            always_inject: None,
        }))
        .await
        .unwrap();
    let body = extract_json(&res);
    let warn = body["always_inject_cap_warning"]
        .as_object()
        .expect("at-cap warning");
    assert_eq!(warn["level"], "at_cap");
    assert_eq!(warn["count"], 20);
}

#[tokio::test]
async fn test_remember_rule_over_cap_warning() {
    let (server, pool, _c) = setup_server().await;
    server
        .switch_project(switch_params("rr-over-cap", "/tmp/rr-over-cap"))
        .await
        .unwrap();
    let project_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM ai_memory.projects WHERE name = $1")
            .bind("rr-over-cap")
            .fetch_one(&pool)
            .await
            .unwrap();

    for i in 0..20 {
        sqlx::query(
            "INSERT INTO ai_memory.semantic_rules (project_id, category, content, is_always_injected) \
             VALUES ($1, 'instruction', $2, true)",
        )
        .bind(project_id)
        .bind(format!("seed {i}"))
        .execute(&pool)
        .await
        .unwrap();
    }

    let res = server
        .remember_rule(Parameters(RememberRuleParams {
            category: "instruction".into(),
            content: "overflow instruction".into(),
            tags: None,
            always_inject: None,
        }))
        .await
        .unwrap();
    let body = extract_json(&res);
    let warn = body["always_inject_cap_warning"]
        .as_object()
        .expect("over-cap warning");
    assert_eq!(warn["level"], "over_cap");
    assert_eq!(warn["count"], 21);
}

#[tokio::test]
async fn test_remember_rule_no_warning_when_flag_off() {
    let (server, pool, _c) = setup_server().await;
    server
        .switch_project(switch_params("rr-no-warn", "/tmp/rr-no-warn"))
        .await
        .unwrap();
    let project_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM ai_memory.projects WHERE name = $1")
            .bind("rr-no-warn")
            .fetch_one(&pool)
            .await
            .unwrap();

    // Seed the project at the cap to prove the probe is gated on the write's flag.
    for i in 0..20 {
        sqlx::query(
            "INSERT INTO ai_memory.semantic_rules (project_id, category, content, is_always_injected) \
             VALUES ($1, 'instruction', $2, true)",
        )
        .bind(project_id)
        .bind(format!("seed {i}"))
        .execute(&pool)
        .await
        .unwrap();
    }

    let res = server
        .remember_rule(Parameters(RememberRuleParams {
            category: "fact".into(),
            content: "no inject on this one".into(),
            tags: None,
            always_inject: None,
        }))
        .await
        .unwrap();
    let body = extract_json(&res);
    assert!(
        body.get("always_inject_cap_warning").is_none(),
        "no warning expected when rule is not always-inject"
    );
}

#[tokio::test]
async fn test_update_rule_flips_always_inject() {
    let (server, pool, _c) = setup_server().await;
    server
        .switch_project(switch_params("ur-flip", "/tmp/ur-flip"))
        .await
        .unwrap();

    let created = server
        .remember_rule(Parameters(RememberRuleParams {
            category: "fact".into(),
            content: "flip me later".into(),
            tags: None,
            always_inject: None,
        }))
        .await
        .unwrap();
    let rule_id = extract_json(&created)["rule_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!rule_is_flagged(&pool, &rule_id).await);

    let res = server
        .update_rule(Parameters(UpdateRuleParams {
            rule_id: rule_id.clone(),
            category: None,
            content: None,
            tags: None,
            always_inject: Some(true),
        }))
        .await
        .unwrap();
    let body = extract_json(&res);
    assert_eq!(body["updated"], true);
    assert_eq!(body["always_inject"], true);
    assert!(rule_is_flagged(&pool, &rule_id).await);
}

#[tokio::test]
async fn test_update_rule_allows_only_always_inject() {
    let (server, pool, _c) = setup_server().await;
    server
        .switch_project(switch_params("ur-only-flag", "/tmp/ur-only-flag"))
        .await
        .unwrap();

    let created = server
        .remember_rule(Parameters(RememberRuleParams {
            category: "instruction".into(),
            content: "start as always-inject".into(),
            tags: None,
            always_inject: None,
        }))
        .await
        .unwrap();
    let rule_id = extract_json(&created)["rule_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Only the always_inject field — must not error with "Provide at least one of"
    let res = server
        .update_rule(Parameters(UpdateRuleParams {
            rule_id: rule_id.clone(),
            category: None,
            content: None,
            tags: None,
            always_inject: Some(false),
        }))
        .await
        .unwrap();
    let body = extract_json(&res);
    assert_eq!(body["updated"], true);
    assert_eq!(body["always_inject"], false);
    assert!(!rule_is_flagged(&pool, &rule_id).await);
}

#[tokio::test]
async fn test_scratchpad_write_read_list_delete() {
    let (server, _pool, _c) = setup_server().await;
    server
        .switch_project(switch_params("scratch-proj", "/tmp/scratch-proj"))
        .await
        .unwrap();

    let written = server
        .write_scratch(Parameters(WriteScratchParams {
            key: "focus".into(),
            value: "auth refactor".into(),
            task_id: None,
            ttl_secs: None,
        }))
        .await
        .unwrap();
    let body = extract_json(&written);
    assert_eq!(body["key"], "focus");
    assert_eq!(body["value"], "auth refactor");

    let read = server
        .read_scratch(Parameters(ReadScratchParams {
            key: "focus".into(),
            task_id: None,
        }))
        .await
        .unwrap();
    assert_eq!(extract_json(&read)["value"], "auth refactor");

    server
        .write_scratch(Parameters(WriteScratchParams {
            key: "focus".into(),
            value: "rename columns".into(),
            task_id: None,
            ttl_secs: None,
        }))
        .await
        .unwrap();
    let read2 = server
        .read_scratch(Parameters(ReadScratchParams {
            key: "focus".into(),
            task_id: None,
        }))
        .await
        .unwrap();
    assert_eq!(extract_json(&read2)["value"], "rename columns");

    let listed = server
        .list_scratch(Parameters(ListScratchParams {
            task_id: None,
            limit: Some(10),
        }))
        .await
        .unwrap();
    let list = extract_json(&listed);
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["key"], "focus");

    let deleted = server
        .delete_scratch(Parameters(DeleteScratchParams {
            key: "focus".into(),
            task_id: None,
        }))
        .await
        .unwrap();
    assert_eq!(extract_json(&deleted)["deleted"], true);

    let miss = server
        .read_scratch(Parameters(ReadScratchParams {
            key: "focus".into(),
            task_id: None,
        }))
        .await
        .unwrap();
    assert!(extract_json(&miss)["entry"].is_null());
}

#[tokio::test]
async fn test_scratchpad_value_scrubbed() {
    let (server, _pool, _c) = setup_server_with(|c| c.scrub_secrets = true).await;
    server
        .switch_project(switch_params("scratch-scrub", "/tmp/scratch-scrub"))
        .await
        .unwrap();

    let secret = "api_key=abcdefghijklmnop1234";
    server
        .write_scratch(Parameters(WriteScratchParams {
            key: "creds".into(),
            value: secret.into(),
            task_id: None,
            ttl_secs: None,
        }))
        .await
        .unwrap();

    let read = server
        .read_scratch(Parameters(ReadScratchParams {
            key: "creds".into(),
            task_id: None,
        }))
        .await
        .unwrap();
    let stored = extract_json(&read)["value"].as_str().unwrap().to_string();
    assert!(
        !stored.contains("abcdefghijklmnop1234"),
        "secret must be scrubbed from stored value, got {stored:?}"
    );
    assert!(stored.contains("[REDACTED]"));
}

#[tokio::test]
async fn test_scratchpad_invalid_ttl_rejected() {
    let (server, _pool, _c) = setup_server().await;
    server
        .switch_project(switch_params("scratch-ttl", "/tmp/scratch-ttl"))
        .await
        .unwrap();

    let err = server
        .write_scratch(Parameters(WriteScratchParams {
            key: "k".into(),
            value: "v".into(),
            task_id: None,
            ttl_secs: Some(0),
        }))
        .await;
    assert!(err.is_err(), "ttl_secs=0 must be rejected");
}
