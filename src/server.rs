use std::sync::Arc;

use rmcp::{model::*, tool, ServerHandler};
use sqlx::PgPool;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::cache::LoreCache;
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
    pub cache: LoreCache,
}

impl LoreServer {
    pub fn new(pool: PgPool, embeddings: AnyEmbeddingProvider, config: Config) -> Self {
        Self {
            inner: Arc::new(LoreServerInner {
                pool,
                embeddings,
                config,
                current_project_id: RwLock::new(None),
                cache: LoreCache::new(1000, 500),
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
        if let Some(id) = *self.inner.current_project_id.read().await {
            return Ok(id);
        }
        // Auto-detect from cwd
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .map_err(|e| rmcp::Error::internal_error(format!("Cannot read cwd: {e}"), None))?;
        let (id, _name) = db::projects::get_or_create_project_by_path(self.pool(), &cwd)
            .await
            .map_err(Self::db_err)?;
        self.set_project_id(id).await;
        Ok(id)
    }

    pub async fn set_project_id(&self, id: Uuid) {
        *self.inner.current_project_id.write().await = Some(id);
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, rmcp::Error> {
        let key = text.to_string();
        if let Some(cached) = self.inner.cache.embeddings.get(&key).await {
            return Ok((*cached).clone());
        }
        let result = self
            .embeddings()
            .embed(text)
            .await
            .map_err(|e| rmcp::Error::internal_error(format!("Embedding error: {e}"), None))?;
        self.inner
            .cache
            .embeddings
            .insert(key, Arc::new(result.clone()))
            .await;
        Ok(result)
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

    fn parse_task_status(s: &str) -> Result<db::TaskStatus, rmcp::Error> {
        match s.to_lowercase().as_str() {
            "active" => Ok(db::TaskStatus::Active),
            "completed" => Ok(db::TaskStatus::Completed),
            "abandoned" => Ok(db::TaskStatus::Abandoned),
            "blocked" => Ok(db::TaskStatus::Blocked),
            other => Err(rmcp::Error::invalid_params(
                format!("Invalid task status: '{other}'. Valid: active, completed, abandoned, blocked"),
                None,
            )),
        }
    }

    fn parse_attempt_outcome(s: &str) -> Result<db::AttemptOutcome, rmcp::Error> {
        match s.to_lowercase().as_str() {
            "pending" => Ok(db::AttemptOutcome::Pending),
            "accepted" => Ok(db::AttemptOutcome::Accepted),
            "rejected" => Ok(db::AttemptOutcome::Rejected),
            "unknown" => Ok(db::AttemptOutcome::Unknown),
            other => Err(rmcp::Error::invalid_params(
                format!("Invalid outcome: '{other}'. Valid: pending, accepted, rejected, unknown"),
                None,
            )),
        }
    }

    fn parse_uuid(s: &str) -> Result<Uuid, rmcp::Error> {
        s.parse::<Uuid>()
            .map_err(|e| rmcp::Error::invalid_params(format!("Invalid UUID '{s}': {e}"), None))
    }

    fn validate_len(field: &str, val: &str, max: usize) -> Result<(), rmcp::Error> {
        if val.len() > max {
            return Err(rmcp::Error::invalid_params(
                format!("{field} exceeds max length ({} > {max} bytes)", val.len()),
                None,
            ));
        }
        Ok(())
    }

    fn db_err(e: sqlx::Error) -> rmcp::Error {
        rmcp::Error::internal_error(format!("Database error: {e}"), None)
    }

    fn json_content<T: serde::Serialize>(val: &T) -> Result<CallToolResult, rmcp::Error> {
        let json = serde_json::to_string_pretty(val)
            .map_err(|e| rmcp::Error::internal_error(format!("Serialization error: {e}"), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    fn json_content_with_nudge<T: serde::Serialize>(
        val: &T,
        next_step: &str,
    ) -> Result<CallToolResult, rmcp::Error> {
        let mut obj = serde_json::to_value(val)
            .map_err(|e| rmcp::Error::internal_error(format!("Serialization error: {e}"), None))?;
        if let Some(map) = obj.as_object_mut() {
            map.insert("_next_step".into(), serde_json::Value::String(next_step.into()));
        }
        let json = serde_json::to_string_pretty(&obj)
            .map_err(|e| rmcp::Error::internal_error(format!("Serialization error: {e}"), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    pub fn protocol_text() -> &'static str {
        "CRITICAL OPERATING PROTOCOL — MANDATORY FOR ALL INTERACTIONS:\n\
         1. FIRST CALL: switch_project(name, root_path) to set context (optional — project is auto-detected from cwd if not called).\n\
         2. NEW GOALS: call start_task(description) BEFORE generating any code.\n\
         3. PROPOSING CODE: call propose_attempt(task_id, approach, code) BEFORE writing code to the user.\n\
         4. FAILURES: if the user reports an error, IMMEDIATELY call log_outcome(attempt_id, 'rejected', reasoning) BEFORE suggesting a fix.\n\
         5. OUTCOME RULES: Do NOT auto-accept. Only call log_outcome(attempt_id, 'accepted', reasoning) when the USER explicitly confirms success. If unsure, use 'pending'.\n\
         6. CONTEXT RECOVERY: if you feel lost or the user says 'try something else', call review_ledger(task_id) to read past failures so you don't repeat them.\n\
         7. PERIODIC CHECK: call get_active_context() every ~5 messages to stay grounded.\n\
         8. COLD START: at the beginning of a new session, call get_next_steps() for a briefing on pending work.\n\
         9. If unsure what to do next, call get_protocol() to re-read these rules.\n\
         Violation causes context rot and repeated failures."
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
        Self::validate_len("content", &content, 4096)?;
        let project_id = self.project_id().await?;
        let cat = Self::parse_rule_category(&category)?;
        let embedding = self.embed(&content).await?;
        let id =
            db::semantic::create_rule(self.pool(), project_id, cat, &content, Some(&embedding))
                .await
                .map_err(Self::db_err)?;
        self.inner.cache.invalidate_search();
        Self::json_content_with_nudge(
            &serde_json::json!({ "rule_id": id.to_string() }),
            "Rule stored. Continue with your current task.",
        )
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
        Self::validate_len("query", &query, 2048)?;
        let project_id = self.project_id().await?;
        let embedding = self.embed(&query).await?;
        let cat = category
            .as_deref()
            .map(Self::parse_rule_category)
            .transpose()?;
        let rules = db::semantic::search_rules_hybrid(
            self.pool(),
            project_id,
            &embedding,
            &query,
            limit.unwrap_or(10),
            cat,
        )
        .await
        .map_err(Self::db_err)?;
        Self::json_content_with_nudge(&rules, "Apply these rules to your current task.")
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
        self.inner.cache.invalidate_search();
        Self::json_content_with_nudge(
            &serde_json::json!({ "deleted": deleted }),
            "Rule removed. Continue with your current task.",
        )
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

    #[tool(description = "Update an existing semantic rule's category and/or content")]
    pub async fn update_rule(
        &self,
        #[tool(param)]
        #[schemars(description = "UUID of the rule to update")]
        rule_id: String,
        #[tool(param)]
        #[schemars(description = "New category: preference, fact, constraint, or lesson")]
        category: Option<String>,
        #[tool(param)]
        #[schemars(description = "New content for the rule")]
        content: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        if category.is_none() && content.is_none() {
            return Err(rmcp::Error::invalid_params(
                "Provide at least one of: category, content",
                None,
            ));
        }
        if let Some(ref c) = content {
            Self::validate_len("content", c, 4096)?;
        }
        let id = Self::parse_uuid(&rule_id)?;
        let cat = category
            .as_deref()
            .map(Self::parse_rule_category)
            .transpose()?;
        let embedding = match &content {
            Some(text) => Some(self.embed(text).await?),
            None => None,
        };
        let updated = db::semantic::update_rule(
            self.pool(),
            id,
            cat,
            content.as_deref(),
            embedding.as_deref(),
        )
        .await
        .map_err(Self::db_err)?;
        self.inner.cache.invalidate_search();
        Self::json_content_with_nudge(
            &serde_json::json!({ "updated": updated }),
            "Rule updated. Continue with your current task.",
        )
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
        Self::validate_len("description", &description, 4096)?;
        let project_id = self.project_id().await?;
        let parent = parent_task_id
            .as_deref()
            .map(Self::parse_uuid)
            .transpose()?;
        let id = db::tasks::create_task(self.pool(), project_id, &description, parent)
            .await
            .map_err(Self::db_err)?;
        Self::json_content_with_nudge(
            &serde_json::json!({ "task_id": id.to_string() }),
            "Task created. Next: call propose_attempt(task_id, approach, code) BEFORE writing code to the user.",
        )
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
        Self::validate_len("approach_summary", &approach_summary, 4096)?;
        if let Some(ref code) = code_snippet {
            Self::validate_len("code_snippet", code, 32768)?;
        }
        let tid = Self::parse_uuid(&task_id)?;
        let id = db::attempts::create_attempt(
            self.pool(),
            tid,
            &approach_summary,
            code_snippet.as_deref(),
        )
        .await
        .map_err(Self::db_err)?;
        Self::json_content_with_nudge(
            &serde_json::json!({ "attempt_id": id.to_string() }),
            "Attempt logged. Present the code to the user and WAIT for their feedback. Do NOT auto-accept. Call log_outcome only after the user confirms success ('accepted') or reports failure ('rejected').",
        )
    }

    #[tool(description = "Log the outcome of an attempt. ONLY mark 'accepted' when the user explicitly confirms success. Use 'pending' if awaiting confirmation.")]
    pub async fn log_outcome(
        &self,
        #[tool(param)]
        #[schemars(description = "UUID of the attempt")]
        attempt_id: String,
        #[tool(param)]
        #[schemars(description = "Outcome: pending (awaiting user confirmation), accepted (user confirmed), rejected (user reported failure), or unknown (stale/abandoned)")]
        outcome: String,
        #[tool(param)]
        #[schemars(description = "Reasoning for the outcome")]
        reasoning: String,
        #[tool(param)]
        #[schemars(description = "Optional git reference (commit hash, branch)")]
        git_ref: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("reasoning", &reasoning, 4096)?;
        let aid = Self::parse_uuid(&attempt_id)?;
        let out = Self::parse_attempt_outcome(&outcome)?;
        let embedding = self.embed(&reasoning).await?;
        let success = db::attempts::log_outcome(
            self.pool(),
            aid,
            out.clone(),
            &reasoning,
            Some(&embedding),
            git_ref.as_deref(),
        )
        .await
        .map_err(Self::db_err)?;
        let nudge = match out {
            db::AttemptOutcome::Rejected => {
                "Outcome logged. Next: call review_ledger(task_id) to review all past failures, then propose_attempt with a new approach."
            }
            db::AttemptOutcome::Accepted => {
                "Outcome logged. Next: call complete_task(task_id, lesson) to close the task and extract a lesson."
            }
            db::AttemptOutcome::Pending => "Outcome set to pending — waiting for user confirmation. Do NOT change to 'accepted' until the user explicitly confirms.",
            db::AttemptOutcome::Unknown => "Outcome marked as unknown — this attempt will be cleaned up during retention.",
        };
        Self::json_content_with_nudge(&serde_json::json!({ "success": success }), nudge)
    }

    #[tool(description = "Review the ledger of attempts for a task")]
    pub async fn review_ledger(
        &self,
        #[tool(param)]
        #[schemars(description = "UUID of the task")]
        task_id: String,
        #[tool(param)]
        #[schemars(description = "Filter by outcome: pending, accepted, rejected, or unknown")]
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
        Self::json_content_with_nudge(
            &attempts,
            "Use the above failures to avoid repeating mistakes. Call propose_attempt with a new approach.",
        )
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
        if let Some(ref l) = lesson {
            Self::validate_len("lesson", l, 4096)?;
        }
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

        Self::json_content_with_nudge(
            &serde_json::json!({ "success": success }),
            "Task closed. For your next goal, call start_task(description).",
        )
    }

    #[tool(description = "Abandon a task with a reason. Optionally saves the reason as a lesson.")]
    pub async fn abandon_task(
        &self,
        #[tool(param)]
        #[schemars(description = "UUID of the task to abandon")]
        task_id: String,
        #[tool(param)]
        #[schemars(description = "Why this task is being abandoned")]
        reason: String,
        #[tool(param)]
        #[schemars(description = "If true, save the reason as a Lesson rule")]
        save_lesson: Option<bool>,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("reason", &reason, 4096)?;
        let tid = Self::parse_uuid(&task_id)?;
        let success = db::tasks::abandon_task(self.pool(), tid)
            .await
            .map_err(Self::db_err)?;

        if success && save_lesson.unwrap_or(false) {
            let project_id = self.project_id().await?;
            let embedding = self.embed(&reason).await?;
            db::semantic::create_rule(
                self.pool(),
                project_id,
                db::RuleCategory::Lesson,
                &reason,
                Some(&embedding),
            )
            .await
            .map_err(Self::db_err)?;
        }

        Self::json_content_with_nudge(
            &serde_json::json!({ "success": success }),
            "Task abandoned. For your next goal, call start_task(description).",
        )
    }

    #[tool(description = "List tasks for the current project, optionally filtered by status")]
    pub async fn list_tasks(
        &self,
        #[tool(param)]
        #[schemars(description = "Filter by status: active, completed, abandoned, or blocked")]
        status: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let project_id = self.project_id().await?;
        let st = status
            .as_deref()
            .map(Self::parse_task_status)
            .transpose()?;
        let tasks = db::tasks::list_tasks(self.pool(), project_id, st)
            .await
            .map_err(Self::db_err)?;
        Self::json_content(&tasks)
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
        Self::validate_len("error_description", &error_description, 2048)?;
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
        Self::json_content_with_nudge(
            &attempts,
            "Review these past failures. Avoid repeating the same approaches. Call propose_attempt with a different strategy.",
        )
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

        let nudge = if active_tasks.is_empty() {
            "No active task. Call start_task(description) for your current goal."
        } else {
            "Use the active task and attempts above to continue. Call propose_attempt for your next approach."
        };

        Self::json_content_with_nudge(
            &serde_json::json!({
                "project": project,
                "active_tasks": active_tasks,
                "recent_attempts": recent_attempts,
                "active_rules_count": active_rules.len(),
            }),
            nudge,
        )
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
        Self::json_content_with_nudge(
            &serde_json::json!({
                "project_id": project_id.to_string(),
                "project_name": project_name,
            }),
            "Project set. Next: call start_task(description) for your current goal.",
        )
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

        Self::json_content_with_nudge(&serde_json::json!({
            "project_id": project_id.to_string(),
            "rules": rules,
            "tasks": tasks,
            "attempts": all_attempts,
        }), "Export complete.")
    }

    #[tool(description = "Get a cold-start briefing: active/blocked tasks with attempt stats, stale pending attempts, and recent lessons. Call this at the start of a new session to know what to work on without resuming prior context.")]
    pub async fn get_next_steps(&self) -> Result<CallToolResult, rmcp::Error> {
        let project_id = self.project_id().await?;
        let project = db::projects::get_project(self.pool(), project_id)
            .await
            .map_err(Self::db_err)?;

        let summaries = db::tasks::get_task_summaries(
            self.pool(),
            project_id,
            &[db::TaskStatus::Active, db::TaskStatus::Blocked],
        )
        .await
        .map_err(Self::db_err)?;

        let lessons = db::semantic::list_rules(
            self.pool(),
            project_id,
            Some(db::RuleCategory::Lesson),
        )
        .await
        .map_err(Self::db_err)?;
        // Only show most recent 5 lessons
        let recent_lessons: Vec<_> = lessons.into_iter().rev().take(5).collect();

        // Build action items
        let mut actions: Vec<String> = Vec::new();
        for s in &summaries {
            match s.status {
                db::TaskStatus::Active if s.pending_attempts > 0 => {
                    actions.push(format!(
                        "Task '{}' has {} pending attempt(s) awaiting outcome resolution",
                        s.description, s.pending_attempts
                    ));
                }
                db::TaskStatus::Active => {
                    actions.push(format!(
                        "Task '{}' is active ({} attempts, {} rejected) — propose next approach",
                        s.description, s.total_attempts, s.rejected_attempts
                    ));
                }
                db::TaskStatus::Blocked => {
                    actions.push(format!(
                        "Task '{}' is BLOCKED — needs unblocking before progress",
                        s.description
                    ));
                }
                _ => {}
            }
        }
        if actions.is_empty() {
            actions.push("No active or blocked tasks. Call start_task(description) for a new goal.".into());
        }

        let nudge = if summaries.iter().any(|s| s.pending_attempts > 0) {
            "Resolve pending attempts first: ask the user for confirmation, then log_outcome."
        } else if summaries.iter().any(|s| s.status == db::TaskStatus::Blocked) {
            "Unblock blocked tasks before starting new work."
        } else if summaries.iter().any(|s| s.status == db::TaskStatus::Active) {
            "Continue active tasks: call review_ledger(task_id) then propose_attempt."
        } else {
            "No pending work. Call start_task(description) when the user gives a new goal."
        };

        Self::json_content_with_nudge(
            &serde_json::json!({
                "project": project,
                "tasks": summaries,
                "actions": actions,
                "recent_lessons": recent_lessons,
            }),
            nudge,
        )
    }

    #[tool(description = "Re-read the mandatory episodic memory protocol. Call this if you are unsure what Lore tool to use next.")]
    pub async fn get_protocol(&self) -> Result<CallToolResult, rmcp::Error> {
        Ok(CallToolResult::success(vec![Content::text(Self::protocol_text())]))
    }
}

#[tool(tool_box)]
impl ServerHandler for LoreServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(Self::protocol_text().into()),
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
        assert!(LoreServer::parse_attempt_outcome("unknown").is_ok());
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

    #[test]
    fn test_parse_task_status_valid() {
        assert!(LoreServer::parse_task_status("active").is_ok());
        assert!(LoreServer::parse_task_status("Completed").is_ok());
        assert!(LoreServer::parse_task_status("ABANDONED").is_ok());
        assert!(LoreServer::parse_task_status("blocked").is_ok());
    }

    #[test]
    fn test_parse_task_status_invalid() {
        assert!(LoreServer::parse_task_status("done").is_err());
    }

    #[test]
    fn test_validate_len_ok() {
        assert!(LoreServer::validate_len("f", "short", 4096).is_ok());
    }

    #[test]
    fn test_validate_len_exceeds() {
        let long = "x".repeat(5000);
        assert!(LoreServer::validate_len("f", &long, 4096).is_err());
    }
}
