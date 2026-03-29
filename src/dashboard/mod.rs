use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Html,
    routing::{delete, get, post},
    Router,
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

pub fn router(pool: PgPool) -> Router {
    let mut env = Environment::new();
    env.add_filter("truncate", truncate_filter);
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
        .route("/tasks/{id}", get(task_detail))
        .route("/tasks/{id}/complete", post(complete_task))
        .route("/tasks/{id}/abandon", post(abandon_task))
        .route("/rules", get(rules_page))
        .route("/rules/{id}", delete(delete_rule))
        .route("/analytics", get(analytics_page))
        .with_state(state)
}

pub async fn serve(pool: PgPool, port: u16) {
    let app = router(pool);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    tracing::info!(port, "Dashboard server started");
    axum::serve(listener, app).await.unwrap();
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

async fn project_detail(
    State(state): State<DashboardState>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Html<String>, StatusCode> {
    let project = db::projects::get_project(&state.pool, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let tasks = db::tasks::list_tasks(&state.pool, id, None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    render(
        &state.env,
        "project_detail.html",
        context! { project => project, tasks => tasks },
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
    render(
        &state.env,
        "task_detail.html",
        context! { task => task, attempts => attempts },
    )
}

async fn complete_task(
    State(state): State<DashboardState>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Html<String>, StatusCode> {
    db::tasks::complete_task(&state.pool, id, None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
    Ok(Html(
        r#"<span class="badge badge-abandoned">Abandoned</span>"#.to_string(),
    ))
}

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

    let rules = if let Some(pid) = q.project_id {
        db::semantic::list_rules(&state.pool, pid, None)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        let mut all = Vec::new();
        for p in &projects {
            let mut r = db::semantic::list_rules(&state.pool, p.id, None)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            all.append(&mut r);
        }
        all
    };

    render(
        &state.env,
        "rules.html",
        context! { rules => rules, projects => projects },
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
    let mut active_tasks: usize = 0;
    let mut total_attempts: usize = 0;
    let mut total_rejected: usize = 0;
    let mut total_rules: usize = 0;

    #[derive(serde::Serialize)]
    struct ProjectStats {
        id: uuid::Uuid,
        name: String,
        task_count: usize,
        active_count: usize,
        attempt_count: usize,
        rule_count: usize,
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
        let rules = db::semantic::list_rules(&state.pool, p.id, None)
            .await
            .unwrap_or_default();

        let mut p_attempts: usize = 0;
        let mut p_rejected: usize = 0;
        for t in &tasks {
            let attempts = db::attempts::list_attempts(&state.pool, t.id, None)
                .await
                .unwrap_or_default();
            p_rejected += attempts
                .iter()
                .filter(|a| a.outcome == db::AttemptOutcome::Rejected)
                .count();
            p_attempts += attempts.len();
        }

        total_tasks += tasks.len();
        active_tasks += active;
        total_attempts += p_attempts;
        total_rejected += p_rejected;
        total_rules += rules.len();

        project_stats.push(ProjectStats {
            id: p.id,
            name: p.name.clone(),
            task_count: tasks.len(),
            active_count: active,
            attempt_count: p_attempts,
            rule_count: rules.len(),
        });
    }

    let rejection_rate = if total_attempts > 0 {
        (total_rejected * 100) / total_attempts
    } else {
        0
    };

    render(
        &state.env,
        "analytics.html",
        context! {
            total_projects => projects.len(),
            total_tasks => total_tasks,
            active_tasks => active_tasks,
            total_attempts => total_attempts,
            total_rules => total_rules,
            rejection_rate => rejection_rate,
            projects => project_stats,
        },
    )
}
