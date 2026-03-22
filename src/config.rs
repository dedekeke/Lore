use std::env;
use std::fmt;

/// MCP transport protocol.
#[derive(Debug, Clone, PartialEq)]
pub enum McpTransport {
    Stdio,
    Sse,
}

impl fmt::Display for McpTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdio => write!(f, "stdio"),
            Self::Sse => write!(f, "sse"),
        }
    }
}

/// Embedding provider backend.
#[derive(Debug, Clone, PartialEq)]
pub enum EmbeddingProvider {
    Local,
    OpenAi,
}

impl fmt::Display for EmbeddingProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::OpenAi => write!(f, "openai"),
        }
    }
}

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
    pub embedding_provider: EmbeddingProvider,
    pub openai_api_key: Option<String>,
    pub embedding_model: String,
    pub embedding_dimensions: usize,

    // Server
    pub mcp_transport: McpTransport,
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
        let embedding_provider = match env::var("EMBEDDING_PROVIDER")
            .unwrap_or_else(|_| "local".into())
            .to_lowercase()
            .as_str()
        {
            "local" => EmbeddingProvider::Local,
            "openai" => EmbeddingProvider::OpenAi,
            other => {
                tracing::warn!(value = other, "Invalid EMBEDDING_PROVIDER (expected 'local' or 'openai'), defaulting to 'local'");
                EmbeddingProvider::Local
            }
        };

        let mcp_transport = match env::var("MCP_TRANSPORT")
            .unwrap_or_else(|_| "stdio".into())
            .to_lowercase()
            .as_str()
        {
            "stdio" => McpTransport::Stdio,
            "sse" => McpTransport::Sse,
            other => {
                tracing::warn!(value = other, "Invalid MCP_TRANSPORT (expected 'stdio' or 'sse'), defaulting to 'stdio'");
                McpTransport::Stdio
            }
        };

        Self {
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://lore:password@localhost:5432/ai_memory".into()),
            database_max_connections: parse_warn_or("DATABASE_MAX_CONNECTIONS", 10),
            database_statement_timeout_secs: parse_warn_or("DATABASE_STATEMENT_TIMEOUT_SECS", 5),

            embedding_provider,
            openai_api_key: env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty()),
            embedding_model: env::var("EMBEDDING_MODEL")
                .unwrap_or_else(|_| "all-MiniLM-L6-v2".into()),
            embedding_dimensions: parse_warn_or("EMBEDDING_DIMENSIONS", 384),

            mcp_transport,
            mcp_sse_port: parse_warn_or("MCP_SSE_PORT", 3100),
            log_level: env::var("LOG_LEVEL").unwrap_or_else(|_| "info".into()),

            retention_attempts_days: parse_warn_or("RETENTION_ATTEMPTS_DAYS", 30),
            retention_snapshots_days: parse_warn_or("RETENTION_SNAPSHOTS_DAYS", 7),
            retention_tasks_archive_days: parse_warn_or("RETENTION_TASKS_ARCHIVE_DAYS", 90),

            default_project_name: env::var("DEFAULT_PROJECT_NAME")
                .unwrap_or_else(|_| "default".into()),
        }
    }
}

/// Helper to parse an env var into a numeric type, warning and falling back on failure.
fn parse_warn_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    match env::var(key) {
        Ok(v) => match v.parse() {
            Ok(parsed) => parsed,
            Err(_) => {
                tracing::warn!(key = key, value = v, "Invalid value for env var, using default");
                default
            }
        },
        Err(_) => default,
    }
}
