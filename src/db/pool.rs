use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use crate::config::Config;

/// Creates a PostgreSQL connection pool and runs pending migrations.
///
/// The pool is configured with the max connections from `Config`. Migrations
/// are embedded at compile time via `sqlx::migrate!()` so the binary is
/// self-contained -- no external migration files needed at runtime.
pub async fn create_pool(config: &Config) -> Result<PgPool, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .max_connections(config.database_max_connections)
        .connect(&config.database_url)
        .await?;

    tracing::info!("Running database migrations");
    sqlx::migrate!().run(&pool).await?;
    tracing::info!("Database migrations complete");

    Ok(pool)
}
