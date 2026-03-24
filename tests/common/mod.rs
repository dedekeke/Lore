use std::time::Duration;

use sqlx::PgPool;
use testcontainers::core::IntoContainerPort;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

pub async fn setup_db() -> (PgPool, ContainerAsync<GenericImage>) {
    let container: ContainerAsync<GenericImage> = GenericImage::new("pgvector/pgvector", "pg17")
        .with_exposed_port(5432.tcp())
        .with_env_var("POSTGRES_DB", "lore_test")
        .with_env_var("POSTGRES_USER", "lore")
        .with_env_var("POSTGRES_PASSWORD", "test")
        .start()
        .await
        .expect("Failed to start pgvector container");

    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get container port");

    let url = format!("postgres://lore:test@127.0.0.1:{port}/lore_test");

    // Retry connection — Postgres emits "ready" during init before the final restart
    let pool = retry_connect(&url, 30).await;

    sqlx::migrate!()
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (pool, container)
}

async fn retry_connect(url: &str, max_attempts: u32) -> PgPool {
    for i in 1..=max_attempts {
        match sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(2))
            .connect(url)
            .await
        {
            Ok(pool) => {
                // Verify the connection is actually usable
                if sqlx::query("SELECT 1").execute(&pool).await.is_ok() {
                    return pool;
                }
            }
            Err(_) if i < max_attempts => {}
            Err(e) => panic!("Failed to connect after {max_attempts} attempts: {e}"),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("Failed to connect after {max_attempts} attempts");
}
