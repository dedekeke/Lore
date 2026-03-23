use std::sync::Arc;

use rmcp::{model::*, tool, ServerHandler};
use sqlx::PgPool;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::config::Config;
use crate::db;
use crate::embeddings::{AnyEmbeddingProvider, EmbeddingProvider};

#[derive(Clone)]
pub struct LoreServer {
    inner: Arc<LoreServerInner>,
}

pub struct LoreServerInner {
    pub pool: PgPool,
    pub embeddings: AnyEmbeddingProvider,
    pub config: Config,
    pub current_project_id: RwLock<Option<Uuid>>,
}

impl LoreServer {
    pub fn new(pool: PgPool, embeddings: AnyEmbeddingProvider, config: Config) -> Self {
        Self {
            inner: Arc::new(LoreServerInner {
                pool,
                embeddings,
                config,
                current_project_id: RwLock::new(None),
            }),
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.inner.pool
    }

    pub fn embeddings(&self) -> &AnyEmbeddingProvider {
        &self.inner.embeddings
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub async fn project_id(&self) -> Result<Uuid, rmcp::Error> {
        self.inner.current_project_id.read().await.ok_or_else(|| {
            rmcp::Error::invalid_params("No active project. Call switch_project first.", None)
        })
    }

    pub async fn set_project_id(&self, id: Uuid) {
        *self.inner.current_project_id.write().await = Some(id);
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, rmcp::Error> {
        self.embeddings()
            .embed(text)
            .await
            .map_err(|e| rmcp::Error::internal_error(format!("Embedding error: {e}"), None))
    }

    fn parse_rule_category(s: &str) -> Result<db::RuleCategory, rmcp::Error> {
        match s.to_lowercase().as_str() {
            "preference" => Ok(db::RuleCategory::Preference),
            "fact" => Ok(db::RuleCategory::Fact),
            "constraint" => Ok(db::RuleCategory::Constraint),
            "lesson" => Ok(db::RuleCategory::Lesson),
            other => Err(rmcp::Error::invalid_params(
                format!(
                    "Invalid rule category: '{other}'. Valid: preference, fact, constraint, lesson"
                ),
                None,
            )),
        }
    }

    fn parse_attempt_outcome(s: &str) -> Result<db::AttemptOutcome, rmcp::Error> {
        match s.to_lowercase().as_str() {
            "pending" => Ok(db::AttemptOutcome::Pending),
            "accepted" => Ok(db::AttemptOutcome::Accepted),
            "rejected" => Ok(db::AttemptOutcome::Rejected),
            other => Err(rmcp::Error::invalid_params(
                format!("Invalid outcome: '{other}'. Valid: pending, accepted, rejected"),
                None,
            )),
        }
    }

    fn parse_uuid(s: &str) -> Result<Uuid, rmcp::Error> {
        s.parse::<Uuid>()
            .map_err(|e| rmcp::Error::invalid_params(format!("Invalid UUID '{s}': {e}"), None))
    }

    fn db_err(e: sqlx::Error) -> rmcp::Error {
        rmcp::Error::internal_error(format!("Database error: {e}"), None)
    }

    fn json_content<T: serde::Serialize>(val: &T) -> Result<CallToolResult, rmcp::Error> {
        let json = serde_json::to_string_pretty(val)
            .map_err(|e| rmcp::Error::internal_error(format!("Serialization error: {e}"), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }
}

// -- Memory tools --
#[tool(tool_box)]
impl LoreServer {
    #[tool(description = "Store a long-term rule/preference/fact/lesson in memory")]
    pub async fn remember_rule(
        &self,
        #[tool(param)]
        #[schemars(description = "Rule category: preference, fact, constraint, or lesson")]
        category: String,
        #[tool(param)]
        #[schemars(description = "The rule content to remember")]
        content: String,
    ) -> Result<CallToolResult, rmcp::Error> {
        let project_id = self.project_id().await?;
        let cat = Self::parse_rule_category(&category)?;
        let embedding = self.embed(&content).await?;
        let id =
            db::semantic::create_rule(self.pool(), project_id, cat, &content, Some(&embedding))
                .await
                .map_err(Self::db_err)?;
        Self::json_content(&serde_json::json!({ "rule_id": id.to_string() }))
    }

    #[tool(description = "Recall rules from memory using semantic search")]
    pub async fn recall_rules(
        &self,
        #[tool(param)]
        #[schemars(description = "Search query")]
        query: String,
        #[tool(param)]
        #[schemars(description = "Max results (default 10)")]
        limit: Option<i64>,
        #[tool(param)]
        #[schemars(description = "Filter by category: preference, fact, constraint, or lesson")]
        category: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let project_id = self.project_id().await?;
        let embedding = self.embed(&query).await?;
        let cat = category
            .as_deref()
            .map(Self::parse_rule_category)
            .transpose()?;
        let rules = db::semantic::search_rules_by_embedding(
            self.pool(),
            project_id,
            &embedding,
            limit.unwrap_or(10),
            cat,
        )
        .await
        .map_err(Self::db_err)?;
        Self::json_content(&rules)
    }

    #[tool(description = "Delete a rule from memory")]
    pub async fn forget_rule(
        &self,
        #[tool(param)]
        #[schemars(description = "UUID of the rule to delete")]
        rule_id: String,
    ) -> Result<CallToolResult, rmcp::Error> {
        let id = Self::parse_uuid(&rule_id)?;
        let deleted = db::semantic::delete_rule(self.pool(), id)
            .await
            .map_err(Self::db_err)?;
        Self::json_content(&serde_json::json!({ "deleted": deleted }))
    }

    #[tool(description = "List all rules, optionally filtered by category")]
    pub async fn list_rules(
        &self,
        #[tool(param)]
        #[schemars(description = "Filter by category: preference, fact, constraint, or lesson")]
        category: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let project_id = self.project_id().await?;
        let cat = category
            .as_deref()
            .map(Self::parse_rule_category)
            .transpose()?;
        let rules = db::semantic::list_rules(self.pool(), project_id, cat)
            .await
            .map_err(Self::db_err)?;
        Self::json_content(&rules)
    }

    // -- Ledger tools --

    #[tool(description = "Start a new task in the episodic ledger")]
    pub async fn start_task(
        &self,
        #[tool(param)]
        #[schemars(description = "Description of the task")]
        description: String,
        #[tool(param)]
        #[schemars(description = "UUID of parent task, if this is a subtask")]
        parent_task_id: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let project_id = self.project_id().await?;
        let parent = parent_task_id
            .as_deref()
            .map(Self::parse_uuid)
            .transpose()?;
        let id = db::tasks::create_task(self.pool(), project_id, &description, parent)
            .await
            .map_err(Self::db_err)?;
        Self::json_content(&serde_json::json!({ "task_id": id.to_string() }))
    }

    #[tool(description = "Propose an approach attempt for a task")]
    pub async fn propose_attempt(
        &self,
        #[tool(param)]
        #[schemars(description = "UUID of the task")]
        task_id: String,
        #[tool(param)]
        #[schemars(description = "Summary of the approach being attempted")]
        approach_summary: String,
        #[tool(param)]
        #[schemars(description = "Optional code snippet for the attempt")]
        code_snippet: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let tid = Self::parse_uuid(&task_id)?;
        let id = db::attempts::create_attempt(
            self.pool(),
            tid,
            &approach_summary,
            code_snippet.as_deref(),
        )
        .await
        .map_err(Self::db_err)?;
        Self::json_content(&serde_json::json!({ "attempt_id": id.to_string() }))
    }

    #[tool(description = "Log the outcome of an attempt (accepted/rejected)")]
    pub async fn log_outcome(
        &self,
        #[tool(param)]
        #[schemars(description = "UUID of the attempt")]
        attempt_id: String,
        #[tool(param)]
        #[schemars(description = "Outcome: accepted or rejected")]
        outcome: String,
        #[tool(param)]
        #[schemars(description = "Reasoning for the outcome")]
        reasoning: String,
        #[tool(param)]
        #[schemars(description = "Optional git reference (commit hash, branch)")]
        git_ref: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let aid = Self::parse_uuid(&attempt_id)?;
        let out = Self::parse_attempt_outcome(&outcome)?;
        let embedding = self.embed(&reasoning).await?;
        let success = db::attempts::log_outcome(
            self.pool(),
            aid,
            out,
            &reasoning,
            Some(&embedding),
            git_ref.as_deref(),
        )
        .await
        .map_err(Self::db_err)?;
        Self::json_content(&serde_json::json!({ "success": success }))
    }

    #[tool(description = "Review the ledger of attempts for a task")]
    pub async fn review_ledger(
        &self,
        #[tool(param)]
        #[schemars(description = "UUID of the task")]
        task_id: String,
        #[tool(param)]
        #[schemars(description = "Filter by outcome: pending, accepted, or rejected")]
        outcome_filter: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let tid = Self::parse_uuid(&task_id)?;
        let filter = outcome_filter
            .as_deref()
            .map(Self::parse_attempt_outcome)
            .transpose()?;
        let attempts = db::attempts::list_attempts(self.pool(), tid, filter)
            .await
            .map_err(Self::db_err)?;
        Self::json_content(&attempts)
    }

    #[tool(description = "Mark a task as completed, optionally recording a lesson learned")]
    pub async fn complete_task(
        &self,
        #[tool(param)]
        #[schemars(description = "UUID of the task")]
        task_id: String,
        #[tool(param)]
        #[schemars(description = "Lesson learned from this task (saved as a Lesson rule)")]
        lesson: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let tid = Self::parse_uuid(&task_id)?;
        let success = db::tasks::complete_task(self.pool(), tid)
            .await
            .map_err(Self::db_err)?;

        if success {
            if let Some(lesson_text) = &lesson {
                let project_id = self.project_id().await?;
                let embedding = self.embed(lesson_text).await?;
                db::semantic::create_rule(
                    self.pool(),
                    project_id,
                    db::RuleCategory::Lesson,
                    lesson_text,
                    Some(&embedding),
                )
                .await
                .map_err(Self::db_err)?;
            }
        }

        Self::json_content(&serde_json::json!({ "success": success }))
    }

    // -- Search tools --

    #[tool(description = "Find similar past failures using semantic search on rejection reasoning")]
    pub async fn find_similar_failures(
        &self,
        #[tool(param)]
        #[schemars(description = "Description of the error or failure")]
        error_description: String,
        #[tool(param)]
        #[schemars(description = "Max results (default 5)")]
        limit: Option<i64>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let project_id = self.project_id().await?;
        let embedding = self.embed(&error_description).await?;
        let attempts = db::attempts::search_similar_failures(
            self.pool(),
            project_id,
            &embedding,
            limit.unwrap_or(5),
        )
        .await
        .map_err(Self::db_err)?;
        Self::json_content(&attempts)
    }

    // -- System tools --

    #[tool(
        description = "Get the current active context: project, active task, and recent attempts"
    )]
    pub async fn get_active_context(&self) -> Result<CallToolResult, rmcp::Error> {
        let project_id = self.project_id().await?;
        let project = db::projects::get_project(self.pool(), project_id)
            .await
            .map_err(Self::db_err)?;

        let active_tasks =
            db::tasks::list_tasks(self.pool(), project_id, Some(db::TaskStatus::Active))
                .await
                .map_err(Self::db_err)?;

        let mut recent_attempts = Vec::new();
        if let Some(task) = active_tasks.first() {
            recent_attempts = db::attempts::list_attempts(self.pool(), task.id, None)
                .await
                .map_err(Self::db_err)?;
        }

        let active_rules = db::semantic::list_rules(self.pool(), project_id, None)
            .await
            .map_err(Self::db_err)?;

        Self::json_content(&serde_json::json!({
            "project": project,
            "active_tasks": active_tasks,
            "recent_attempts": recent_attempts,
            "active_rules_count": active_rules.len(),
        }))
    }

    #[tool(description = "Switch to a project by name (creates it if it doesn't exist)")]
    pub async fn switch_project(
        &self,
        #[tool(param)]
        #[schemars(description = "Project name")]
        name: Option<String>,
        #[tool(param)]
        #[schemars(description = "Project root filesystem path")]
        root_path: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let project_name = name.unwrap_or_else(|| self.config().default_project_name.clone());
        let path = root_path.unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string())
        });

        let project_id = db::projects::get_or_create_project(self.pool(), &project_name, &path)
            .await
            .map_err(Self::db_err)?;

        self.set_project_id(project_id).await;
        Self::json_content(&serde_json::json!({
            "project_id": project_id.to_string(),
            "project_name": project_name,
        }))
    }

    #[tool(
        description = "Export all memory (rules, tasks, attempts) for the current project as JSON"
    )]
    pub async fn export_memory(
        &self,
        #[tool(param)]
        #[schemars(description = "Export format: json (only json supported currently)")]
        format: String,
    ) -> Result<CallToolResult, rmcp::Error> {
        if format.to_lowercase() != "json" {
            return Err(rmcp::Error::invalid_params(
                "Only 'json' format is currently supported",
                None,
            ));
        }

        let project_id = self.project_id().await?;
        let rules = db::semantic::list_rules(self.pool(), project_id, None)
            .await
            .map_err(Self::db_err)?;
        let tasks = db::tasks::list_tasks(self.pool(), project_id, None)
            .await
            .map_err(Self::db_err)?;

        let mut all_attempts = Vec::new();
        for task in &tasks {
            let attempts = db::attempts::list_attempts(self.pool(), task.id, None)
                .await
                .map_err(Self::db_err)?;
            all_attempts.extend(attempts);
        }

        Self::json_content(&serde_json::json!({
            "project_id": project_id.to_string(),
            "rules": rules,
            "tasks": tasks,
            "attempts": all_attempts,
        }))
    }
}

#[tool(tool_box)]
impl ServerHandler for LoreServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Lore is a memory management MCP server. It provides long-term semantic memory \
                 (rules, preferences, lessons) and episodic memory (task ledger with attempts). \
                 Call switch_project first to set the active project context."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rule_category_valid() {
        assert!(LoreServer::parse_rule_category("preference").is_ok());
        assert!(LoreServer::parse_rule_category("Fact").is_ok());
        assert!(LoreServer::parse_rule_category("CONSTRAINT").is_ok());
        assert!(LoreServer::parse_rule_category("lesson").is_ok());
    }

    #[test]
    fn test_parse_rule_category_invalid() {
        assert!(LoreServer::parse_rule_category("garbage").is_err());
    }

    #[test]
    fn test_parse_attempt_outcome_valid() {
        assert!(LoreServer::parse_attempt_outcome("pending").is_ok());
        assert!(LoreServer::parse_attempt_outcome("Accepted").is_ok());
        assert!(LoreServer::parse_attempt_outcome("REJECTED").is_ok());
    }

    #[test]
    fn test_parse_attempt_outcome_invalid() {
        assert!(LoreServer::parse_attempt_outcome("maybe").is_err());
    }

    #[test]
    fn test_parse_uuid_valid() {
        let u = uuid::Uuid::new_v4().to_string();
        assert!(LoreServer::parse_uuid(&u).is_ok());
    }

    #[test]
    fn test_parse_uuid_invalid() {
        assert!(LoreServer::parse_uuid("not-a-uuid").is_err());
    }
}
