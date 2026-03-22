use std::env;

/// Application configuration parsed from environment variables.
///
/// All fields have sensible defaults matching `.env.example` so the server
/// can start with minimal configuration. Only `DATABASE_URL` is strictly
/// required at runtime (though it has a default for local development).
#[derive(Debug, Clone)]
pub struct Config {
    // Database
    pub database_url: String,
    pub database_max_connections: u32,
    pub database_statement_timeout_secs: u64,

    // Embeddings
    pub embedding_provider: String,
    pub openai_api_key: Option<String>,
    pub embedding_model: String,
    pub embedding_dimensions: usize,

    // Server
    pub mcp_transport: String,
    pub mcp_sse_port: u16,
    pub log_level: String,

    // Retention
    pub retention_attempts_days: u32,
    pub retention_snapshots_days: u32,
    pub retention_tasks_archive_days: u32,

    // Project
    pub default_project_name: String,
}

impl Config {
    /// Parse configuration from environment variables.
    ///
    /// Assumes `dotenvy::dotenv()` has already been called so `.env` values
    /// are available in the process environment.
    pub fn from_env() -> Self {
        Self {
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://lore:password@localhost:5432/ai_memory".into()),
            database_max_connections: parse_or("DATABASE_MAX_CONNECTIONS", 10),
            database_statement_timeout_secs: parse_or("DATABASE_STATEMENT_TIMEOUT_SECS", 5),

            embedding_provider: env::var("EMBEDDING_PROVIDER")
                .unwrap_or_else(|_| "local".into()),
            openai_api_key: env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty()),
            embedding_model: env::var("EMBEDDING_MODEL")
                .unwrap_or_else(|_| "all-MiniLM-L6-v2".into()),
            embedding_dimensions: parse_or("EMBEDDING_DIMENSIONS", 384),

            mcp_transport: env::var("MCP_TRANSPORT").unwrap_or_else(|_| "stdio".into()),
            mcp_sse_port: parse_or("MCP_SSE_PORT", 3100),
            log_level: env::var("LOG_LEVEL").unwrap_or_else(|_| "info".into()),

            retention_attempts_days: parse_or("RETENTION_ATTEMPTS_DAYS", 30),
            retention_snapshots_days: parse_or("RETENTION_SNAPSHOTS_DAYS", 7),
            retention_tasks_archive_days: parse_or("RETENTION_TASKS_ARCHIVE_DAYS", 90),

            default_project_name: env::var("DEFAULT_PROJECT_NAME")
                .unwrap_or_else(|_| "default".into()),
        }
    }
}

/// Helper to parse an env var into a numeric type, falling back to a default.
fn parse_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
