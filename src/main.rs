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

    // Initialize logging before Config::from_env() so config parse warnings are captured.
    // Read LOG_LEVEL directly to break the circular dependency.
    let log_level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(&log_level)),
        )
        .init();

    let config = Config::from_env();

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

    // Wait for shutdown signal (SIGINT or SIGTERM)
    shutdown_signal().await;

    tracing::info!("Shutting down Lore MCP server");
}

/// Waits for either SIGINT (ctrl-c) or SIGTERM for graceful shutdown.
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("Failed to register SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {},
            _ = sigterm.recv() => {},
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.expect("Failed to listen for ctrl-c");
    }
}
