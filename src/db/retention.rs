use crate::config::Config;
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, sqlx::FromRow)]
struct EligibleTask {
    task_id: Uuid,
    project_id: Uuid,
    task_description: String,
}

#[derive(Debug, sqlx::FromRow)]
struct AttemptRow {
    task_id: Uuid,
    outcome: String,
    approach_summary: String,
    reasoning: Option<String>,
}

pub async fn run_retention_loop(pool: PgPool, config: Config) {
    // Defer first tick to avoid DB load at startup
    let start = tokio::time::Instant::now() + Duration::from_secs(3600);
    let mut interval = tokio::time::interval_at(start, Duration::from_secs(3600));
    loop {
        interval.tick().await;
        tracing::info!("Running retention cleanup");

        match escalate_stale_pending(&pool, config.retention_pending_escalation_hours).await {
            Ok(n) => {
                if n > 0 {
                    tracing::info!(rows = n, "Escalated stale pending attempts to unknown")
                }
            }
            Err(e) => tracing::warn!(error = %e, "Failed to escalate stale pending attempts"),
        }
        match prune_unknown_attempts(&pool, config.retention_unknown_days).await {
            Ok(n) => {
                if n > 0 {
                    tracing::info!(rows = n, "Pruned unknown attempts")
                }
            }
            Err(e) => tracing::warn!(error = %e, "Failed to prune unknown attempts"),
        }
        match prune_old_attempts(&pool, config.retention_attempts_days).await {
            Ok(n) => {
                if n > 0 {
                    tracing::info!(rows = n, "Pruned old attempts")
                }
            }
            Err(e) => tracing::warn!(error = %e, "Failed to prune old attempts"),
        }
        match prune_old_snapshots(&pool, config.retention_snapshots_days).await {
            Ok(n) => {
                if n > 0 {
                    tracing::info!(rows = n, "Pruned old snapshots")
                }
            }
            Err(e) => tracing::warn!(error = %e, "Failed to prune old snapshots"),
        }
        match purge_completed_tasks(&pool, config.retention_tasks_archive_days).await {
            Ok(n) => {
                if n > 0 {
                    tracing::info!(rows = n, "Purged completed tasks")
                }
            }
            Err(e) => tracing::warn!(error = %e, "Failed to purge completed tasks"),
        }
        match consolidate_old_attempts(&pool, config.decay_after_days, config.decay_min_accepted)
            .await
        {
            Ok(n) => {
                if n > 0 {
                    tracing::info!(
                        lessons = n,
                        "Consolidated old accepted attempts into lessons"
                    )
                }
            }
            Err(e) => tracing::warn!(error = %e, "Failed to consolidate old attempts"),
        }
    }
}

/// Escalate pending attempts older than N hours to 'unknown'
pub async fn escalate_stale_pending(pool: &PgPool, hours: u32) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE ai_memory.attempts \
         SET outcome = 'unknown', resolved_at = NOW() \
         WHERE outcome = 'pending' AND created_at < NOW() - make_interval(hours => $1)",
    )
    .bind(hours as i32)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Delete unknown attempts older than N days
pub async fn prune_unknown_attempts(pool: &PgPool, days: u32) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM ai_memory.attempts \
         WHERE outcome = 'unknown' AND resolved_at < NOW() - make_interval(days => $1)",
    )
    .bind(days as i32)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn prune_old_attempts(pool: &PgPool, days: u32) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM ai_memory.attempts \
         WHERE outcome != 'unknown' AND created_at < NOW() - make_interval(days => $1)",
    )
    .bind(days as i32)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn prune_old_snapshots(pool: &PgPool, days: u32) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM ai_memory.context_snapshots WHERE wiped_at < NOW() - make_interval(days => $1)",
    )
    .bind(days as i32)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Consolidate old attempts into a structured lesson per task, preserving failure narrative.
/// Only targets completed tasks with >= min_accepted accepted attempts older than N days.
pub async fn consolidate_old_attempts(
    pool: &PgPool,
    days: u32,
    min_accepted: i64,
) -> Result<u64, sqlx::Error> {
    let eligible: Vec<EligibleTask> = sqlx::query_as(
        "SELECT t.id AS task_id, t.project_id, t.description AS task_description \
         FROM ai_memory.tasks t \
         JOIN ai_memory.attempts a ON a.task_id = t.id \
         WHERE t.status = 'completed' \
         AND a.outcome = 'accepted' \
         AND a.resolved_at < NOW() - make_interval(days => $1) \
         GROUP BY t.id HAVING COUNT(a.id) >= $2",
    )
    .bind(days as i32)
    .bind(min_accepted)
    .fetch_all(pool)
    .await?;

    if eligible.is_empty() {
        return Ok(0);
    }

    let task_ids: Vec<Uuid> = eligible.iter().map(|t| t.task_id).collect();

    let attempts: Vec<AttemptRow> = sqlx::query_as(
        "SELECT task_id, outcome, approach_summary, reasoning \
         FROM ai_memory.attempts \
         WHERE task_id = ANY($1) AND outcome IN ('accepted', 'rejected') \
         ORDER BY created_at",
    )
    .bind(&task_ids)
    .fetch_all(pool)
    .await?;

    let mut created = 0u64;
    for task in &eligible {
        let task_attempts: Vec<&AttemptRow> = attempts
            .iter()
            .filter(|a| a.task_id == task.task_id)
            .collect();
        let rejected: Vec<&&AttemptRow> = task_attempts
            .iter()
            .filter(|a| a.outcome == "rejected")
            .collect();
        let accepted: Vec<&&AttemptRow> = task_attempts
            .iter()
            .filter(|a| a.outcome == "accepted")
            .collect();

        let mut lesson = format!("## Task: {}\n", task.task_description);

        if !rejected.is_empty() {
            lesson.push_str("### Rejected approaches:\n");
            for a in &rejected {
                let r = a.reasoning.as_deref().unwrap_or("");
                if r.is_empty() {
                    lesson.push_str(&format!("- {}\n", a.approach_summary));
                } else {
                    lesson.push_str(&format!("- {}: {}\n", a.approach_summary, r));
                }
            }
        }

        lesson.push_str("### Accepted approach:\n");
        for a in &accepted {
            let r = a.reasoning.as_deref().unwrap_or("");
            if r.is_empty() {
                lesson.push_str(&format!("- {}\n", a.approach_summary));
            } else {
                lesson.push_str(&format!("- {}: {}\n", a.approach_summary, r));
            }
        }

        let lesson: String = lesson.chars().take(4096).collect();

        // Create lesson rule (without embedding — batch embed will pick it up)
        sqlx::query(
            "INSERT INTO ai_memory.semantic_rules (project_id, category, content, source_task_id) \
             VALUES ($1, 'lesson', $2, $3)",
        )
        .bind(task.project_id)
        .bind(&lesson)
        .bind(task.task_id)
        .execute(pool)
        .await?;

        // Delete all consolidated attempts (both rejected and accepted)
        sqlx::query(
            "DELETE FROM ai_memory.attempts \
             WHERE task_id = $1 AND outcome IN ('accepted', 'rejected') \
             AND resolved_at < NOW() - make_interval(days => $2)",
        )
        .bind(task.task_id)
        .bind(days as i32)
        .execute(pool)
        .await?;

        created += 1;
    }
    Ok(created)
}

// TODO: implement proper archival (move to archive table) — deferred to post-MVP
pub async fn purge_completed_tasks(pool: &PgPool, days: u32) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM ai_memory.tasks \
         WHERE status = 'completed' AND completed_at < NOW() - make_interval(days => $1)",
    )
    .bind(days as i32)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}
