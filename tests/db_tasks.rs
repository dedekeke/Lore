mod common;

use lore::db::attempts::{self, AttemptOutcome, LogOutcomeArgs};
use lore::db::tasks::{TaskListFilters, TaskStatus};
use lore::db::{projects, task_links, tasks};

#[tokio::test]
async fn test_create_and_get_task() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let tid = tasks::create_task(&pool, pid, "do something", None, None, None, None, None)
        .await
        .unwrap();
    let task = tasks::get_task(&pool, tid).await.unwrap().unwrap();

    assert_eq!(task.description, "do something");
    assert_eq!(task.status, TaskStatus::Active);
    assert!(task.completed_at.is_none());
}

#[tokio::test]
async fn test_list_tasks_with_status_filter() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let t1 = tasks::create_task(&pool, pid, "task1", None, None, None, None, None)
        .await
        .unwrap();
    tasks::create_task(&pool, pid, "task2", None, None, None, None, None)
        .await
        .unwrap();
    tasks::complete_task(&pool, t1, None).await.unwrap();

    let active = tasks::list_tasks(&pool, pid, Some(TaskStatus::Active))
        .await
        .unwrap();
    let all = tasks::list_tasks(&pool, pid, None).await.unwrap();

    assert_eq!(active.len(), 1);
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn test_complete_task() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let tid = tasks::create_task(&pool, pid, "finish me", None, None, None, None, None)
        .await
        .unwrap();

    assert!(tasks::complete_task(&pool, tid, None).await.unwrap());

    let task = tasks::get_task(&pool, tid).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Completed);
    assert!(task.completed_at.is_some());
}

#[tokio::test]
async fn test_update_task_status() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let tid = tasks::create_task(&pool, pid, "block me", None, None, None, None, None)
        .await
        .unwrap();

    tasks::update_task_status(&pool, tid, TaskStatus::Blocked)
        .await
        .unwrap();
    let task = tasks::get_task(&pool, tid).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Blocked);
}

#[tokio::test]
async fn test_subtask_parent() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let parent = tasks::create_task(&pool, pid, "parent", None, None, None, None, None)
        .await
        .unwrap();
    let child = tasks::create_task(&pool, pid, "child", Some(parent), None, None, None, None)
        .await
        .unwrap();

    let task = tasks::get_task(&pool, child).await.unwrap().unwrap();
    assert_eq!(task.parent_task_id, Some(parent));
}

#[tokio::test]
async fn test_get_task_stats_no_tasks() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let stats = tasks::get_task_stats(&pool, pid, None).await.unwrap();
    assert!(stats.is_empty());
}

#[tokio::test]
async fn test_get_task_stats_with_attempts() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let t1 = tasks::create_task(&pool, pid, "stats task", None, None, None, None, None)
        .await
        .unwrap();
    let a1 = attempts::create_attempt(&pool, t1, "approach 1", None, None, None)
        .await
        .unwrap();
    attempts::log_outcome(
        &pool,
        LogOutcomeArgs {
            attempt_id: a1,
            outcome: AttemptOutcome::Rejected,
            reasoning: "bad approach",
            reasoning_embedding: None,
            git_ref: None,
            code_snippet: None,
            resolved_by_agent_id: None,
            resolved_by_session_id: None,
        },
    )
    .await
    .unwrap();
    let a2 = attempts::create_attempt(&pool, t1, "approach 2", None, None, None)
        .await
        .unwrap();
    attempts::log_outcome(
        &pool,
        LogOutcomeArgs {
            attempt_id: a2,
            outcome: AttemptOutcome::Accepted,
            reasoning: "works",
            reasoning_embedding: None,
            git_ref: None,
            code_snippet: None,
            resolved_by_agent_id: None,
            resolved_by_session_id: None,
        },
    )
    .await
    .unwrap();

    // Active task — no resolution_minutes
    let stats = tasks::get_task_stats(&pool, pid, None).await.unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].total_attempts, 2);
    assert_eq!(stats[0].rejected_attempts, 1);
    assert_eq!(stats[0].accepted_attempts, 1);
    assert!(stats[0].resolution_minutes.is_none());

    // Complete the task — now resolution_minutes should be set
    tasks::complete_task(&pool, t1, Some(a2)).await.unwrap();
    let stats = tasks::get_task_stats(&pool, pid, None).await.unwrap();
    assert!(stats[0].resolution_minutes.is_some());
}

#[tokio::test]
async fn test_get_task_stats_filtered_by_status() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let t1 = tasks::create_task(&pool, pid, "active", None, None, None, None, None)
        .await
        .unwrap();
    let t2 = tasks::create_task(&pool, pid, "done", None, None, None, None, None)
        .await
        .unwrap();
    tasks::complete_task(&pool, t2, None).await.unwrap();

    let active = tasks::get_task_stats(&pool, pid, Some(TaskStatus::Active))
        .await
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, t1);

    let completed = tasks::get_task_stats(&pool, pid, Some(TaskStatus::Completed))
        .await
        .unwrap();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].id, t2);
}

#[tokio::test]
async fn test_create_task_with_description_embedding() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let emb = vec![0.42_f32; 384];
    let tid = tasks::create_task(
        &pool,
        pid,
        "embedded task",
        None,
        None,
        None,
        None,
        Some(&emb),
    )
    .await
    .unwrap();

    let task = tasks::get_task(&pool, tid).await.unwrap().unwrap();
    assert_eq!(task.description, "embedded task");
    let stored = task.description_embedding.unwrap();
    let stored_vec: Vec<f32> = stored.to_vec();
    assert_eq!(stored_vec.len(), 384);
    assert!((stored_vec[0] - 0.42).abs() < 1e-6);
}

#[tokio::test]
async fn test_create_task_without_embedding() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let tid = tasks::create_task(&pool, pid, "no emb", None, None, None, None, None)
        .await
        .unwrap();
    let task = tasks::get_task(&pool, tid).await.unwrap().unwrap();
    assert!(task.description_embedding.is_none());
}

#[tokio::test]
async fn test_list_tasks_includes_embedding() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let emb = vec![0.1_f32; 384];
    tasks::create_task(&pool, pid, "t1", None, None, None, None, Some(&emb))
        .await
        .unwrap();
    tasks::create_task(&pool, pid, "t2", None, None, None, None, None)
        .await
        .unwrap();

    let all = tasks::list_tasks(&pool, pid, None).await.unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().any(|t| t.description_embedding.is_some()));
    assert!(all.iter().any(|t| t.description_embedding.is_none()));
}

#[tokio::test]
async fn test_ticket_number_create_update_clear() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    // Create with ticket
    let tid = tasks::create_task(
        &pool,
        pid,
        "with ticket",
        None,
        None,
        None,
        Some("ABC-123"),
        None,
    )
    .await
    .unwrap();
    let task = tasks::get_task(&pool, tid).await.unwrap().unwrap();
    assert_eq!(task.ticket_number.as_deref(), Some("ABC-123"));

    // Update to a new ticket
    let updated = tasks::apply_task_update(
        &pool,
        tid,
        tasks::TaskUpdate {
            ticket_number: Some(Some("ENG-42")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(updated);
    let task = tasks::get_task(&pool, tid).await.unwrap().unwrap();
    assert_eq!(task.ticket_number.as_deref(), Some("ENG-42"));

    // Clear it
    let updated = tasks::apply_task_update(
        &pool,
        tid,
        tasks::TaskUpdate {
            ticket_number: Some(None),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(updated);
    let task = tasks::get_task(&pool, tid).await.unwrap().unwrap();
    assert!(task.ticket_number.is_none());

    // CHECK constraint rejects empty/whitespace
    let bad = tasks::create_task(&pool, pid, "bad", None, None, None, Some("   "), None).await;
    assert!(bad.is_err());
}

#[tokio::test]
async fn test_apply_task_update_description_also_updates_summary() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    // Long description so the auto-generated summary is non-trivially derived.
    let long = "a".repeat(200);
    let tid = tasks::create_task(&pool, pid, &long, None, None, None, None, None)
        .await
        .unwrap();

    let updated = tasks::apply_task_update(
        &pool,
        tid,
        tasks::TaskUpdate {
            description: Some("short replacement"),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(updated);

    let task = tasks::get_task(&pool, tid).await.unwrap().unwrap();
    assert_eq!(task.description, "short replacement");
    // generate_summary leaves <=120-char descriptions unchanged.
    assert_eq!(task.summary.as_deref(), Some("short replacement"));
}

#[tokio::test]
async fn test_ticket_number_survives_complete_task() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let tid = tasks::create_task(
        &pool,
        pid,
        "complete with ticket",
        None,
        None,
        None,
        Some("ABC-7"),
        None,
    )
    .await
    .unwrap();
    tasks::complete_task(&pool, tid, None).await.unwrap();

    let task = tasks::get_task(&pool, tid).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Completed);
    assert_eq!(task.ticket_number.as_deref(), Some("ABC-7"));
}

#[tokio::test]
async fn test_find_by_ticket_number_hit_miss_and_multi() {
    let (pool, _c) = common::setup_db().await;
    let pid_a = projects::create_project(&pool, "a", "/a").await.unwrap();
    let pid_b = projects::create_project(&pool, "b", "/b").await.unwrap();

    // Two tasks in project A share a ticket
    let t1 = tasks::create_task(
        &pool,
        pid_a,
        "refactor under ABC-1",
        None,
        None,
        None,
        Some("ABC-1"),
        None,
    )
    .await
    .unwrap();
    let t2 = tasks::create_task(
        &pool,
        pid_a,
        "follow-up bug under ABC-1",
        None,
        None,
        None,
        Some("ABC-1"),
        None,
    )
    .await
    .unwrap();
    // Different ticket in project A
    tasks::create_task(&pool, pid_a, "other", None, None, None, Some("XYZ-9"), None)
        .await
        .unwrap();
    // Same ticket value in project B — must NOT leak across projects
    tasks::create_task(
        &pool,
        pid_b,
        "cross-project same ticket",
        None,
        None,
        None,
        Some("ABC-1"),
        None,
    )
    .await
    .unwrap();

    // Multi-hit
    let hits = tasks::find_by_ticket_number(&pool, pid_a, "ABC-1")
        .await
        .unwrap();
    assert_eq!(hits.len(), 2);
    let ids: Vec<_> = hits.iter().map(|t| t.id).collect();
    assert!(ids.contains(&t1) && ids.contains(&t2));

    // Single-hit
    let hits = tasks::find_by_ticket_number(&pool, pid_a, "XYZ-9")
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);

    // Miss
    let hits = tasks::find_by_ticket_number(&pool, pid_a, "NOPE-0")
        .await
        .unwrap();
    assert!(hits.is_empty());

    // Project isolation: project B has 1 task with ABC-1, project A has 2; never cross
    let hits_b = tasks::find_by_ticket_number(&pool, pid_b, "ABC-1")
        .await
        .unwrap();
    assert_eq!(hits_b.len(), 1);
    assert!(!ids.contains(&hits_b[0].id));
}

#[tokio::test]
async fn test_task_summary_includes_ticket_number() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    tasks::create_task(
        &pool,
        pid,
        "with ticket",
        None,
        Some("P1"),
        None,
        Some("LORE-9"),
        None,
    )
    .await
    .unwrap();

    let summaries = tasks::get_task_summaries(&pool, pid, &[TaskStatus::Active])
        .await
        .unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].ticket_number.as_deref(), Some("LORE-9"));
}

#[tokio::test]
async fn test_filtered_count_and_list_paginated() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "filter-test", "/")
        .await
        .unwrap();

    tasks::create_task(
        &pool,
        pid,
        "alpha",
        None,
        Some("P1"),
        Some("Bug"),
        Some("ABC-1"),
        None,
    )
    .await
    .unwrap();
    tasks::create_task(
        &pool,
        pid,
        "beta",
        None,
        Some("P2"),
        Some("Feature"),
        Some("ABC-2"),
        None,
    )
    .await
    .unwrap();
    tasks::create_task(
        &pool,
        pid,
        "gamma",
        None,
        Some("P1"),
        Some("Feature"),
        Some("XYZ-9"),
        None,
    )
    .await
    .unwrap();
    let done = tasks::create_task(
        &pool,
        pid,
        "delta",
        None,
        Some("P3"),
        Some("Bug"),
        Some("ABC-7"),
        None,
    )
    .await
    .unwrap();
    tasks::complete_task(&pool, done, None).await.unwrap();

    // Active + priority=P1 → alpha, gamma
    let f = TaskListFilters {
        status: Some(TaskStatus::Active),
        priority: Some("P1"),
        ..Default::default()
    };
    assert_eq!(tasks::count_tasks(&pool, pid, f.clone()).await.unwrap(), 2);
    let rows = tasks::list_tasks_paginated(&pool, pid, f, "created_at", "asc", 50, 0)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);

    // Active + task_type=Feature → beta, gamma
    let f = TaskListFilters {
        status: Some(TaskStatus::Active),
        task_type: Some("Feature"),
        ..Default::default()
    };
    assert_eq!(tasks::count_tasks(&pool, pid, f.clone()).await.unwrap(), 2);
    let rows = tasks::list_tasks_paginated(&pool, pid, f, "created_at", "asc", 50, 0)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|t| t.task_type.as_deref() == Some("Feature")));

    // Active + ticket_prefix=abc (case-insensitive) → alpha, beta (gamma is XYZ, delta is completed)
    let f = TaskListFilters {
        status: Some(TaskStatus::Active),
        ticket_prefix: Some("abc"),
        ..Default::default()
    };
    assert_eq!(tasks::count_tasks(&pool, pid, f).await.unwrap(), 2);

    // No status filter + ticket_prefix=ABC → alpha, beta, delta
    let f = TaskListFilters {
        ticket_prefix: Some("ABC"),
        ..Default::default()
    };
    assert_eq!(tasks::count_tasks(&pool, pid, f).await.unwrap(), 3);

    // All filters stacked → alpha
    let f = TaskListFilters {
        status: Some(TaskStatus::Active),
        priority: Some("P1"),
        task_type: Some("Bug"),
        ticket_prefix: Some("ABC"),
    };
    assert_eq!(tasks::count_tasks(&pool, pid, f.clone()).await.unwrap(), 1);
    let rows = tasks::list_tasks_paginated(&pool, pid, f, "created_at", "asc", 50, 0)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].description, "alpha");
}

#[tokio::test]
async fn test_cascade_close_decomp_subtasks_inherits_parent_resolved_attempt() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let parent = tasks::create_task(&pool, pid, "parent", None, None, None, None, None)
        .await
        .unwrap();
    let child_decomp = tasks::create_task(
        &pool,
        pid,
        "decomp child",
        Some(parent),
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    // Parent has an accepted attempt; child has none.
    let parent_attempt = attempts::create_attempt(&pool, parent, "ap", None, None, None)
        .await
        .unwrap();
    attempts::log_outcome(
        &pool,
        LogOutcomeArgs {
            attempt_id: parent_attempt,
            outcome: AttemptOutcome::Accepted,
            reasoning: "ok",
            reasoning_embedding: None,
            git_ref: None,
            code_snippet: None,
            resolved_by_agent_id: None,
            resolved_by_session_id: None,
        },
    )
    .await
    .unwrap();

    let cascaded =
        tasks::cascade_close_decomposition_subtasks(&pool, parent, Some(parent_attempt)).await;
    assert_eq!(cascaded, 1);

    let c = tasks::get_task(&pool, child_decomp).await.unwrap().unwrap();
    assert_eq!(c.status, TaskStatus::Completed);
    assert_eq!(c.resolved_attempt_id, Some(parent_attempt));
}

#[tokio::test]
async fn test_cascade_close_skips_followup_subtasks() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let parent = tasks::create_task(&pool, pid, "parent", None, None, None, None, None)
        .await
        .unwrap();
    let followup = tasks::create_task(&pool, pid, "fu", Some(parent), None, None, None, None)
        .await
        .unwrap();
    task_links::upsert_link(&pool, parent, followup, "follow_up")
        .await
        .unwrap();

    let cascaded = tasks::cascade_close_decomposition_subtasks(&pool, parent, None).await;
    assert_eq!(cascaded, 0);

    let f = tasks::get_task(&pool, followup).await.unwrap().unwrap();
    assert_eq!(f.status, TaskStatus::Active);
}

#[tokio::test]
async fn test_cascade_close_prefers_child_accepted_attempt_over_parent() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let parent = tasks::create_task(&pool, pid, "parent", None, None, None, None, None)
        .await
        .unwrap();
    let child = tasks::create_task(&pool, pid, "child", Some(parent), None, None, None, None)
        .await
        .unwrap();

    let parent_attempt = attempts::create_attempt(&pool, parent, "p-ap", None, None, None)
        .await
        .unwrap();
    let child_attempt = attempts::create_attempt(&pool, child, "c-ap", None, None, None)
        .await
        .unwrap();
    attempts::log_outcome(
        &pool,
        LogOutcomeArgs {
            attempt_id: child_attempt,
            outcome: AttemptOutcome::Accepted,
            reasoning: "child ok",
            reasoning_embedding: None,
            git_ref: None,
            code_snippet: None,
            resolved_by_agent_id: None,
            resolved_by_session_id: None,
        },
    )
    .await
    .unwrap();

    let cascaded =
        tasks::cascade_close_decomposition_subtasks(&pool, parent, Some(parent_attempt)).await;
    assert_eq!(cascaded, 1);

    let c = tasks::get_task(&pool, child).await.unwrap().unwrap();
    assert_eq!(c.resolved_attempt_id, Some(child_attempt));
}

#[tokio::test]
async fn test_list_active_decomposition_subtasks_filters_completed_and_followups() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let parent = tasks::create_task(&pool, pid, "parent", None, None, None, None, None)
        .await
        .unwrap();
    let active_decomp = tasks::create_task(
        &pool,
        pid,
        "active decomp",
        Some(parent),
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let done_decomp = tasks::create_task(
        &pool,
        pid,
        "done decomp",
        Some(parent),
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    tasks::complete_task(&pool, done_decomp, None)
        .await
        .unwrap();
    let followup = tasks::create_task(&pool, pid, "fu", Some(parent), None, None, None, None)
        .await
        .unwrap();
    task_links::upsert_link(&pool, parent, followup, "follow_up")
        .await
        .unwrap();

    let active = tasks::list_active_decomposition_subtasks(&pool, parent)
        .await
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, active_decomp);
}
