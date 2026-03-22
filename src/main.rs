mod config;
mod db;
mod embeddings;
mod server;
mod tools;

use config::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    // Load .env file (silently ignore if missing -- production may use real env vars)
    let _ = dotenvy::dotenv();

    let config = Config::from_env();

    // Initialize structured logging with level from config
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(&config.log_level)),
        )
        .init();

    tracing::info!("Starting Lore MCP server");

    let _pool = match db::create_pool(&config).await {
        Ok(pool) => {
            tracing::info!("Lore MCP server initialized");
            pool
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to initialize database pool");
            std::process::exit(1);
        }
    };

    // Keep the server alive until interrupted
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for ctrl-c");

    tracing::info!("Shutting down Lore MCP server");
}
