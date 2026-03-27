use lore::config::Config;
use lore::{db, embeddings, server};
use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();

    let log_level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&log_level)),
        )
        // MCP stdio transport uses stdout for JSON-RPC, so logs must go to stderr
        .with_writer(std::io::stderr)
        .init();

    let config = Config::from_env();
    tracing::info!("Starting Lore MCP server");

    let pool = match db::create_pool(&config).await {
        Ok(pool) => {
            tracing::info!("Database pool initialized");
            pool
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to initialize database pool");
            std::process::exit(1);
        }
    };

    let embeddings = match embeddings::create_provider(&config) {
        Ok(provider) => {
            tracing::info!("Embedding provider initialized");
            provider
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to initialize embedding provider");
            std::process::exit(1);
        }
    };

    // Background: batch backfill NULL embeddings
    if let Ok(batch_provider) = embeddings::create_provider(&config) {
        tokio::spawn(lore::batch_embed::backfill_embeddings(
            pool.clone(),
            batch_provider,
        ));
    }

    tokio::spawn(db::retention::run_retention_loop(
        pool.clone(),
        config.clone(),
    ));

    let server = server::LoreServer::new(pool, embeddings, config);
    let transport = rmcp::transport::io::stdio();

    tracing::info!("Lore MCP server listening on stdio");
    let handle = match server.serve(transport).await {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "Failed to start MCP transport");
            std::process::exit(1);
        }
    };

    if let Err(e) = handle.waiting().await {
        tracing::error!(error = %e, "MCP server terminated with error");
    }
    tracing::info!("Lore MCP server shut down");
}
