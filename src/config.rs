use std::collections::HashSet;
use std::env;
use std::fmt;

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

#[derive(Debug, Clone, PartialEq)]
pub enum EmbeddingProvider {
    Local,
    Gemini,
}

impl fmt::Display for EmbeddingProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Gemini => write!(f, "gemini"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub database_max_connections: u32,
    pub database_statement_timeout_secs: u64,

    pub embedding_provider: EmbeddingProvider,
    pub gemini_api_key: Option<String>,
    pub embedding_model: String,
    pub embedding_dimensions: usize,

    pub mcp_transport: McpTransport,
    pub mcp_sse_port: u16,
    pub log_level: String,

    pub retention_attempts_days: u32,
    pub retention_snapshots_days: u32,
    pub retention_tasks_archive_days: u32,
    pub retention_unknown_days: u32,
    pub retention_pending_escalation_hours: u32,
    pub decay_after_days: u32,
    pub decay_min_accepted: i64,

    pub default_project_name: String,
    pub disabled_tools: HashSet<String>,
}

impl Config {
    pub fn from_env() -> Self {
        let embedding_provider = match env::var("EMBEDDING_PROVIDER")
            .unwrap_or_else(|_| "local".into())
            .to_lowercase()
            .as_str()
        {
            "local" => EmbeddingProvider::Local,
            "gemini" => EmbeddingProvider::Gemini,
            other => {
                tracing::warn!(
                    value = other,
                    "Invalid EMBEDDING_PROVIDER, defaulting to 'local'"
                );
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
                tracing::warn!(
                    value = other,
                    "Invalid MCP_TRANSPORT, defaulting to 'stdio'"
                );
                McpTransport::Stdio
            }
        };

        Self {
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://lore:password@localhost:5432/ai_memory".into()),
            database_max_connections: parse_warn_or("DATABASE_MAX_CONNECTIONS", 10),
            database_statement_timeout_secs: parse_warn_or("DATABASE_STATEMENT_TIMEOUT_SECS", 5),

            embedding_provider,
            gemini_api_key: env::var("GEMINI_API_KEY").ok().filter(|s| !s.is_empty()),
            embedding_model: env::var("EMBEDDING_MODEL")
                .unwrap_or_else(|_| "all-MiniLM-L6-v2".into()),
            embedding_dimensions: parse_warn_or("EMBEDDING_DIMENSIONS", 384),

            mcp_transport,
            mcp_sse_port: parse_warn_or("MCP_SSE_PORT", 3100),
            log_level: env::var("LOG_LEVEL").unwrap_or_else(|_| "info".into()),

            retention_attempts_days: parse_warn_or("RETENTION_ATTEMPTS_DAYS", 30),
            retention_snapshots_days: parse_warn_or("RETENTION_SNAPSHOTS_DAYS", 7),
            retention_tasks_archive_days: parse_warn_or("RETENTION_TASKS_ARCHIVE_DAYS", 90),
            retention_unknown_days: parse_warn_or("RETENTION_UNKNOWN_DAYS", 7),
            retention_pending_escalation_hours: parse_warn_or(
                "RETENTION_PENDING_ESCALATION_HOURS",
                72,
            ),
            decay_after_days: parse_warn_or("DECAY_AFTER_DAYS", 14),
            decay_min_accepted: parse_warn_or("DECAY_MIN_ACCEPTED", 2),

            default_project_name: env::var("DEFAULT_PROJECT_NAME")
                .unwrap_or_else(|_| "default".into()),

            disabled_tools: env::var("DISABLED_TOOLS")
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        }
    }
}

pub fn parse_warn_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    match env::var(key) {
        Ok(v) => match v.parse() {
            Ok(parsed) => parsed,
            Err(_) => {
                tracing::warn!(
                    key = key,
                    value = v,
                    "Invalid value for env var, using default"
                );
                default
            }
        },
        Err(_) => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // env vars are process-global — serialize config tests
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_env() {
        for key in [
            "DATABASE_URL",
            "DATABASE_MAX_CONNECTIONS",
            "DATABASE_STATEMENT_TIMEOUT_SECS",
            "EMBEDDING_PROVIDER",
            "GEMINI_API_KEY",
            "EMBEDDING_MODEL",
            "EMBEDDING_DIMENSIONS",
            "MCP_TRANSPORT",
            "MCP_SSE_PORT",
            "LOG_LEVEL",
            "RETENTION_ATTEMPTS_DAYS",
            "RETENTION_SNAPSHOTS_DAYS",
            "RETENTION_TASKS_ARCHIVE_DAYS",
            "RETENTION_UNKNOWN_DAYS",
            "RETENTION_PENDING_ESCALATION_HOURS",
            "DECAY_AFTER_DAYS",
            "DECAY_MIN_ACCEPTED",
            "DEFAULT_PROJECT_NAME",
            "DISABLED_TOOLS",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn test_defaults() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();

        let cfg = Config::from_env();
        assert_eq!(cfg.embedding_provider, EmbeddingProvider::Local);
        assert_eq!(cfg.embedding_dimensions, 384);
        assert_eq!(cfg.mcp_transport, McpTransport::Stdio);
        assert_eq!(cfg.database_max_connections, 10);
        assert_eq!(cfg.retention_attempts_days, 30);
        assert_eq!(cfg.default_project_name, "default");
        assert!(cfg.disabled_tools.is_empty());
    }

    #[test]
    fn test_gemini_provider_parsing() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("EMBEDDING_PROVIDER", "gemini");

        let cfg = Config::from_env();
        assert_eq!(cfg.embedding_provider, EmbeddingProvider::Gemini);
    }

    #[test]
    fn test_invalid_provider_falls_back() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("EMBEDDING_PROVIDER", "garbage");

        let cfg = Config::from_env();
        assert_eq!(cfg.embedding_provider, EmbeddingProvider::Local);
    }

    #[test]
    fn test_invalid_numeric_falls_back() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("DATABASE_MAX_CONNECTIONS", "notanumber");

        let cfg = Config::from_env();
        assert_eq!(cfg.database_max_connections, 10);
    }

    #[test]
    fn test_empty_gemini_key_is_none() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("GEMINI_API_KEY", "");

        let cfg = Config::from_env();
        assert!(cfg.gemini_api_key.is_none());
    }

    #[test]
    fn test_disabled_tools_parsing() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var(
            "DISABLED_TOOLS",
            "forget_rule, export_memory , log_context_wipe",
        );

        let cfg = Config::from_env();
        assert_eq!(cfg.disabled_tools.len(), 3);
        assert!(cfg.disabled_tools.contains("forget_rule"));
        assert!(cfg.disabled_tools.contains("export_memory"));
        assert!(cfg.disabled_tools.contains("log_context_wipe"));
    }
}
