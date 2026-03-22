use sqlx::PgPool;

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

// TODO: implement proper archival (move to archive table) in Phase 7
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
