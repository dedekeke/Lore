use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rmcp::{model::*, service::RequestContext, tool, RoleServer, ServerHandler};
use sqlx::PgPool;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::cache::LoreCache;
use crate::config::Config;
use crate::db;
use crate::embeddings::{AnyEmbeddingProvider, EmbeddingProvider};
use crate::webhooks;

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
    pub tool_call_count: AtomicU64,
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
                tool_call_count: AtomicU64::new(0),
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

    /// Capture current git HEAD commit hash for the project's root_path
    async fn capture_git_ref(&self) -> Option<String> {
        let pid = self.inner.current_project_id.read().await;
        let project_id = (*pid)?;
        drop(pid);
        let project = db::projects::get_project(self.pool(), project_id)
            .await
            .ok()
            .flatten()?;
        let output = tokio::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(&project.root_path)
            .output()
            .await
            .ok()?;
        if output.status.success() {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            None
        }
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
                format!(
                    "Invalid task status: '{other}'. Valid: active, completed, abandoned, blocked"
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

    fn fire_webhook(&self, event: &str, data: serde_json::Value) {
        if let Some(url) = &self.config().webhook_url {
            let project = self.config().default_project_name.clone();
            webhooks::fire(url, &self.config().webhook_events, event, &project, data);
        }
    }

    fn reset_session_counter(&self) {
        self.inner.tool_call_count.store(0, Ordering::Relaxed);
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
            map.insert(
                "_next_step".into(),
                serde_json::Value::String(next_step.into()),
            );
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

        // Check for near-duplicates (cosine similarity >= 0.95)
        let duplicates = db::semantic::find_duplicates(self.pool(), project_id, &embedding, 0.95)
            .await
            .map_err(Self::db_err)?;
        if !duplicates.is_empty() {
            let dup_ids: Vec<String> = duplicates.iter().map(|r| r.id.to_string()).collect();
            let dup_preview: String = duplicates[0].content.chars().take(100).collect();
            return Self::json_content_with_nudge(
                &serde_json::json!({
                    "duplicate_warning": true,
                    "similar_rule_ids": dup_ids,
                    "similar_content_preview": dup_preview,
                }),
                "Near-duplicate rule found. Use update_rule to modify the existing rule instead, or use forget_rule to delete it first.",
            );
        }

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
        #[tool(param)]
        #[schemars(description = "Optional agent identifier for multi-agent workflows")]
        agent_id: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("approach_summary", &approach_summary, 4096)?;
        if let Some(ref code) = code_snippet {
            Self::validate_len("code_snippet", code, 32768)?;
        }
        let tid = Self::parse_uuid(&task_id)?;
        let git_ref = self.capture_git_ref().await;
        let id = db::attempts::create_attempt(
            self.pool(),
            tid,
            &approach_summary,
            code_snippet.as_deref(),
            agent_id.as_deref(),
            git_ref.as_deref(),
        )
        .await
        .map_err(Self::db_err)?;
        let mut result = serde_json::json!({ "attempt_id": id.to_string() });
        if let Some(ref gr) = git_ref {
            result["git_checkpoint"] = serde_json::Value::String(gr.clone());
        }
        Self::json_content_with_nudge(
            &result,
            "Attempt logged. Present the code to the user and WAIT for their feedback. Do NOT auto-accept. Call log_outcome only after the user confirms success ('accepted') or reports failure ('rejected').",
        )
    }

    #[tool(
        description = "Log the outcome of an attempt. ONLY mark 'accepted' when the user explicitly confirms success. Use 'pending' if awaiting confirmation."
    )]
    pub async fn log_outcome(
        &self,
        #[tool(param)]
        #[schemars(description = "UUID of the attempt")]
        attempt_id: String,
        #[tool(param)]
        #[schemars(
            description = "Outcome: pending (awaiting user confirmation), accepted (user confirmed), rejected (user reported failure), or unknown (stale/abandoned)"
        )]
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
        // Check rejection threshold for webhook
        if out == db::AttemptOutcome::Rejected {
            if let Ok(Some(attempt)) = db::attempts::get_attempt(self.pool(), aid).await {
                let rejected = db::attempts::list_attempts(
                    self.pool(),
                    attempt.task_id,
                    Some(db::AttemptOutcome::Rejected),
                )
                .await
                .unwrap_or_default();
                let threshold = self.config().webhook_rejection_threshold as usize;
                if rejected.len() == threshold {
                    self.fire_webhook(
                        "rejection_threshold",
                        serde_json::json!({
                            "task_id": attempt.task_id,
                            "rejection_count": rejected.len(),
                            "latest_reasoning": reasoning,
                        }),
                    );
                }
            }
        }

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

        if success {
            self.fire_webhook(
                "task_completed",
                serde_json::json!({ "task_id": task_id, "lesson": lesson }),
            );
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

        if success {
            self.fire_webhook(
                "task_abandoned",
                serde_json::json!({ "task_id": task_id, "reason": reason }),
            );
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
        let st = status.as_deref().map(Self::parse_task_status).transpose()?;
        let tasks = db::tasks::list_tasks(self.pool(), project_id, st)
            .await
            .map_err(Self::db_err)?;
        Self::json_content(&tasks)
    }

    #[tool(
        description = "Get task analytics: attempt counts, rejection rate, and time-to-resolution"
    )]
    pub async fn get_task_stats(
        &self,
        #[tool(param)]
        #[schemars(description = "Filter by status: active, completed, abandoned, or blocked")]
        status: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let project_id = self.project_id().await?;
        let st = status.as_deref().map(Self::parse_task_status).transpose()?;
        let stats = db::tasks::get_task_stats(self.pool(), project_id, st)
            .await
            .map_err(Self::db_err)?;

        // Compute aggregate summary
        let total = stats.len();
        let total_attempts: i64 = stats.iter().map(|s| s.total_attempts).sum();
        let total_rejected: i64 = stats.iter().map(|s| s.rejected_attempts).sum();
        let total_accepted: i64 = stats.iter().map(|s| s.accepted_attempts).sum();
        let resolved_attempts = total_accepted + total_rejected;
        let rejection_rate = if resolved_attempts > 0 {
            total_rejected as f64 / resolved_attempts as f64
        } else {
            0.0
        };
        let resolved: Vec<_> = stats.iter().filter_map(|s| s.resolution_minutes).collect();
        // avg_resolution only counts tasks with completed_at (completed/abandoned)
        let avg_resolution = if resolved.is_empty() {
            None
        } else {
            Some(resolved.iter().sum::<f64>() / resolved.len() as f64)
        };

        Self::json_content(&serde_json::json!({
            "summary": {
                "total_tasks": total,
                "total_attempts": total_attempts,
                "total_rejected": total_rejected,
                "rejection_rate": rejection_rate,
                "avg_resolution_minutes": avg_resolution,
            },
            "tasks": stats,
        }))
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
        #[tool(param)]
        #[schemars(description = "Search across all projects (default false)")]
        cross_project: Option<bool>,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("error_description", &error_description, 2048)?;
        let project_id = if cross_project.unwrap_or(false) {
            None
        } else {
            Some(self.project_id().await?)
        };
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

        let context_wipes = if let Some(task) = active_tasks.first() {
            db::snapshots::count_snapshots(self.pool(), task.id)
                .await
                .map_err(Self::db_err)?
        } else {
            0
        };

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
                "context_wipes": context_wipes,
            }),
            nudge,
        )
    }

    #[tool(
        description = "Log a context wipe event. Call this when the AI context window is about to be exhausted or has been reset."
    )]
    pub async fn log_context_wipe(
        &self,
        #[tool(param)]
        #[schemars(description = "UUID of the active task")]
        task_id: String,
        #[tool(param)]
        #[schemars(description = "Approximate token count before the wipe")]
        token_count: i32,
        #[tool(param)]
        #[schemars(description = "UUID of the last attempt before the wipe")]
        last_attempt_id: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let tid = Self::parse_uuid(&task_id)?;
        let aid = last_attempt_id
            .as_deref()
            .map(Self::parse_uuid)
            .transpose()?;
        let id = db::snapshots::create_snapshot(self.pool(), tid, token_count, aid)
            .await
            .map_err(Self::db_err)?;
        Self::json_content_with_nudge(
            &serde_json::json!({ "snapshot_id": id.to_string() }),
            "Context wipe recorded. In the new session, call get_next_steps() or get_active_context() to resume.",
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

    fn render_markdown_export(
        project: &Option<db::projects::Project>,
        rules: &[db::semantic::SemanticRule],
        tasks: &[db::tasks::Task],
        attempts: &[db::attempts::Attempt],
    ) -> String {
        use std::fmt::Write;
        let mut md = String::new();

        let name = project
            .as_ref()
            .map(|p| p.name.as_str())
            .unwrap_or("Unknown");
        writeln!(md, "# Lore Export — {name}\n").unwrap();

        writeln!(md, "## Rules ({} total)\n", rules.len()).unwrap();
        for r in rules {
            let preview: String = r.content.chars().take(80).collect();
            writeln!(md, "### [{:?}] {preview}", r.category).unwrap();
            writeln!(md, "- **ID:** `{}`", r.id).unwrap();
            writeln!(md, "- **Content:** {}\n", r.content).unwrap();
        }

        writeln!(md, "## Tasks ({} total)\n", tasks.len()).unwrap();
        for t in tasks {
            writeln!(md, "### {:?}: {}", t.status, t.description).unwrap();
            writeln!(md, "- **ID:** `{}`", t.id).unwrap();
            writeln!(
                md,
                "- **Created:** {}",
                t.created_at.format("%Y-%m-%d %H:%M UTC")
            )
            .unwrap();
            if let Some(ca) = t.completed_at {
                writeln!(md, "- **Completed:** {}", ca.format("%Y-%m-%d %H:%M UTC")).unwrap();
            }
            let task_attempts: Vec<_> = attempts.iter().filter(|a| a.task_id == t.id).collect();
            if !task_attempts.is_empty() {
                writeln!(md, "\n**Attempts:**\n").unwrap();
                for a in task_attempts {
                    writeln!(md, "- **{:?}** — {}", a.outcome, a.approach_summary).unwrap();
                    if !a.reasoning.is_empty() {
                        for line in a.reasoning.lines() {
                            writeln!(md, "  > {line}").unwrap();
                        }
                    }
                }
            }
            writeln!(md).unwrap();
        }

        md
    }

    #[tool(description = "Export all memory (rules, tasks, attempts) for the current project")]
    pub async fn export_memory(
        &self,
        #[tool(param)]
        #[schemars(description = "Export format: json or markdown")]
        format: String,
    ) -> Result<CallToolResult, rmcp::Error> {
        let fmt = format.to_lowercase();
        if fmt != "json" && fmt != "markdown" {
            return Err(rmcp::Error::invalid_params(
                "Supported formats: json, markdown",
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

        if fmt == "markdown" {
            let project = db::projects::get_project(self.pool(), project_id)
                .await
                .map_err(Self::db_err)?;
            let md = Self::render_markdown_export(&project, &rules, &tasks, &all_attempts);
            return Ok(CallToolResult::success(vec![Content::text(md)]));
        }

        Self::json_content_with_nudge(
            &serde_json::json!({
                "project_id": project_id.to_string(),
                "rules": rules,
                "tasks": tasks,
                "attempts": all_attempts,
            }),
            "Export complete.",
        )
    }

    #[tool(
        description = "Get a cold-start briefing: active/blocked tasks with attempt stats, stale pending attempts, and recent lessons. Call this at the start of a new session to know what to work on without resuming prior context."
    )]
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

        let lessons =
            db::semantic::list_rules(self.pool(), project_id, Some(db::RuleCategory::Lesson))
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
            actions.push(
                "No active or blocked tasks. Call start_task(description) for a new goal.".into(),
            );
        }

        let nudge = if summaries.iter().any(|s| s.pending_attempts > 0) {
            "Resolve pending attempts first: ask the user for confirmation, then log_outcome."
        } else if summaries
            .iter()
            .any(|s| s.status == db::TaskStatus::Blocked)
        {
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

    #[tool(
        description = "Re-read the mandatory episodic memory protocol. Call this if you are unsure what Lore tool to use next."
    )]
    pub async fn get_protocol(&self) -> Result<CallToolResult, rmcp::Error> {
        Ok(CallToolResult::success(vec![Content::text(
            Self::protocol_text(),
        )]))
    }

    #[tool(
        description = "Generate a handoff packet for session transitions. Call this before context exhaustion to create a dense briefing that the next session can ingest via get_next_steps. Automatically logs a context wipe event."
    )]
    pub async fn generate_handoff(
        &self,
        #[tool(param)]
        #[schemars(description = "Approximate token count consumed in current session")]
        token_count: Option<i32>,
    ) -> Result<CallToolResult, rmcp::Error> {
        use std::fmt::Write;
        let project_id = self.project_id().await?;
        let project = db::projects::get_project(self.pool(), project_id)
            .await
            .map_err(Self::db_err)?;

        let active_tasks =
            db::tasks::list_tasks(self.pool(), project_id, Some(db::TaskStatus::Active))
                .await
                .map_err(Self::db_err)?;
        let blocked_tasks =
            db::tasks::list_tasks(self.pool(), project_id, Some(db::TaskStatus::Blocked))
                .await
                .map_err(Self::db_err)?;

        let mut md = String::new();
        let name = project
            .as_ref()
            .map(|p| p.name.as_str())
            .unwrap_or("Unknown");
        writeln!(md, "# Handoff Packet — {name}\n").unwrap();

        // Active tasks with recent attempts
        if !active_tasks.is_empty() {
            writeln!(md, "## Active Tasks\n").unwrap();
            for t in &active_tasks {
                writeln!(md, "### {}\n- **ID:** `{}`", t.description, t.id).unwrap();
                let attempts = db::attempts::list_attempts(self.pool(), t.id, None)
                    .await
                    .unwrap_or_default();
                let recent: Vec<_> = attempts.iter().rev().take(5).collect();
                if !recent.is_empty() {
                    writeln!(md, "- **Recent attempts:**").unwrap();
                    for a in recent.iter().rev() {
                        let outcome = format!("{:?}", a.outcome).to_lowercase();
                        let summary = if a.reasoning.is_empty() {
                            a.approach_summary.clone()
                        } else {
                            format!("{}: {}", a.approach_summary, a.reasoning)
                        };
                        let git = a
                            .git_ref
                            .as_deref()
                            .map(|g| format!(" @ {g}"))
                            .unwrap_or_default();
                        writeln!(md, "  - [{outcome}] {summary}{git}").unwrap();
                    }
                }
                writeln!(md).unwrap();
            }
        }

        // Blocked tasks
        if !blocked_tasks.is_empty() {
            writeln!(md, "## Blocked Tasks\n").unwrap();
            for t in &blocked_tasks {
                writeln!(md, "- `{}`: {}", t.id, t.description).unwrap();
            }
            writeln!(md).unwrap();
        }

        // Recent lessons
        let lessons =
            db::semantic::list_rules(self.pool(), project_id, Some(db::RuleCategory::Lesson))
                .await
                .unwrap_or_default();
        let recent_lessons: Vec<_> = lessons.iter().rev().take(5).collect();
        if !recent_lessons.is_empty() {
            writeln!(md, "## Recent Lessons\n").unwrap();
            for l in recent_lessons.iter().rev() {
                let preview: String = l.content.chars().take(200).collect();
                writeln!(md, "- {preview}").unwrap();
            }
            writeln!(md).unwrap();
        }

        // Log context wipe if there's an active task
        if let Some(task) = active_tasks.first() {
            let last_attempt = db::attempts::list_attempts(self.pool(), task.id, None)
                .await
                .ok()
                .and_then(|a| a.last().map(|a| a.id));
            let _ = db::snapshots::create_snapshot(
                self.pool(),
                task.id,
                token_count.unwrap_or(0),
                last_attempt,
            )
            .await;
        }

        writeln!(
            md,
            "---\n*Handoff generated. Next session: call `get_next_steps()` to resume.*"
        )
        .unwrap();

        Ok(CallToolResult::success(vec![Content::text(md)]))
    }
}

impl ServerHandler for LoreServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(Self::protocol_text().into()),
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
            ..Default::default()
        }
    }

    async fn list_resources(
        &self,
        _request: PaginatedRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, rmcp::Error> {
        use rmcp::model::AnnotateAble;

        Ok(ListResourcesResult {
            resources: vec![
                RawResource {
                    uri: "lore://protocol".into(),
                    name: "Lore Protocol".into(),
                    description: Some("Mandatory episodic memory protocol rules".into()),
                    mime_type: Some("text/plain".into()),
                    size: None,
                }
                .no_annotation(),
                RawResource {
                    uri: "lore://active-context".into(),
                    name: "Active Context".into(),
                    description: Some(
                        "Current project, active tasks, and context wipe count".into(),
                    ),
                    mime_type: Some("application/json".into()),
                    size: None,
                }
                .no_annotation(),
            ],
            next_cursor: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, rmcp::Error> {
        match request.uri.as_str() {
            "lore://protocol" => Ok(ReadResourceResult {
                contents: vec![ResourceContents::text(
                    Self::protocol_text(),
                    "lore://protocol",
                )],
            }),
            "lore://active-context" => {
                let project_id = self.inner.current_project_id.read().await;
                let text = if let Some(pid) = *project_id {
                    drop(project_id);
                    let project = db::projects::get_project(self.pool(), pid)
                        .await
                        .ok()
                        .flatten();
                    let tasks: Vec<db::tasks::Task> = db::tasks::list_tasks(self.pool(), pid, None)
                        .await
                        .unwrap_or_default();
                    let wipes = db::snapshots::count_snapshots(self.pool(), pid)
                        .await
                        .unwrap_or(0);
                    serde_json::to_string_pretty(&serde_json::json!({
                        "project": project,
                        "active_tasks": tasks.iter()
                            .filter(|t| t.status == db::TaskStatus::Active)
                            .count(),
                        "total_tasks": tasks.len(),
                        "context_wipes": wipes,
                    }))
                    .unwrap_or_default()
                } else {
                    r#"{"project": null, "hint": "Call switch_project first"}"#.to_string()
                };
                Ok(ReadResourceResult {
                    contents: vec![ResourceContents::text(text, "lore://active-context")],
                })
            }
            _ => Err(ErrorData::resource_not_found(
                "Unknown resource URI",
                Some(serde_json::Value::String(request.uri)),
            )),
        }
    }

    async fn list_tools(
        &self,
        _request: PaginatedRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::Error> {
        let disabled = &self.config().disabled_tools;
        let tools = if disabled.is_empty() {
            Self::tool_box().list()
        } else {
            Self::tool_box()
                .list()
                .into_iter()
                .filter(|t| !disabled.contains(t.name.as_ref()))
                .collect()
        };
        Ok(ListToolsResult {
            next_cursor: None,
            tools,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let disabled = &self.config().disabled_tools;
        if disabled.contains(request.name.as_ref()) {
            return Err(ErrorData::invalid_params(
                "Tool is disabled via DISABLED_TOOLS config",
                Some(serde_json::json!({ "tool": request.name })),
            ));
        }

        if matches!(request.name.as_ref(), "get_next_steps" | "switch_project") {
            self.reset_session_counter();
        }

        let ctx = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let result = Self::tool_box().call(ctx).await?;

        self.inner.tool_call_count.fetch_add(1, Ordering::Relaxed);

        Ok(result)
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

    #[test]
    fn test_tool_box_lists_all_tools() {
        let tools = LoreServer::tool_box().list();
        assert!(
            tools.len() >= 21,
            "Expected at least 21 tools, got {}",
            tools.len()
        );
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(names.contains(&"remember_rule"));
        assert!(names.contains(&"forget_rule"));
        assert!(names.contains(&"generate_handoff"));
    }

    #[test]
    fn test_disabled_tools_filter() {
        let all = LoreServer::tool_box().list();
        let disabled: std::collections::HashSet<String> = ["forget_rule", "export_memory"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let filtered: Vec<_> = all
            .into_iter()
            .filter(|t| !disabled.contains(t.name.as_ref()))
            .collect();
        let names: Vec<&str> = filtered.iter().map(|t| t.name.as_ref()).collect();
        assert!(!names.contains(&"forget_rule"));
        assert!(!names.contains(&"export_memory"));
        assert!(names.contains(&"remember_rule"));
    }
}
