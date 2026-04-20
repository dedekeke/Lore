use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Html,
    routing::{delete, get, post},
    Json, Router,
};
use minijinja::{context, Environment};
use sqlx::PgPool;

use crate::db;

#[derive(Clone)]
pub struct DashboardState {
    pool: PgPool,
    env: Arc<Environment<'static>>,
}

fn truncate_filter(value: String, kwargs: minijinja::value::Kwargs) -> String {
    let length: usize = kwargs.get("length").unwrap_or(255);
    if value.len() <= length {
        value
    } else {
        let mut s: String = value.chars().take(length.saturating_sub(3)).collect();
        s.push_str("...");
        s
    }
}

/// Format a DateTime string as dd-mm-yyyy
fn dateformat(value: String) -> String {
    // Input is ISO 8601 like "2026-03-29T13:57:14.579090Z"
    chrono::DateTime::parse_from_rfc3339(&value)
        .map(|dt| dt.format("%d-%m-%Y").to_string())
        .unwrap_or_else(|_| value.chars().take(10).collect())
}

pub fn router(pool: PgPool) -> Router {
    let mut env = Environment::new();
    env.add_filter("truncate", truncate_filter);
    env.add_filter("dateformat", dateformat);
    env.add_template("base.html", include_str!("templates/base.html"))
        .unwrap();
    env.add_template("projects.html", include_str!("templates/projects.html"))
        .unwrap();
    env.add_template(
        "project_detail.html",
        include_str!("templates/project_detail.html"),
    )
    .unwrap();
    env.add_template(
        "task_detail.html",
        include_str!("templates/task_detail.html"),
    )
    .unwrap();
    env.add_template("rules.html", include_str!("templates/rules.html"))
        .unwrap();
    env.add_template("analytics.html", include_str!("templates/analytics.html"))
        .unwrap();

    let state = DashboardState {
        pool,
        env: Arc::new(env),
    };

    Router::new()
        .route("/", get(projects_page))
        .route("/projects/{id}", get(project_detail))
        .route(
            "/tasks/{id}",
            get(task_detail)
                .delete(delete_task_handler)
                .patch(update_task_handler),
        )
        .route("/tasks", post(create_task_handler))
        .route("/tasks/{id}/complete", post(complete_task))
        .route("/tasks/{id}/abandon", post(abandon_task))
        .route("/batch/tasks/delete", post(batch_delete_tasks))
        .route("/batch/tasks/status", post(batch_update_tasks_status))
        .route("/batch/rules/delete", post(batch_delete_rules))
        .route("/rules", get(rules_page))
        .route("/rules/{id}", delete(delete_rule))
        .route("/analytics", get(analytics_page))
        .with_state(state)
}

pub async fn serve(pool: PgPool, port: u16) {
    let app = router(pool);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(port, error = %e, "Dashboard failed to bind — port in use?");
            return;
        }
    };
    tracing::info!(port, "Dashboard server started");
    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!(error = %e, "Dashboard server error");
    }
}

fn render(
    env: &Environment,
    template: &str,
    ctx: minijinja::Value,
) -> Result<Html<String>, StatusCode> {
    let tmpl = env.get_template(template).map_err(|e| {
        tracing::error!(error = %e, "Template not found");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let html = tmpl.render(ctx).map_err(|e| {
        tracing::error!(error = %e, "Template render error");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Html(html))
}

/// Build project_id -> project_name lookup
async fn project_name_map(pool: &PgPool) -> HashMap<uuid::Uuid, String> {
    db::projects::list_projects(pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.id, p.name))
        .collect()
}

async fn projects_page(State(state): State<DashboardState>) -> Result<Html<String>, StatusCode> {
    let projects = db::projects::list_projects(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    render(
        &state.env,
        "projects.html",
        context! { projects => projects },
    )
}

#[derive(serde::Deserialize)]
pub struct ProjectDetailQuery {
    status: Option<String>,
    priority: Option<String>,
    task_type: Option<String>,
    page: Option<i64>,
    per_page: Option<i64>,
    sort: Option<String>,
    dir: Option<String>,
}

async fn project_detail(
    State(state): State<DashboardState>,
    Path(id): Path<uuid::Uuid>,
    Query(q): Query<ProjectDetailQuery>,
) -> Result<Html<String>, StatusCode> {
    let project = db::projects::get_project(&state.pool, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Default to active when no status param; empty string = explicit "all"
    let (status_filter, resolved_status) = match q.status.as_deref() {
        None => (Some(db::TaskStatus::Active), "active".to_string()),
        Some("active") => (Some(db::TaskStatus::Active), "active".to_string()),
        Some("completed") => (Some(db::TaskStatus::Completed), "completed".to_string()),
        Some("abandoned") => (Some(db::TaskStatus::Abandoned), "abandoned".to_string()),
        Some("blocked") => (Some(db::TaskStatus::Blocked), "blocked".to_string()),
        Some(_) => (None, "".to_string()),
    };

    let per_page = q.per_page.unwrap_or(50).clamp(1, 200);
    let page = q.page.unwrap_or(1).max(1);
    let offset = (page - 1) * per_page;
    let sort_col = q.sort.as_deref().unwrap_or("created_at");
    let sort_dir = q.dir.as_deref().unwrap_or("desc");

    let total = db::tasks::count_tasks(&state.pool, id, status_filter.clone())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total_pages = (total + per_page - 1) / per_page;

    let mut tasks = db::tasks::list_tasks_paginated(
        &state.pool,
        id,
        status_filter,
        sort_col,
        sort_dir,
        per_page,
        offset,
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // In-memory filtering for priority and task_type (pending DB-level filter)
    if let Some(ref p) = q.priority {
        tasks.retain(|t| t.priority.as_deref() == Some(p.as_str()));
    }
    if let Some(ref tt) = q.task_type {
        tasks.retain(|t| t.task_type.as_deref() == Some(tt.as_str()));
    }

    // Collect distinct values for filter dropdowns
    let all_tasks = db::tasks::list_tasks(&state.pool, id, None)
        .await
        .unwrap_or_default();
    let priorities: Vec<String> = {
        let mut v: Vec<String> = all_tasks
            .iter()
            .filter_map(|t| t.priority.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    };
    let task_types: Vec<String> = {
        let mut v: Vec<String> = all_tasks
            .iter()
            .filter_map(|t| t.task_type.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    };

    render(
        &state.env,
        "project_detail.html",
        context! {
            project => project,
            tasks => tasks,
            priorities => priorities,
            task_types => task_types,
            current_status => resolved_status,
            current_priority => q.priority,
            current_task_type => q.task_type,
            page => page,
            per_page => per_page,
            total_pages => total_pages,
            total => total,
            sort => sort_col,
            dir => sort_dir,
        },
    )
}

async fn task_detail(
    State(state): State<DashboardState>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Html<String>, StatusCode> {
    let task = db::tasks::get_task(&state.pool, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let attempts = db::attempts::list_attempts(&state.pool, id, None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let snapshots = db::snapshots::list_snapshots(&state.pool, id)
        .await
        .unwrap_or_default();
    let subtasks = db::tasks::list_subtasks(&state.pool, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let parent = match task.parent_task_id {
        Some(pid) => db::tasks::get_task(&state.pool, pid)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        None => None,
    };
    // All tasks in same project for parent-linking dropdown (exclude self and own subtasks)
    let all_tasks = db::tasks::list_tasks(&state.pool, task.project_id, None)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|t| t.id != task.id && t.parent_task_id != Some(task.id))
        .collect::<Vec<_>>();
    render(
        &state.env,
        "task_detail.html",
        context! { task => task, attempts => attempts, snapshots => snapshots, subtasks => subtasks, parent => parent, all_tasks => all_tasks },
    )
}

async fn complete_task(
    State(state): State<DashboardState>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Html<String>, StatusCode> {
    db::tasks::complete_task(&state.pool, id, None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    db::tasks::try_rollup_parents(&state.pool, id).await;
    Ok(Html(
        r#"<span class="badge badge-completed">Completed</span>"#.to_string(),
    ))
}

async fn abandon_task(
    State(state): State<DashboardState>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Html<String>, StatusCode> {
    db::tasks::abandon_task(&state.pool, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    db::tasks::try_rollup_parents(&state.pool, id).await;
    Ok(Html(
        r#"<span class="badge badge-abandoned">Abandoned</span>"#.to_string(),
    ))
}

async fn delete_task_handler(
    State(state): State<DashboardState>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Html<String>, StatusCode> {
    db::tasks::delete_task(&state.pool, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Html(String::new()))
}

#[derive(serde::Deserialize)]
struct UpdateTaskPayload {
    priority: Option<String>,
    task_type: Option<String>,
    description: Option<String>,
    // "" = clear parent, valid UUID = set parent, absent = don't touch
    parent_task_id: Option<String>,
}

async fn update_task_handler(
    State(state): State<DashboardState>,
    Path(id): Path<uuid::Uuid>,
    Json(payload): Json<UpdateTaskPayload>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Treat empty string as "clear the field"
    let priority = payload.priority.map(|v| {
        let trimmed = v.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });
    let task_type = payload.task_type.map(|v| {
        let trimmed = v.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });
    let description = payload
        .description
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());

    let parent_task_id: Option<Option<uuid::Uuid>> = payload.parent_task_id.map(|v| {
        let trimmed = v.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            uuid::Uuid::parse_str(&trimmed).ok()
        }
    });

    let updated = db::tasks::update_task(
        &state.pool,
        id,
        priority.as_ref().map(|o| o.as_deref()),
        task_type.as_ref().map(|o| o.as_deref()),
        description.as_deref(),
        None,
        parent_task_id,
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(serde_json::json!({ "updated": updated })))
}

#[derive(serde::Deserialize)]
struct CreateTaskPayload {
    project_id: uuid::Uuid,
    description: String,
    priority: Option<String>,
    task_type: Option<String>,
    parent_task_id: Option<String>,
}

async fn create_task_handler(
    State(state): State<DashboardState>,
    Json(payload): Json<CreateTaskPayload>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let desc = payload.description.trim();
    if desc.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let priority = payload
        .priority
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let task_type = payload
        .task_type
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let parent_id = payload
        .parent_task_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|v| uuid::Uuid::parse_str(v).ok());

    let id = db::tasks::create_task(
        &state.pool,
        payload.project_id,
        desc,
        parent_id,
        priority,
        task_type,
        None,
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(serde_json::json!({ "id": id })))
}

// --- Batch operations ---

#[derive(serde::Deserialize)]
struct BatchIdsPayload {
    ids: Vec<uuid::Uuid>,
}

#[derive(serde::Deserialize)]
struct BatchStatusPayload {
    ids: Vec<uuid::Uuid>,
    status: String,
}

async fn batch_delete_tasks(
    State(state): State<DashboardState>,
    Json(payload): Json<BatchIdsPayload>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let count = db::tasks::batch_delete_tasks(&state.pool, &payload.ids)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "deleted": count })))
}

async fn batch_update_tasks_status(
    State(state): State<DashboardState>,
    Json(payload): Json<BatchStatusPayload>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let status = match payload.status.as_str() {
        "active" => db::TaskStatus::Active,
        "completed" => db::TaskStatus::Completed,
        "abandoned" => db::TaskStatus::Abandoned,
        "blocked" => db::TaskStatus::Blocked,
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let count = db::tasks::batch_update_task_status(&state.pool, &payload.ids, status)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "updated": count })))
}

async fn batch_delete_rules(
    State(state): State<DashboardState>,
    Json(payload): Json<BatchIdsPayload>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let count = db::semantic::batch_delete_rules(&state.pool, &payload.ids)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "deleted": count })))
}

// --- Rules ---

#[derive(serde::Deserialize)]
pub struct RulesQuery {
    project_id: Option<uuid::Uuid>,
}

async fn rules_page(
    State(state): State<DashboardState>,
    Query(q): Query<RulesQuery>,
) -> Result<Html<String>, StatusCode> {
    let projects = db::projects::list_projects(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let pnames = project_name_map(&state.pool).await;

    let rules = if let Some(pid) = q.project_id {
        db::semantic::list_rules(&state.pool, pid, None, None)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        let mut all = Vec::new();
        for p in &projects {
            let mut r = db::semantic::list_rules(&state.pool, p.id, None, None)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            all.append(&mut r);
        }
        all
    };

    #[derive(serde::Serialize)]
    struct RuleView {
        id: uuid::Uuid,
        category: db::RuleCategory,
        content: String,
        project_name: String,
    }
    let rule_views: Vec<RuleView> = rules
        .iter()
        .map(|r| RuleView {
            id: r.id,
            category: r.category.clone(),
            content: r.content.clone(),
            project_name: pnames
                .get(&r.project_id)
                .cloned()
                .unwrap_or_else(|| r.project_id.to_string()),
        })
        .collect();

    render(
        &state.env,
        "rules.html",
        context! { rules => rule_views, projects => projects },
    )
}

async fn delete_rule(
    State(state): State<DashboardState>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Html<String>, StatusCode> {
    db::semantic::delete_rule(&state.pool, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Html(String::new()))
}

async fn analytics_page(State(state): State<DashboardState>) -> Result<Html<String>, StatusCode> {
    let projects = db::projects::list_projects(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut total_tasks: usize = 0;
    let mut completed_tasks: usize = 0;
    let mut active_tasks: usize = 0;
    let mut total_attempts: usize = 0;
    let mut total_rejected: usize = 0;
    let mut total_accepted: usize = 0;
    let mut total_rules: usize = 0;
    let mut first_try_success: usize = 0;
    let mut total_rule_bytes: usize = 0;
    let mut total_attempt_bytes: usize = 0;
    let mut total_context_wipes: usize = 0;

    #[derive(serde::Serialize)]
    struct ProjectStats {
        id: uuid::Uuid,
        name: String,
        task_count: usize,
        completed_count: usize,
        active_count: usize,
        attempt_count: usize,
        rejected_count: usize,
        rule_count: usize,
        first_try_count: usize,
        knowledge_bytes: usize,
    }

    let mut project_stats = Vec::new();
    for p in &projects {
        let tasks = db::tasks::list_tasks(&state.pool, p.id, None)
            .await
            .unwrap_or_default();
        let active = tasks
            .iter()
            .filter(|t| t.status == db::TaskStatus::Active)
            .count();
        let completed = tasks
            .iter()
            .filter(|t| t.status == db::TaskStatus::Completed)
            .count();
        let rules = db::semantic::list_rules(&state.pool, p.id, None, None)
            .await
            .unwrap_or_default();

        let p_rule_bytes: usize = rules.iter().map(|r| r.content.len()).sum();

        let mut p_attempts: usize = 0;
        let mut p_rejected: usize = 0;
        let mut p_accepted: usize = 0;
        let mut p_first_try: usize = 0;
        let mut p_attempt_bytes: usize = 0;
        for t in &tasks {
            let attempts = db::attempts::list_attempts(&state.pool, t.id, None)
                .await
                .unwrap_or_default();
            let t_rejected = attempts
                .iter()
                .filter(|a| a.outcome == db::AttemptOutcome::Rejected)
                .count();
            let t_accepted = attempts
                .iter()
                .filter(|a| a.outcome == db::AttemptOutcome::Accepted)
                .count();
            if t.status == db::TaskStatus::Completed && t_rejected == 0 && t_accepted > 0 {
                p_first_try += 1;
            }
            for a in &attempts {
                p_attempt_bytes += a.approach_summary.len()
                    + a.reasoning.len()
                    + a.code_snippet.as_ref().map_or(0, |c| c.len());
            }
            p_rejected += t_rejected;
            p_accepted += t_accepted;
            p_attempts += attempts.len();

            let wipes = db::snapshots::count_snapshots(&state.pool, t.id)
                .await
                .unwrap_or(0) as usize;
            total_context_wipes += wipes;
        }

        total_tasks += tasks.len();
        completed_tasks += completed;
        active_tasks += active;
        total_attempts += p_attempts;
        total_rejected += p_rejected;
        total_accepted += p_accepted;
        total_rules += rules.len();
        first_try_success += p_first_try;
        total_rule_bytes += p_rule_bytes;
        total_attempt_bytes += p_attempt_bytes;

        project_stats.push(ProjectStats {
            id: p.id,
            name: p.name.clone(),
            task_count: tasks.len(),
            completed_count: completed,
            active_count: active,
            attempt_count: p_attempts,
            rejected_count: p_rejected,
            rule_count: rules.len(),
            first_try_count: p_first_try,
            knowledge_bytes: p_rule_bytes + p_attempt_bytes,
        });
    }

    let rejection_rate = if total_attempts > 0 {
        (total_rejected * 100) / total_attempts
    } else {
        0
    };
    let first_try_rate = if completed_tasks > 0 {
        (first_try_success * 100) / completed_tasks
    } else {
        0
    };
    let knowledge_tokens = (total_rule_bytes + total_attempt_bytes) / 3;
    let avg_attempts_per_task = if completed_tasks > 0 {
        format!("{:.1}", total_attempts as f64 / completed_tasks as f64)
    } else {
        "—".to_string()
    };

    // Followups = completed tasks that spawned at least one subtask AFTER completion.
    // Captures the "log follow-ups after merge/complete_task" workflow; a count
    // matching `completed_tasks` means every completion begot at least one
    // deferred review task.
    let followup_parents: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT parent.id) \
         FROM ai_memory.tasks parent \
         JOIN ai_memory.tasks child ON child.parent_task_id = parent.id \
         WHERE parent.status = 'completed' \
           AND parent.completed_at IS NOT NULL \
           AND child.created_at > parent.completed_at",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);
    let followups_captured = followup_parents as usize;
    let followups_rate = if completed_tasks > 0 {
        (followups_captured * 100) / completed_tasks
    } else {
        0
    };

    render(
        &state.env,
        "analytics.html",
        context! {
            total_projects => projects.len(),
            total_tasks => total_tasks,
            completed_tasks => completed_tasks,
            active_tasks => active_tasks,
            total_attempts => total_attempts,
            total_accepted => total_accepted,
            total_rejected => total_rejected,
            total_rules => total_rules,
            rejection_rate => rejection_rate,
            first_try_success => first_try_success,
            first_try_rate => first_try_rate,
            knowledge_tokens => knowledge_tokens,
            total_rule_bytes => total_rule_bytes,
            total_attempt_bytes => total_attempt_bytes,
            context_wipes => total_context_wipes,
            avg_attempts_per_task => avg_attempts_per_task,
            followups_captured => followups_captured,
            followups_rate => followups_rate,
            projects => project_stats,
        },
    )
}
