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
    pub http_client: reqwest::Client,
}

impl LoreServer {
    pub fn new(pool: PgPool, embeddings: AnyEmbeddingProvider, config: Config) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        Self {
            inner: Arc::new(LoreServerInner {
                pool,
                embeddings,
                config,
                current_project_id: RwLock::new(None),
                cache: LoreCache::new(1000, 500),
                tool_call_count: AtomicU64::new(0),
                http_client,
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
        if !self.config().capture_git_ref {
            return None;
        }
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

    async fn fire_webhook(&self, event: &str, data: serde_json::Value) {
        if let Some(url) = &self.config().webhook_url {
            let project_name = if let Some(pid) = *self.inner.current_project_id.read().await {
                if let Ok(Some(project)) = db::projects::get_project(self.pool(), pid).await {
                    project.name
                } else {
                    self.config().default_project_name.clone()
                }
            } else {
                self.config().default_project_name.clone()
            };
            webhooks::fire(
                &self.inner.http_client,
                url,
                &self.config().webhook_events,
                event,
                &project_name,
                data,
            );
        }
    }

    fn reset_session_counter(&self) {
        self.inner.tool_call_count.store(0, Ordering::Relaxed);
    }

    /// Urgency score: base_priority + rejection_penalty + staleness.
    /// Returns (score, human-readable explanation).
    fn score_task(
        summary: &db::tasks::TaskSummary,
        now: chrono::DateTime<chrono::Utc>,
    ) -> (f64, String) {
        let base = match summary.priority.as_deref() {
            Some("P1") => 4.0,
            Some("P2") => 3.0,
            Some("P3") => 2.0,
            Some("P4") => 1.0,
            _ => 1.0,
        };
        let rejection_penalty = summary.rejected_attempts as f64 * 0.3;
        let staleness_days = (now - summary.created_at).num_hours() as f64 / 24.0;
        let staleness_score = staleness_days * 0.1;
        // blocked_dependents deferred until task_links table exists
        let total = base + rejection_penalty + staleness_score;

        let mut parts = Vec::new();
        if let Some(p) = &summary.priority {
            parts.push(format!("{p}={base:.1}"));
        }
        if summary.rejected_attempts > 0 {
            parts.push(format!("rejections={rejection_penalty:.1}"));
        }
        if staleness_days >= 1.0 {
            parts.push(format!("stale={staleness_score:.1}"));
        }
        let explanation = if parts.is_empty() {
            "base".to_string()
        } else {
            parts.join("+")
        };
        (total, explanation)
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
         3. SUBTASKS: if a task involves 3+ distinct steps, decompose it — call start_task(description, parent_task_id) for each subtask.\n\
         4. PROPOSING CODE: call propose_attempt(task_id, approach) BEFORE writing code to the user.\n\
         5. FAILURES: if the user reports an error, IMMEDIATELY call log_outcome(attempt_id, 'rejected', reasoning, code_snippet) BEFORE suggesting a fix.\n\
         6. OUTCOME RULES: Do NOT auto-accept. Only call log_outcome(attempt_id, 'accepted', reasoning, code_snippet) when the USER explicitly confirms success. If unsure, use 'pending'. Include code_snippet with the actual code written.\n\
         7. CONTEXT RECOVERY: if you feel lost or the user says 'try something else', call review_ledger(task_id) to read past failures so you don't repeat them.\n\
         8. PERIODIC CHECK: call get_active_context() every ~5 messages to stay grounded.\n\
         9. COLD START: at the beginning of a new session, call get_next_steps() for a briefing on pending work.\n\
         10. If unsure what to do next, call get_protocol() to re-read these rules.\n\
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
        #[tool(param)]
        #[schemars(description = "Optional tags for categorizing the rule")]
        tags: Option<Vec<String>>,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("content", &content, 4096)?;
        let project_id = self.project_id().await?;
        let cat = Self::parse_rule_category(&category)?;
        let embedding = self.embed(&content).await?;

        // Check for near-duplicates (cosine similarity >= 0.95)
        let duplicates = db::semantic::find_duplicates(
            self.pool(),
            project_id,
            &embedding,
            db::semantic::SIMILARITY_DEDUP_THRESHOLD,
        )
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

        // Check for potential contradictions (CONTRADICTION_FLOOR..DEDUP_THRESHOLD band)
        let contradictions =
            db::semantic::find_potential_contradictions(self.pool(), project_id, &embedding)
                .await
                .map_err(Self::db_err)?;

        let tags_vec = tags.unwrap_or_default();
        let id = db::semantic::create_rule(
            self.pool(),
            project_id,
            cat,
            &content,
            Some(&embedding),
            &tags_vec,
        )
        .await
        .map_err(Self::db_err)?;
        self.inner.cache.invalidate_search();

        if !contradictions.is_empty() {
            let conflict_rules: Vec<serde_json::Value> = contradictions
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "id": r.id.to_string(),
                        "content": r.content.chars().take(150).collect::<String>(),
                    })
                })
                .collect();
            return Self::json_content_with_nudge(
                &serde_json::json!({
                    "rule_id": id.to_string(),
                    "contradiction_warning": true,
                    "potentially_conflicting_rules": conflict_rules,
                }),
                "Rule stored, but potentially conflicting rules found. Review them — use forget_rule or update_rule to resolve contradictions.",
            );
        }

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
        #[tool(param)]
        #[schemars(
            description = "Filter by tags (AND semantics — rules must have ALL specified tags)"
        )]
        tags: Option<Vec<String>>,
        #[tool(param)]
        #[schemars(description = "Search across all projects (default false)")]
        cross_project: Option<bool>,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("query", &query, 2048)?;
        let current_project_id = self.project_id().await?;
        let project_id = if cross_project.unwrap_or(false) {
            None
        } else {
            Some(current_project_id)
        };
        let embedding = self.embed(&query).await?;
        let cat = category
            .as_deref()
            .map(Self::parse_rule_category)
            .transpose()?;
        let rules = db::semantic::search_rules_hybrid(
            self.pool(),
            project_id,
            current_project_id,
            &embedding,
            &query,
            limit.unwrap_or(10),
            cat,
            None,
            tags.as_deref(),
        )
        .await
        .map_err(Self::db_err)?;
        Self::json_content_with_nudge(&rules, "Apply these rules to your current task.")
    }

    #[tool(
        description = "Delete or supersede a rule. If supersede=true, marks rule as superseded (sets valid_until) instead of deleting — preserving history."
    )]
    pub async fn forget_rule(
        &self,
        #[tool(param)]
        #[schemars(description = "UUID of the rule to delete or supersede")]
        rule_id: String,
        #[tool(param)]
        #[schemars(
            description = "If true, mark rule as superseded instead of deleting (default false)"
        )]
        supersede: Option<bool>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let id = Self::parse_uuid(&rule_id)?;
        if supersede.unwrap_or(false) {
            let superseded = db::semantic::supersede_rule(self.pool(), id)
                .await
                .map_err(Self::db_err)?;
            self.inner.cache.invalidate_search();
            Self::json_content_with_nudge(
                &serde_json::json!({ "superseded": superseded }),
                "Rule superseded (valid_until set). It will no longer appear in searches.",
            )
        } else {
            let deleted = db::semantic::delete_rule(self.pool(), id)
                .await
                .map_err(Self::db_err)?;
            self.inner.cache.invalidate_search();
            Self::json_content_with_nudge(
                &serde_json::json!({ "deleted": deleted }),
                "Rule removed. Continue with your current task.",
            )
        }
    }

    #[tool(description = "List all rules, optionally filtered by category")]
    pub async fn list_rules(
        &self,
        #[tool(param)]
        #[schemars(description = "Filter by category: preference, fact, constraint, or lesson")]
        category: Option<String>,
        #[tool(param)]
        #[schemars(
            description = "Filter by tags (AND semantics — rules must have ALL specified tags)"
        )]
        tags: Option<Vec<String>>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let project_id = self.project_id().await?;
        let cat = category
            .as_deref()
            .map(Self::parse_rule_category)
            .transpose()?;
        let rules = db::semantic::list_rules(self.pool(), project_id, cat, tags.as_deref())
            .await
            .map_err(Self::db_err)?;
        Self::json_content(&rules)
    }

    #[tool(
        description = "Find near-duplicate rules (cosine >= 0.88). Review the pairs and use forget_rule or update_rule to resolve."
    )]
    pub async fn get_duplicate_rules(
        &self,
        #[tool(param)]
        #[schemars(description = "Max pairs to return (default 20)")]
        limit: Option<i64>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let project_id = self.project_id().await?;
        let pairs =
            db::semantic::find_duplicate_clusters(self.pool(), project_id, limit.unwrap_or(20))
                .await
                .map_err(Self::db_err)?;
        if pairs.is_empty() {
            return Self::json_content_with_nudge(
                &serde_json::json!({ "duplicate_pairs": [] }),
                "No near-duplicate rules found.",
            );
        }
        Self::json_content_with_nudge(
            &serde_json::json!({ "duplicate_pairs": pairs }),
            "Review these pairs. Use forget_rule(supersede=true) to retire duplicates, or update_rule to merge content.",
        )
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
        #[tool(param)]
        #[schemars(description = "New tags for the rule (replaces existing tags)")]
        tags: Option<Vec<String>>,
    ) -> Result<CallToolResult, rmcp::Error> {
        if category.is_none() && content.is_none() && tags.is_none() {
            return Err(rmcp::Error::invalid_params(
                "Provide at least one of: category, content, tags",
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
            tags.as_deref(),
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
        #[tool(param)]
        #[schemars(description = "Priority level: P1, P2, P3, or P4")]
        priority: Option<String>,
        #[tool(param)]
        #[schemars(description = "Task type, e.g. Bug, Feature, Security, Refactor")]
        task_type: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("description", &description, 4096)?;
        let project_id = self.project_id().await?;
        let parent = parent_task_id
            .as_deref()
            .map(Self::parse_uuid)
            .transpose()?;
        let embedding = self.embed(&description).await.ok();
        let id = db::tasks::create_task(
            self.pool(),
            project_id,
            &description,
            parent,
            priority.as_deref(),
            task_type.as_deref(),
            embedding.as_deref(),
        )
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
        #[schemars(description = "Optional agent identifier for multi-agent workflows")]
        agent_id: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("approach_summary", &approach_summary, 4096)?;
        let tid = Self::parse_uuid(&task_id)?;
        let git_ref = self.capture_git_ref().await;
        let id = db::attempts::create_attempt(
            self.pool(),
            tid,
            &approach_summary,
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
        #[tool(param)]
        #[schemars(
            description = "Optional code snippet — include the actual code that was written for this attempt"
        )]
        code_snippet: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("reasoning", &reasoning, 4096)?;
        if let Some(ref code) = code_snippet {
            Self::validate_len("code_snippet", code, 32768)?;
        }
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
            code_snippet.as_deref(),
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
                    )
                    .await;
                }
            }
        }

        // Auto-link rules to code chunks on accepted outcomes (best-effort)
        if out == db::AttemptOutcome::Accepted {
            if let Some(ref code) = code_snippet {
                if let Ok(Some(attempt)) = db::attempts::get_attempt(self.pool(), aid).await {
                    match self.embed(code).await {
                        Ok(code_emb) => {
                            if let Ok(project_id) = self.project_id().await {
                                let pool = self.pool().clone();
                                let task_id = attempt.task_id;
                                tokio::spawn(async move {
                                    match db::rule_chunk_links::link_rules_from_code(
                                        &pool, task_id, &code_emb, project_id, 0.80,
                                    )
                                    .await
                                    {
                                        Ok(n) => {
                                            if n > 0 {
                                                tracing::info!(
                                                    links = n,
                                                    "Auto-linked rules to code chunks"
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!(error = %e, "Failed to auto-link rules to code chunks");
                                        }
                                    }
                                });
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to embed code snippet for auto-linking");
                        }
                    }
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
        let links = db::task_links::get_links_for_task(self.pool(), tid)
            .await
            .map_err(Self::db_err)?;
        Self::json_content_with_nudge(
            &serde_json::json!({
                "attempts": attempts,
                "task_links": links,
            }),
            "Use the above failures to avoid repeating mistakes. Call propose_attempt with a new approach.",
        )
    }

    #[tool(
        description = "Create a relationship link between two tasks. Types: blocks, related_to, caused_by, duplicate_of."
    )]
    pub async fn link_tasks(
        &self,
        #[tool(param)]
        #[schemars(description = "UUID of the source task")]
        source_task_id: String,
        #[tool(param)]
        #[schemars(description = "UUID of the target task")]
        target_task_id: String,
        #[tool(param)]
        #[schemars(description = "Link type: blocks, related_to, caused_by, or duplicate_of")]
        link_type: String,
    ) -> Result<CallToolResult, rmcp::Error> {
        let source = Self::parse_uuid(&source_task_id)?;
        let target = Self::parse_uuid(&target_task_id)?;
        let valid_types = ["blocks", "related_to", "caused_by", "duplicate_of"];
        let lt = link_type.to_lowercase();
        if !valid_types.contains(&lt.as_str()) {
            return Err(rmcp::Error::invalid_params(
                format!(
                    "Invalid link_type '{lt}'. Must be one of: {}",
                    valid_types.join(", ")
                ),
                None,
            ));
        }
        if source == target {
            return Err(rmcp::Error::invalid_params(
                "Cannot link a task to itself",
                None,
            ));
        }
        let id = db::task_links::create_link(self.pool(), source, target, &lt)
            .await
            .map_err(Self::db_err)?;
        Self::json_content_with_nudge(
            &serde_json::json!({ "link_id": id.to_string() }),
            "Link created. Continue with your current task.",
        )
    }

    // -- Knowledge graph tools --

    #[tool(
        description = "Add an edge to the knowledge graph between two entities (e.g. concepts, files, modules)"
    )]
    pub async fn add_edge(
        &self,
        #[tool(param)]
        #[schemars(description = "Source entity name")]
        source_entity: String,
        #[tool(param)]
        #[schemars(description = "Target entity name")]
        target_entity: String,
        #[tool(param)]
        #[schemars(description = "Relationship type (e.g. depends_on, uses, related_to)")]
        edge_type: String,
        #[tool(param)]
        #[schemars(description = "Confidence score 0.0-1.0 (default 1.0)")]
        confidence: Option<f64>,
        #[tool(param)]
        #[schemars(description = "UUID of the task that produced this edge")]
        source_task_id: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("source_entity", &source_entity, 512)?;
        Self::validate_len("target_entity", &target_entity, 512)?;
        Self::validate_len("edge_type", &edge_type, 128)?;
        if let Some(c) = confidence {
            if !(0.0..=1.0).contains(&c) {
                return Err(rmcp::Error::invalid_params(
                    "confidence must be between 0.0 and 1.0",
                    None,
                ));
            }
        }
        let project_id = self.project_id().await?;
        let task_id = source_task_id
            .as_deref()
            .map(Self::parse_uuid)
            .transpose()?;
        let id = db::knowledge_edges::create_edge(
            self.pool(),
            project_id,
            &source_entity,
            &target_entity,
            &edge_type,
            confidence,
            task_id,
        )
        .await
        .map_err(Self::db_err)?;
        Self::json_content_with_nudge(
            &serde_json::json!({ "edge_id": id.to_string() }),
            "Edge added to knowledge graph. Continue with your current task.",
        )
    }

    #[tool(
        description = "Query neighbors of an entity in the knowledge graph. Supports multi-hop traversal via depth parameter."
    )]
    pub async fn query_neighbors(
        &self,
        #[tool(param)]
        #[schemars(description = "Entity name to find neighbors of")]
        entity: String,
        #[tool(param)]
        #[schemars(description = "Filter by edge type")]
        edge_type: Option<String>,
        #[tool(param)]
        #[schemars(description = "Traversal depth (default 1, max 5)")]
        depth: Option<u32>,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("entity", &entity, 512)?;
        let d = depth.unwrap_or(1).min(5);
        let project_id = self.project_id().await?;
        let edges = db::knowledge_edges::query_neighbors_bfs(
            self.pool(),
            project_id,
            &entity,
            edge_type.as_deref(),
            d,
        )
        .await
        .map_err(Self::db_err)?;
        Self::json_content(&edges)
    }

    #[tool(
        description = "Find the shortest path between two entities in the knowledge graph using BFS"
    )]
    pub async fn find_path(
        &self,
        #[tool(param)]
        #[schemars(description = "Starting entity name")]
        from_entity: String,
        #[tool(param)]
        #[schemars(description = "Target entity name")]
        to_entity: String,
        #[tool(param)]
        #[schemars(description = "Max traversal depth (default 5, max 10)")]
        max_depth: Option<u32>,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("from_entity", &from_entity, 512)?;
        Self::validate_len("to_entity", &to_entity, 512)?;
        let d = max_depth.unwrap_or(5).min(10);
        let project_id = self.project_id().await?;
        let path =
            db::knowledge_edges::find_path(self.pool(), project_id, &from_entity, &to_entity, d)
                .await
                .map_err(Self::db_err)?;
        if path.is_empty() {
            Self::json_content(&serde_json::json!({
                "path": [],
                "message": "No path found between entities"
            }))
        } else {
            Self::json_content(&serde_json::json!({
                "path": path,
                "hop_count": path.len()
            }))
        }
    }

    #[tool(description = "Update an existing task's priority, task_type, or description")]
    pub async fn update_task(
        &self,
        #[tool(param)]
        #[schemars(description = "UUID of the task to update")]
        task_id: String,
        #[tool(param)]
        #[schemars(description = "Priority level: P1, P2, P3, or P4. Empty string clears it.")]
        priority: Option<String>,
        #[tool(param)]
        #[schemars(description = "New task type (e.g. Bug, Feature). Empty string clears it.")]
        task_type: Option<String>,
        #[tool(param)]
        #[schemars(description = "New description text")]
        description: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        if let Some(ref d) = description {
            Self::validate_len("description", d, 4096)?;
        }
        let tid = Self::parse_uuid(&task_id)?;
        let p = priority.map(|v| {
            let trimmed = v.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        });
        let tt = task_type.map(|v| {
            let trimmed = v.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        });
        let desc = description
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());

        // Re-embed if description changed
        let desc_embedding = if desc.is_some() {
            match self.embed(desc.as_deref().unwrap()).await {
                Ok(emb) => Some(emb),
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to embed updated description");
                    None
                }
            }
        } else {
            None
        };

        let updated = db::tasks::update_task(
            self.pool(),
            tid,
            p.as_ref().map(|o| o.as_deref()),
            tt.as_ref().map(|o| o.as_deref()),
            desc.as_deref(),
            desc_embedding.as_deref(),
            None,
        )
        .await
        .map_err(Self::db_err)?;

        Self::json_content_with_nudge(
            &serde_json::json!({ "updated": updated, "task_id": task_id }),
            "Task updated. Continue with your current work.",
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
        #[tool(param)]
        #[schemars(
            description = "UUID of the accepted attempt that resolved this task. If omitted, auto-detects from the last accepted attempt."
        )]
        resolved_attempt_id: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        if let Some(ref l) = lesson {
            Self::validate_len("lesson", l, 4096)?;
        }
        let tid = Self::parse_uuid(&task_id)?;
        // Resolve the winning attempt: explicit param or last accepted
        let resolved = match resolved_attempt_id {
            Some(ref id) => Some(Self::parse_uuid(id)?),
            None => {
                db::attempts::list_attempts(self.pool(), tid, Some(db::AttemptOutcome::Accepted))
                    .await
                    .ok()
                    .and_then(|a| a.last().map(|a| a.id))
            }
        };
        let success = db::tasks::complete_task(self.pool(), tid, resolved)
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
                    &[],
                )
                .await
                .map_err(Self::db_err)?;
            }
        }

        let rolled_up = if success {
            db::tasks::try_rollup_parents(self.pool(), tid).await
        } else {
            0
        };

        if success {
            self.fire_webhook(
                "task_completed",
                serde_json::json!({ "task_id": task_id, "lesson": lesson, "parents_rolled_up": rolled_up }),
            )
            .await;
        }

        Self::json_content_with_nudge(
            &serde_json::json!({ "success": success, "parents_rolled_up": rolled_up }),
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
                &[],
            )
            .await
            .map_err(Self::db_err)?;
        }

        let rolled_up = if success {
            db::tasks::try_rollup_parents(self.pool(), tid).await
        } else {
            0
        };

        if success {
            self.fire_webhook(
                "task_abandoned",
                serde_json::json!({ "task_id": task_id, "reason": reason, "parents_rolled_up": rolled_up }),
            )
            .await;
        }

        Self::json_content_with_nudge(
            &serde_json::json!({ "success": success, "parents_rolled_up": rolled_up }),
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

    #[tool(description = "List subtasks of a parent task")]
    pub async fn list_subtasks(
        &self,
        #[tool(param)]
        #[schemars(description = "UUID of the parent task")]
        parent_task_id: String,
    ) -> Result<CallToolResult, rmcp::Error> {
        let pid = Self::parse_uuid(&parent_task_id)?;
        let subtasks = db::tasks::list_subtasks(self.pool(), pid)
            .await
            .map_err(Self::db_err)?;
        Self::json_content(&subtasks)
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

        let active_rules_count = db::semantic::count_rules(self.pool(), project_id)
            .await
            .map_err(Self::db_err)?;

        let context_wipes = if let Some(task) = active_tasks.first() {
            db::snapshots::count_snapshots(self.pool(), task.id)
                .await
                .map_err(Self::db_err)?
        } else {
            0
        };

        // Proactive retrieval: auto-surface relevant rules and similar failures
        // based on the active task's description embedding.
        let proactive_enabled = self.config().proactive_context;
        let mut proactive: Option<serde_json::Value> = None;
        if proactive_enabled {
            if let Some(task) = active_tasks.first() {
                let emb_vec: Option<Vec<f32>> = match task.description_embedding.as_ref() {
                    Some(v) => Some(v.to_vec()),
                    None => self.embed(&task.description).await.ok(),
                };
                if let Some(emb) = emb_vec {
                    let rules = db::semantic::search_rules_by_embedding(
                        self.pool(),
                        Some(project_id),
                        &emb,
                        5,
                        None,
                        None,
                        Some(project_id),
                    )
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "Proactive rule search failed");
                        vec![]
                    });
                    let failures = db::attempts::search_similar_failures(
                        self.pool(),
                        Some(project_id),
                        &emb,
                        3,
                    )
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "Proactive failure search failed");
                        vec![]
                    });
                    proactive = Some(serde_json::json!({
                        "relevant_rules": rules,
                        "similar_failures": failures,
                    }));
                }
            }
        }

        let nudge = if active_tasks.is_empty() {
            "No active task. Call start_task(description) for your current goal."
        } else {
            "Use the active task and attempts above to continue. Call propose_attempt for your next approach."
        };

        let mut body = serde_json::json!({
            "project": project,
            "active_tasks": active_tasks,
            "recent_attempts": recent_attempts,
            "active_rules_count": active_rules_count,
            "context_wipes": context_wipes,
        });
        if let Some(p) = proactive {
            body["proactively_retrieved"] = p;
        }
        Self::json_content_with_nudge(&body, nudge)
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
        let rules = db::semantic::list_rules(self.pool(), project_id, None, None)
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
    pub async fn get_next_steps(
        &self,
        #[tool(param)]
        #[schemars(description = "Context tier: L0 (minimal ~100 tokens), L1 (full, default)")]
        tier: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let project_id = self.project_id().await?;
        let project = db::projects::get_project(self.pool(), project_id)
            .await
            .map_err(Self::db_err)?;

        // L0: minimal orientation
        if tier.as_deref().map(|t| t.to_uppercase()).as_deref() == Some("L0") {
            let active_count =
                db::tasks::count_tasks(self.pool(), project_id, Some(db::TaskStatus::Active))
                    .await
                    .map_err(Self::db_err)?;
            let blocked_count =
                db::tasks::count_tasks(self.pool(), project_id, Some(db::TaskStatus::Blocked))
                    .await
                    .map_err(Self::db_err)?;
            let lesson_count = db::semantic::count_rules_by_category(
                self.pool(),
                project_id,
                db::RuleCategory::Lesson,
            )
            .await
            .map_err(Self::db_err)?;
            return Self::json_content_with_nudge(
                &serde_json::json!({
                    "project": project,
                    "active_task_count": active_count,
                    "blocked_task_count": blocked_count,
                    "lesson_count": lesson_count,
                }),
                "L0 brief loaded. Call get_next_steps(tier='L1') for full details, or start_task() for a new goal.",
            );
        }

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
            None,
        )
        .await
        .map_err(Self::db_err)?;
        let recent_lessons: Vec<_> = lessons.into_iter().rev().take(5).collect();

        // Score and rank tasks by urgency
        let now = chrono::Utc::now();
        let mut scored: Vec<_> = summaries
            .iter()
            .map(|s| {
                let (score, explanation) = Self::score_task(s, now);
                (s, score, explanation)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Build action items from scored order
        let mut actions: Vec<String> = Vec::new();
        for (s, score, explanation) in &scored {
            let action = match s.status {
                db::TaskStatus::Active if s.pending_attempts > 0 => {
                    format!(
                        "[score={score:.2}] Task '{}' has {} pending attempt(s) awaiting outcome resolution ({explanation})",
                        s.description, s.pending_attempts
                    )
                }
                db::TaskStatus::Active => {
                    format!(
                        "[score={score:.2}] Task '{}' is active ({} attempts, {} rejected) — propose next approach ({explanation})",
                        s.description, s.total_attempts, s.rejected_attempts
                    )
                }
                db::TaskStatus::Blocked => {
                    format!(
                        "[score={score:.2}] Task '{}' is BLOCKED — needs unblocking ({explanation})",
                        s.description
                    )
                }
                _ => continue,
            };
            actions.push(action);
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
        let lessons = db::semantic::list_rules(
            self.pool(),
            project_id,
            Some(db::RuleCategory::Lesson),
            None,
        )
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

    #[tool(
        description = "Index a project's codebase into vector storage for semantic code search. Scans files respecting .gitignore, chunks by language-aware boundaries, embeds via ONNX, stores in pgvector. Incremental: only re-indexes changed files (SHA-256 fingerprinting). Call at session start for fast code retrieval."
    )]
    pub async fn index_codebase(
        &self,
        #[tool(param)]
        #[schemars(
            description = "Root path of the project to index (defaults to project root_path)"
        )]
        root_path: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let project_id = self.project_id().await?;

        let project = db::projects::get_project(self.pool(), project_id)
            .await
            .map_err(Self::db_err)?
            .ok_or_else(|| rmcp::Error::internal_error("Project not found", None))?;

        let path = if let Some(ref p) = root_path {
            // Restrict to subdirectories of the project's registered root
            let canonical = std::path::Path::new(p)
                .canonicalize()
                .map_err(|e| rmcp::Error::internal_error(format!("Invalid path: {e}"), None))?;
            let project_root = std::path::Path::new(&project.root_path)
                .canonicalize()
                .map_err(|e| {
                    rmcp::Error::internal_error(format!("Invalid project root: {e}"), None)
                })?;
            if !canonical.starts_with(&project_root) {
                return Err(rmcp::Error::internal_error(
                    "root_path must be within the project directory",
                    None,
                ));
            }
            canonical.to_string_lossy().to_string()
        } else {
            project.root_path
        };

        let result = crate::indexer::index_codebase(
            self.pool(),
            self.embeddings(),
            project_id,
            &path,
            None,
            self.config().codebase_behavior_version,
        )
        .await
        .map_err(|e| rmcp::Error::internal_error(e.to_string(), None))?;

        Self::json_content_with_nudge(
            &result,
            "Codebase indexed. Use search_codebase to find relevant code.",
        )
    }

    #[tool(
        description = "Search the indexed codebase for relevant code chunks. Returns the most relevant code snippets matching your query using hybrid vector + keyword search with MMR diversity re-ranking. Much faster and cheaper than reading entire files."
    )]
    pub async fn search_codebase(
        &self,
        #[tool(param)]
        #[schemars(description = "Natural language query describing what code you're looking for")]
        query: String,
        #[tool(param)]
        #[schemars(description = "Max results to return (default 5)")]
        limit: Option<i64>,
        #[tool(param)]
        #[schemars(description = "Optional file path pattern filter (SQL LIKE, e.g. 'src/%.rs')")]
        file_pattern: Option<String>,
        #[tool(param)]
        #[schemars(
            description = "Result diversity via MMR re-ranking: 0.0=pure relevance, 1.0=max diversity (default 0.3)"
        )]
        diversity: Option<f32>,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("query", &query, 2048)?;
        let project_id = self.project_id().await?;
        let embedding = self.embed(&query).await?;

        let chunks = db::codebase::search_chunks(
            self.pool(),
            project_id,
            &embedding,
            &query,
            limit.unwrap_or(5),
            file_pattern.as_deref(),
            Some(diversity.unwrap_or(0.3)),
        )
        .await
        .map_err(Self::db_err)?;

        // Format results with file path and line numbers for easy navigation
        let results: Vec<serde_json::Value> = chunks
            .iter()
            .map(|c| {
                let mut obj = serde_json::json!({
                    "file": c.file_path,
                    "lines": format!("{}:{}", c.start_line, c.end_line),
                    "language": c.language,
                    "content": c.content,
                });
                if let Some(ref s) = c.summary {
                    obj["summary"] = serde_json::Value::String(s.clone());
                }
                obj
            })
            .collect();

        Self::json_content_with_nudge(
            &results,
            "Use these code snippets as context for your current task.",
        )
    }

    #[tool(
        description = "Get statistics about the indexed codebase: file count, chunk count, last indexed time, summary coverage."
    )]
    pub async fn get_index_status(&self) -> Result<CallToolResult, rmcp::Error> {
        let project_id = self.project_id().await?;
        let stats = db::codebase::get_index_stats(self.pool(), project_id)
            .await
            .map_err(Self::db_err)?;
        Self::json_content_with_nudge(&stats, "Call index_codebase to update the index if stale.")
    }

    #[tool(
        description = "Get semantic rules linked to code chunks in a given file. Returns rules that were auto-linked via embedding similarity when code was accepted."
    )]
    pub async fn get_rules_for_file(
        &self,
        #[tool(param)]
        #[schemars(
            description = "File path to look up (must match indexed code_chunks file_path)"
        )]
        file_path: String,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("file_path", &file_path, 1024)?;
        let project_id = self.project_id().await?;
        let rules = db::rule_chunk_links::get_rules_for_file(self.pool(), &file_path, project_id)
            .await
            .map_err(Self::db_err)?;
        if rules.is_empty() {
            return Self::json_content_with_nudge(
                &serde_json::json!({ "rules": [], "file_path": file_path }),
                "No rules linked to this file. Index the codebase and accept attempts with code snippets to build links.",
            );
        }
        Self::json_content_with_nudge(
            &serde_json::json!({ "rules": rules, "file_path": file_path }),
            "Apply these rules when modifying this file.",
        )
    }

    #[tool(
        description = "Generate LLM summaries for indexed code chunks that lack them. Calls Gemini to produce 1-sentence descriptions per function/class. Run after index_codebase to improve high-level search queries."
    )]
    pub async fn generate_summaries(
        &self,
        #[tool(param)]
        #[schemars(description = "Max chunks to summarize per call (default 50, max 100)")]
        limit: Option<i64>,
    ) -> Result<CallToolResult, rmcp::Error> {
        let project_id = self.project_id().await?;
        let limit = limit.unwrap_or(50).min(100);

        let chunks = db::codebase::get_chunks_needing_summary(self.pool(), project_id, limit)
            .await
            .map_err(Self::db_err)?;

        if chunks.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "All chunks already have summaries.",
            )]));
        }

        let api_key = self
            .config()
            .gemini_api_key
            .as_deref()
            .filter(|k| !k.is_empty())
            .ok_or_else(|| {
                rmcp::Error::internal_error("GEMINI_API_KEY required for generate_summaries", None)
            })?;

        let mut updates: Vec<(Uuid, String)> = Vec::new();
        let mut errors = 0u64;

        // Batch chunks into groups of 10 for efficient LLM calls
        for batch in chunks.chunks(10) {
            let prompt = build_summary_prompt(batch);
            match call_gemini_for_summaries(&self.inner.http_client, api_key, &prompt).await {
                Ok(summaries) => {
                    for (chunk, summary) in batch.iter().zip(summaries) {
                        if !summary.is_empty() {
                            updates.push((chunk.id, summary));
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "LLM summary batch failed");
                    errors += batch.len() as u64;
                }
            }
        }

        let updated = db::codebase::update_summaries(self.pool(), &updates)
            .await
            .map_err(Self::db_err)?;

        Self::json_content_with_nudge(
            &serde_json::json!({
                "summaries_generated": updated,
                "errors": errors,
                "remaining": chunks.len() as u64 - updated - errors,
            }),
            "Summaries improve search quality for high-level queries.",
        )
    }

    // -- Codebase edge tools --

    #[tool(
        description = "Find all callers of a function/method in the codebase edge graph. Returns entities that call (or reference) the given entity, with source file info."
    )]
    pub async fn find_callers(
        &self,
        #[tool(param)]
        #[schemars(description = "Function or method name to find callers of")]
        entity: String,
        #[tool(param)]
        #[schemars(description = "Edge type filter (default: 'calls')")]
        edge_type: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("entity", &entity, 512)?;
        let project_id = self.project_id().await?;
        let _edge_type = edge_type.unwrap_or_else(|| "calls".to_string());
        let edges = db::codebase_edges::get_callers(self.pool(), project_id, &entity)
            .await
            .map_err(Self::db_err)?;
        Self::json_content_with_nudge(
            &serde_json::json!({
                "entity": entity,
                "callers": edges.iter().map(|e| serde_json::json!({
                    "caller": e.source_entity,
                    "edge_type": e.edge_type,
                    "source_file": e.source_file,
                })).collect::<Vec<_>>(),
                "count": edges.len(),
            }),
            "Use find_callees to see what this entity calls.",
        )
    }

    #[tool(
        description = "Find all callees of a function/method in the codebase edge graph. Returns entities that the given entity calls (or references), with target file info."
    )]
    pub async fn find_callees(
        &self,
        #[tool(param)]
        #[schemars(description = "Function or method name to find callees of")]
        entity: String,
        #[tool(param)]
        #[schemars(description = "Edge type filter (default: 'calls')")]
        edge_type: Option<String>,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("entity", &entity, 512)?;
        let project_id = self.project_id().await?;
        let _edge_type = edge_type.unwrap_or_else(|| "calls".to_string());
        let edges = db::codebase_edges::get_callees(self.pool(), project_id, &entity)
            .await
            .map_err(Self::db_err)?;
        Self::json_content_with_nudge(
            &serde_json::json!({
                "entity": entity,
                "callees": edges.iter().map(|e| serde_json::json!({
                    "callee": e.target_entity,
                    "edge_type": e.edge_type,
                    "target_file": e.target_file,
                })).collect::<Vec<_>>(),
                "count": edges.len(),
            }),
            "Use find_callers to see what calls this entity.",
        )
    }

    #[tool(
        description = "Find the shortest path between two entities in the codebase edge graph using BFS. Useful for understanding how two functions/modules are connected through call chains."
    )]
    pub async fn shortest_code_path(
        &self,
        #[tool(param)]
        #[schemars(description = "Starting entity (function/method name)")]
        from: String,
        #[tool(param)]
        #[schemars(description = "Target entity (function/method name)")]
        to: String,
        #[tool(param)]
        #[schemars(description = "Max traversal depth (default 5, max 10)")]
        max_depth: Option<i32>,
    ) -> Result<CallToolResult, rmcp::Error> {
        Self::validate_len("from", &from, 512)?;
        Self::validate_len("to", &to, 512)?;
        let depth = max_depth.unwrap_or(5).min(10);
        let project_id = self.project_id().await?;
        let path =
            db::codebase_edges::find_shortest_path(self.pool(), project_id, &from, &to, depth)
                .await
                .map_err(Self::db_err)?;
        if path.is_empty() {
            Self::json_content_with_nudge(
                &serde_json::json!({
                    "from": from,
                    "to": to,
                    "path": [],
                    "message": "No path found between these entities within the depth limit"
                }),
                "Use find_callers/find_callees to explore individual nodes.",
            )
        } else {
            Self::json_content_with_nudge(
                &serde_json::json!({
                    "from": from,
                    "to": to,
                    "path": path.iter().map(|e| serde_json::json!({
                        "source": e.source_entity,
                        "target": e.target_entity,
                        "edge_type": e.edge_type,
                        "source_file": e.source_file,
                        "target_file": e.target_file,
                    })).collect::<Vec<_>>(),
                    "hop_count": path.len(),
                }),
                "Use find_callers/find_callees to explore individual nodes.",
            )
        }
    }
}

fn build_summary_prompt(chunks: &[db::codebase::CodeChunk]) -> String {
    let mut prompt = String::from(
        "For each numbered code snippet below, write exactly ONE short sentence (max 15 words) \
         describing what the code does. Return one summary per line, numbered to match.\n\n",
    );
    for (i, chunk) in chunks.iter().enumerate() {
        let lang = chunk.language.as_deref().unwrap_or("unknown");
        prompt.push_str(&format!(
            "--- Snippet {} ({}, {}:{}-{}) ---\n{}\n\n",
            i + 1,
            lang,
            chunk.file_path,
            chunk.start_line,
            chunk.end_line,
            chunk.content
        ));
    }
    prompt
}

async fn call_gemini_for_summaries(
    client: &reqwest::Client,
    api_key: &str,
    prompt: &str,
) -> Result<Vec<String>, String> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash:generateContent?key={}",
        api_key
    );

    let body = serde_json::json!({
        "contents": [{"parts": [{"text": prompt}]}],
        "generationConfig": {"temperature": 0.1, "maxOutputTokens": 2048}
    });

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Gemini request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Gemini API {status}: {text}"));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse Gemini response: {e}"))?;

    let text = json["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap_or("");

    let summaries: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            // Strip leading "1. " or "1) " numbering
            let trimmed = l.trim();
            if let Some(rest) = trimmed.strip_prefix(|c: char| c.is_ascii_digit()) {
                let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
                rest.trim_start_matches(['.', ')', ':', '-', ' '])
                    .to_string()
            } else {
                trimmed.to_string()
            }
        })
        .collect();

    Ok(summaries)
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
            tools.len() >= 28,
            "Expected at least 28 tools, got {}",
            tools.len()
        );
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(names.contains(&"remember_rule"));
        assert!(names.contains(&"forget_rule"));
        assert!(names.contains(&"generate_handoff"));
        assert!(names.contains(&"link_tasks"));
        assert!(names.contains(&"add_edge"));
        assert!(names.contains(&"query_neighbors"));
        assert!(names.contains(&"find_path"));
        assert!(names.contains(&"get_rules_for_file"));
        assert!(names.contains(&"find_callers"));
        assert!(names.contains(&"find_callees"));
        assert!(names.contains(&"shortest_code_path"));
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

    #[test]
    fn test_score_task_base_priority() {
        let now = chrono::Utc::now();
        let make = |priority: Option<&str>, rejected: i64| db::tasks::TaskSummary {
            id: uuid::Uuid::new_v4(),
            description: "test".into(),
            status: db::TaskStatus::Active,
            created_at: now,
            priority: priority.map(|s| s.to_string()),
            total_attempts: 0,
            pending_attempts: 0,
            rejected_attempts: rejected,
            accepted_attempts: 0,
        };

        let (s1, _) = LoreServer::score_task(&make(Some("P1"), 0), now);
        let (s2, _) = LoreServer::score_task(&make(Some("P2"), 0), now);
        let (s4, _) = LoreServer::score_task(&make(None, 0), now);
        assert!(s1 > s2);
        assert!(s2 > s4);

        // Rejections add 0.3 each
        let (sr, _) = LoreServer::score_task(&make(None, 3), now);
        assert!((sr - (1.0 + 0.9)).abs() < 0.01);
    }

    #[test]
    fn test_score_task_staleness() {
        let now = chrono::Utc::now();
        let old = now - chrono::Duration::days(10);
        let summary = db::tasks::TaskSummary {
            id: uuid::Uuid::new_v4(),
            description: "stale".into(),
            status: db::TaskStatus::Active,
            created_at: old,
            priority: None,
            total_attempts: 0,
            pending_attempts: 0,
            rejected_attempts: 0,
            accepted_attempts: 0,
        };
        let (score, explanation) = LoreServer::score_task(&summary, now);
        // base(1.0) + staleness(10 * 0.1 = 1.0) = 2.0
        assert!((score - 2.0).abs() < 0.1);
        assert!(explanation.contains("stale"));
    }
}
