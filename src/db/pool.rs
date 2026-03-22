use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use crate::config::Config;

/// Creates a PostgreSQL connection pool and runs pending migrations.
///
/// The pool is configured with the max connections from `Config`. Each new
/// connection sets `statement_timeout` to prevent runaway queries. Migrations
/// are embedded at compile time via `sqlx::migrate!()` so the binary is
/// self-contained -- no external migration files needed at runtime.
pub async fn create_pool(config: &Config) -> Result<PgPool, sqlx::Error> {
    let timeout_ms = config.database_statement_timeout_secs * 1000;

    let pool = PgPoolOptions::new()
        .max_connections(config.database_max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .after_connect(move |conn, _meta| {
            Box::pin(async move {
                sqlx::query(&format!("SET statement_timeout = '{timeout_ms}ms'"))
                    .execute(&mut *conn)
                    .await
                    .map(|_| ())
            })
        })
        .connect(&config.database_url)
        .await?;

    tracing::info!("Running database migrations");
    sqlx::migrate!().run(&pool).await?;
    tracing::info!("Database migrations complete");

    Ok(pool)
}
