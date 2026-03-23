use sqlx::PgPool;
use testcontainers::core::IntoContainerPort;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

pub async fn setup_db() -> (PgPool, ContainerAsync<GenericImage>) {
    let container = GenericImage::new("pgvector/pgvector", "pg17")
        .with_exposed_port(5432.tcp())
        .with_wait_for(testcontainers::core::WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
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

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&url)
        .await
        .expect("Failed to connect to test DB");

    sqlx::migrate!()
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (pool, container)
}
