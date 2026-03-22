use std::time::Duration;
use sqlx::PgPool;
use crate::config::Config;

pub async fn run_retention_loop(pool: PgPool, config: Config) {
    // Defer first tick to avoid DB load at startup
    let start = tokio::time::Instant::now() + Duration::from_secs(3600);
    let mut interval = tokio::time::interval_at(start, Duration::from_secs(3600));
    loop {
        interval.tick().await;
        tracing::info!("Running retention cleanup");

        match prune_old_attempts(&pool, config.retention_attempts_days).await {
            Ok(n) => tracing::info!(rows = n, "Pruned old attempts"),
            Err(e) => tracing::warn!(error = %e, "Failed to prune old attempts"),
        }
        match prune_old_snapshots(&pool, config.retention_snapshots_days).await {
            Ok(n) => tracing::info!(rows = n, "Pruned old snapshots"),
            Err(e) => tracing::warn!(error = %e, "Failed to prune old snapshots"),
        }
        match purge_completed_tasks(&pool, config.retention_tasks_archive_days).await {
            Ok(n) => tracing::info!(rows = n, "Purged completed tasks"),
            Err(e) => tracing::warn!(error = %e, "Failed to purge completed tasks"),
        }
    }
}

pub async fn prune_old_attempts(pool: &PgPool, days: u32) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM ai_memory.attempts WHERE created_at < NOW() - make_interval(days => $1)",
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
