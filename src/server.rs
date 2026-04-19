use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    service::RequestContext,
    tool, tool_router, Peer, RoleServer, ServerHandler,
};
use sqlx::PgPool;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::cache::LoreCache;
use crate::config::Config;
use crate::db;
use crate::elicit::{self, ConfirmOutcome};
use crate::embeddings::{AnyEmbeddingProvider, EmbeddingProvider};
use crate::webhooks;

struct PromptArg {
    name: &'static str,
    description: &'static str,
    required: bool,
}

struct PromptDef {
    name: &'static str,
    description: &'static str,
    arguments: &'static [PromptArg],
    render: fn(&serde_json::Map<String, serde_json::Value>) -> String,
}

impl PromptDef {
    fn to_prompt(&self) -> Prompt {
        let args: Vec<PromptArgument> = self
            .arguments
            .iter()
            .map(|a| PromptArgument {
                name: a.name.to_string(),
                title: None,
                description: Some(a.description.to_string()),
                required: Some(a.required),
            })
            .collect();
        Prompt::new::<&str, String>(
            self.name,
            Some(self.description.to_string()),
            if args.is_empty() { None } else { Some(args) },
        )
    }

    fn render(&self, args: &serde_json::Map<String, serde_json::Value>) -> String {
        (self.render)(args)
    }
}

fn arg_str(args: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    args.get(key)
        .and_then(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .or_else(|| Some(v.to_string()))
        })
        .unwrap_or_default()
}

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
    pub tool_router: ToolRouter<LoreServer>,
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
                tool_router: Self::tool_router(),
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

    pub async fn project_id(&self) -> Result<Uuid, rmcp::ErrorData> {
        if let Some(id) = *self.inner.current_project_id.read().await {
            return Ok(id);
        }
        // Auto-detect from cwd
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .map_err(|e| rmcp::ErrorData::internal_error(format!("Cannot read cwd: {e}"), None))?;
        let (id, _name) = db::projects::get_or_create_project_by_path(self.pool(), &cwd)
            .await
            .map_err(Self::db_err)?;
        self.set_project_id(id).await;
        Ok(id)
    }

    pub async fn set_project_id(&self, id: Uuid) {
        *self.inner.current_project_id.write().await = Some(id);
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, rmcp::ErrorData> {
        let key = text.to_string();
        if let Some(cached) = self.inner.cache.embeddings.get(&key).await {
            return Ok((*cached).clone());
        }
        let result =
            self.embeddings().embed(text).await.map_err(|e| {
                rmcp::ErrorData::internal_error(format!("Embedding error: {e}"), None)
            })?;
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

    fn parse_rule_category(s: &str) -> Result<db::RuleCategory, rmcp::ErrorData> {
        match s.to_lowercase().as_str() {
            "preference" => Ok(db::RuleCategory::Preference),
            "fact" => Ok(db::RuleCategory::Fact),
            "constraint" => Ok(db::RuleCategory::Constraint),
            "lesson" => Ok(db::RuleCategory::Lesson),
            other => Err(rmcp::ErrorData::invalid_params(
                format!(
                    "Invalid rule category: '{other}'. Valid: preference, fact, constraint, lesson"
                ),
                None,
            )),
        }
    }

    fn parse_task_status(s: &str) -> Result<db::TaskStatus, rmcp::ErrorData> {
        match s.to_lowercase().as_str() {
            "active" => Ok(db::TaskStatus::Active),
            "completed" => Ok(db::TaskStatus::Completed),
            "abandoned" => Ok(db::TaskStatus::Abandoned),
            "blocked" => Ok(db::TaskStatus::Blocked),
            other => Err(rmcp::ErrorData::invalid_params(
                format!(
                    "Invalid task status: '{other}'. Valid: active, completed, abandoned, blocked"
                ),
                None,
            )),
        }
    }

    fn parse_attempt_outcome(s: &str) -> Result<db::AttemptOutcome, rmcp::ErrorData> {
        match s.to_lowercase().as_str() {
            "pending" => Ok(db::AttemptOutcome::Pending),
            "accepted" => Ok(db::AttemptOutcome::Accepted),
            "rejected" => Ok(db::AttemptOutcome::Rejected),
            "unknown" => Ok(db::AttemptOutcome::Unknown),
            other => Err(rmcp::ErrorData::invalid_params(
                format!("Invalid outcome: '{other}'. Valid: pending, accepted, rejected, unknown"),
                None,
            )),
        }
    }

    fn parse_uuid(s: &str) -> Result<Uuid, rmcp::ErrorData> {
        s.parse::<Uuid>()
            .map_err(|e| rmcp::ErrorData::invalid_params(format!("Invalid UUID '{s}': {e}"), None))
    }

    fn validate_len(field: &str, val: &str, max: usize) -> Result<(), rmcp::ErrorData> {
        if val.len() > max {
            return Err(rmcp::ErrorData::invalid_params(
                format!("{field} exceeds max length ({} > {max} bytes)", val.len()),
                None,
            ));
        }
        Ok(())
    }

    fn db_err(e: sqlx::Error) -> rmcp::ErrorData {
        rmcp::ErrorData::internal_error(format!("Database error: {e}"), None)
    }

    fn maybe_scrub(&self, input: String) -> String {
        if self.config().scrub_secrets {
            crate::scrubber::scrub(&input)
        } else {
            input
        }
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

    fn json_content<T: serde::Serialize>(val: &T) -> Result<CallToolResult, rmcp::ErrorData> {
        let json = serde_json::to_string_pretty(val).map_err(|e| {
            rmcp::ErrorData::internal_error(format!("Serialization error: {e}"), None)
        })?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Map a [`ConfirmOutcome`] into either a short-circuit tool result (user
    /// declined/cancelled/refused) or `Ok(None)` to proceed. Propagates
    /// protocol-level errors.
    ///
    /// `allow_not_supported` controls fallback on clients that do not
    /// advertise elicitation capability:
    /// - `true`  → proceed silently (safe for opt-in, non-destructive prompts).
    /// - `false` → short-circuit with a structured response so destructive
    ///   callers don't silently run on non-elicit clients. The caller can
    ///   retry with `force: true` to confirm intent.
    fn handle_confirm(
        outcome: ConfirmOutcome,
        context: &str,
        allow_not_supported: bool,
    ) -> Result<Option<CallToolResult>, rmcp::ErrorData> {
        match outcome {
            ConfirmOutcome::Confirmed { .. } => Ok(None),
            ConfirmOutcome::NotSupported if allow_not_supported => Ok(None),
            ConfirmOutcome::NotSupported => Self::json_content_with_nudge(
                &serde_json::json!({
                    "cancelled": true,
                    "outcome": "not_supported",
                }),
                "Client does not support MCP elicitation. Re-invoke with force=true to proceed without user confirmation.",
            )
            .map(Some),
            ConfirmOutcome::Refused { reason } => {
                let body = serde_json::json!({
                    "cancelled": true,
                    "outcome": "refused",
                    "reason": reason,
                });
                Self::json_content_with_nudge(&body, "User refused. No state was changed.")
                    .map(Some)
            }
            ConfirmOutcome::Declined => Self::json_content_with_nudge(
                &serde_json::json!({ "cancelled": true, "outcome": "declined" }),
                "User declined. No state was changed.",
            )
            .map(Some),
            ConfirmOutcome::Cancelled => Self::json_content_with_nudge(
                &serde_json::json!({ "cancelled": true, "outcome": "cancelled" }),
                "User dismissed the prompt. No state was changed.",
            )
            .map(Some),
            ConfirmOutcome::Error(e) => Err(rmcp::ErrorData::internal_error(
                format!("{context} elicitation failed: {e}"),
                None,
            )),
        }
    }

    fn json_content_with_nudge<T: serde::Serialize>(
        val: &T,
        next_step: &str,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let mut obj = serde_json::to_value(val).map_err(|e| {
            rmcp::ErrorData::internal_error(format!("Serialization error: {e}"), None)
        })?;
        if let Some(map) = obj.as_object_mut() {
            map.insert(
                "_next_step".into(),
                serde_json::Value::String(next_step.into()),
            );
        }
        let json = serde_json::to_string_pretty(&obj).map_err(|e| {
            rmcp::ErrorData::internal_error(format!("Serialization error: {e}"), None)
        })?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    fn prompt_catalog() -> Vec<PromptDef> {
        vec![
            PromptDef {
                name: "plan_task",
                description:
                    "Decompose a goal into a parent task plus subtasks following the Lore protocol.",
                arguments: &[PromptArg {
                    name: "goal",
                    description: "What the user wants to accomplish.",
                    required: true,
                }],
                render: |args| {
                    let goal = arg_str(args, "goal");
                    format!(
                        "New goal: {goal}\n\n\
                         Follow Lore protocol:\n\
                         1. Call start_task(description=\"{goal}\") first.\n\
                         2. If this involves 3+ distinct steps, decompose into subtasks via start_task(..., parent_task_id=<id>).\n\
                         3. For each subtask, call propose_attempt BEFORE writing code.\n\
                         4. Set priority (P1-P4) and task_type (Bug/Feature/Security/Refactor) where appropriate."
                    )
                },
            },
            PromptDef {
                name: "resume_work",
                description: "Cold-start briefing: pick the top pending task and continue.",
                arguments: &[],
                render: |_args| {
                    "Resuming session. Steps:\n\
                     1. Call get_next_steps() for the scored action list.\n\
                     2. Select the top-scored active task.\n\
                     3. Call review_ledger(task_id) if it has rejected attempts.\n\
                     4. Call propose_attempt(task_id, approach) before writing code."
                        .to_string()
                },
            },
            PromptDef {
                name: "review_ledger",
                description: "Inspect prior attempts for a task before proposing a new approach.",
                arguments: &[PromptArg {
                    name: "task_id",
                    description: "UUID of the task to review.",
                    required: true,
                }],
                render: |args| {
                    let task_id = arg_str(args, "task_id");
                    format!(
                        "Call review_ledger(task_id=\"{task_id}\") and analyse:\n\
                         - Rejected attempts: why did each fail? Identify the pattern.\n\
                         - Pending attempts: is any awaiting user confirmation?\n\
                         - Do NOT repeat an approach that was already rejected.\n\
                         Then call propose_attempt with a genuinely different strategy."
                    )
                },
            },
            PromptDef {
                name: "diagnose_failure",
                description: "Log a failed attempt and propose a corrective fix.",
                arguments: &[
                    PromptArg {
                        name: "task_id",
                        description: "UUID of the task.",
                        required: true,
                    },
                    PromptArg {
                        name: "error",
                        description: "Error message or failure description.",
                        required: true,
                    },
                ],
                render: |args| {
                    let task_id = arg_str(args, "task_id");
                    let error = arg_str(args, "error");
                    format!(
                        "Failure reported on task {task_id}.\n\
                         Error: {error}\n\n\
                         Protocol:\n\
                         1. Call log_outcome(attempt_id=<last_pending>, outcome=\"rejected\", reasoning=\"{error}\", code_snippet=<failing code>).\n\
                         2. Call review_ledger(task_id=\"{task_id}\") to cross-check prior failures.\n\
                         3. Call propose_attempt with a fix that addresses the root cause, not the symptom."
                    )
                },
            },
            PromptDef {
                name: "record_lesson",
                description: "Store a reusable lesson learned from a completed task.",
                arguments: &[
                    PromptArg {
                        name: "topic",
                        description: "Short topic tag for the lesson.",
                        required: true,
                    },
                    PromptArg {
                        name: "insight",
                        description: "What was learned and why it matters.",
                        required: true,
                    },
                ],
                render: |args| {
                    let topic = arg_str(args, "topic");
                    let insight = arg_str(args, "insight");
                    format!(
                        "Call remember_rule(category=\"lesson\", content=\"[{topic}] {insight}\"). \
                         Make the content self-contained: future sessions will retrieve it without surrounding context, so include the trigger condition and the corrective action."
                    )
                },
            },
        ]
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

// -- Tool parameter structs (rmcp 0.10 Parameters wrapper) --

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RememberRuleParams {
    #[schemars(description = "Rule category: preference, fact, constraint, or lesson")]
    pub category: String,
    #[schemars(description = "The rule content to remember")]
    pub content: String,
    #[schemars(description = "Optional tags for categorizing the rule")]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RecallRulesParams {
    #[schemars(description = "Search query")]
    pub query: String,
    #[schemars(description = "Max results (default 10)")]
    pub limit: Option<i64>,
    #[schemars(description = "Filter by category: preference, fact, constraint, or lesson")]
    pub category: Option<String>,
    #[schemars(
        description = "Filter by tags (AND semantics — rules must have ALL specified tags)"
    )]
    pub tags: Option<Vec<String>>,
    #[schemars(description = "Search across all projects (default false)")]
    pub cross_project: Option<bool>,
    #[schemars(
        description = "If true, return compact previews (id, category, first 80 chars, score, tags, hit_count, last_used_at) instead of full content. Use get_rule(id) to fetch full details."
    )]
    pub compact: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetRuleParams {
    #[schemars(description = "UUID of the rule to fetch")]
    pub rule_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ForgetRuleParams {
    #[schemars(description = "UUID of the rule to delete or supersede")]
    pub rule_id: String,
    #[schemars(
        description = "If true, mark rule as superseded instead of deleting (default false)"
    )]
    pub supersede: Option<bool>,
    #[schemars(
        description = "If true, skip the interactive elicitation confirmation prompt and delete/supersede immediately (default false). Required on clients that do not support MCP elicitation — otherwise the tool returns a `not_supported` cancellation."
    )]
    pub force: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListRulesParams {
    #[schemars(description = "Filter by category: preference, fact, constraint, or lesson")]
    pub category: Option<String>,
    #[schemars(
        description = "Filter by tags (AND semantics — rules must have ALL specified tags)"
    )]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetDuplicateRulesParams {
    #[schemars(description = "Max pairs to return (default 20)")]
    pub limit: Option<i64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateRuleParams {
    #[schemars(description = "UUID of the rule to update")]
    pub rule_id: String,
    #[schemars(description = "New category: preference, fact, constraint, or lesson")]
    pub category: Option<String>,
    #[schemars(description = "New content for the rule")]
    pub content: Option<String>,
    #[schemars(description = "New tags for the rule (replaces existing tags)")]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StartTaskParams {
    #[schemars(description = "Description of the task")]
    pub description: String,
    #[schemars(description = "UUID of parent task, if this is a subtask")]
    pub parent_task_id: Option<String>,
    #[schemars(description = "Priority level: P1, P2, P3, or P4")]
    pub priority: Option<String>,
    #[schemars(description = "Task type, e.g. Bug, Feature, Security, Refactor")]
    pub task_type: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ProposeAttemptParams {
    #[schemars(description = "UUID of the task")]
    pub task_id: String,
    #[schemars(description = "Summary of the approach being attempted")]
    pub approach_summary: String,
    #[schemars(description = "Optional agent identifier for multi-agent workflows")]
    pub agent_id: Option<String>,
    #[schemars(
        description = "If true, ask the user to confirm the approach via MCP elicitation before persisting the attempt. This is an LLM-initiated review request — use it when you want an explicit human sign-off on your plan. Default false. Silently skipped when the client does not support elicitation."
    )]
    pub request_confirmation: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LogOutcomeParams {
    #[schemars(description = "UUID of the attempt")]
    pub attempt_id: String,
    #[schemars(
        description = "Outcome: pending (awaiting user confirmation), accepted (user confirmed), rejected (user reported failure), or unknown (stale/abandoned)"
    )]
    pub outcome: String,
    #[schemars(description = "Reasoning for the outcome")]
    pub reasoning: String,
    #[schemars(description = "Optional git reference (commit hash, branch)")]
    pub git_ref: Option<String>,
    #[schemars(
        description = "Optional code snippet — include the actual code that was written for this attempt"
    )]
    pub code_snippet: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReviewLedgerParams {
    #[schemars(description = "UUID of the task")]
    pub task_id: String,
    #[schemars(description = "Filter by outcome: pending, accepted, rejected, or unknown")]
    pub outcome_filter: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LinkTasksParams {
    #[schemars(description = "UUID of the source task")]
    pub source_task_id: String,
    #[schemars(description = "UUID of the target task")]
    pub target_task_id: String,
    #[schemars(description = "Link type: blocks, related_to, caused_by, or duplicate_of")]
    pub link_type: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddEdgeParams {
    #[schemars(description = "Source entity name")]
    pub source_entity: String,
    #[schemars(description = "Target entity name")]
    pub target_entity: String,
    #[schemars(description = "Relationship type (e.g. depends_on, uses, related_to)")]
    pub edge_type: String,
    #[schemars(description = "Confidence score 0.0-1.0 (default 1.0)")]
    pub confidence: Option<f64>,
    #[schemars(description = "UUID of the task that produced this edge")]
    pub source_task_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct QueryNeighborsParams {
    #[schemars(description = "Entity name to find neighbors of")]
    pub entity: String,
    #[schemars(description = "Filter by edge type")]
    pub edge_type: Option<String>,
    #[schemars(description = "Traversal depth (default 1, max 5)")]
    pub depth: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FindPathParams {
    #[schemars(description = "Starting entity name")]
    pub from_entity: String,
    #[schemars(description = "Target entity name")]
    pub to_entity: String,
    #[schemars(description = "Max traversal depth (default 5, max 10)")]
    pub max_depth: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateTaskParams {
    #[schemars(description = "UUID of the task to update")]
    pub task_id: String,
    #[schemars(description = "Priority level: P1, P2, P3, or P4. Empty string clears it.")]
    pub priority: Option<String>,
    #[schemars(description = "New task type (e.g. Bug, Feature). Empty string clears it.")]
    pub task_type: Option<String>,
    #[schemars(description = "New description text")]
    pub description: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CompleteTaskParams {
    #[schemars(description = "UUID of the task")]
    pub task_id: String,
    #[schemars(description = "Lesson learned from this task (saved as a Lesson rule)")]
    pub lesson: Option<String>,
    #[schemars(
        description = "UUID of the accepted attempt that resolved this task. If omitted, auto-detects from the last accepted attempt."
    )]
    pub resolved_attempt_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AbandonTaskParams {
    #[schemars(description = "UUID of the task to abandon")]
    pub task_id: String,
    #[schemars(description = "Why this task is being abandoned")]
    pub reason: String,
    #[schemars(description = "If true, save the reason as a Lesson rule")]
    pub save_lesson: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListTasksParams {
    #[schemars(description = "Filter by status: active, completed, abandoned, or blocked")]
    pub status: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListSubtasksParams {
    #[schemars(description = "UUID of the parent task")]
    pub parent_task_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetTaskStatsParams {
    #[schemars(description = "Filter by status: active, completed, abandoned, or blocked")]
    pub status: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FindSimilarFailuresParams {
    #[schemars(description = "Description of the error or failure")]
    pub error_description: String,
    #[schemars(description = "Max results (default 5)")]
    pub limit: Option<i64>,
    #[schemars(description = "Search across all projects (default false)")]
    pub cross_project: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LogContextWipeParams {
    #[schemars(description = "UUID of the active task")]
    pub task_id: String,
    #[schemars(description = "Approximate token count before the wipe")]
    pub token_count: i32,
    #[schemars(description = "UUID of the last attempt before the wipe")]
    pub last_attempt_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SwitchProjectParams {
    #[schemars(description = "Project name")]
    pub name: Option<String>,
    #[schemars(description = "Project root filesystem path")]
    pub root_path: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExportMemoryParams {
    #[schemars(description = "Export format: json or markdown")]
    pub format: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetNextStepsParams {
    #[schemars(description = "Context tier: L0 (minimal ~100 tokens), L1 (full, default)")]
    pub tier: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GenerateHandoffParams {
    #[schemars(description = "Approximate token count consumed in current session")]
    pub token_count: Option<i32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GenerateSessionSummaryParams {
    #[schemars(description = "Max tasks to include (default 10, max 25)")]
    pub max_tasks: Option<i64>,
    #[schemars(description = "Max attempts per task to include (default 5, max 10)")]
    pub max_attempts_per_task: Option<i64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct IndexCodebaseParams {
    #[schemars(description = "Root path of the project to index (defaults to project root_path)")]
    pub root_path: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchCodebaseParams {
    #[schemars(description = "Natural language query describing what code you're looking for")]
    pub query: String,
    #[schemars(description = "Max results to return (default 5)")]
    pub limit: Option<i64>,
    #[schemars(description = "Optional file path pattern filter (SQL LIKE, e.g. 'src/%.rs')")]
    pub file_pattern: Option<String>,
    #[schemars(
        description = "Result diversity via MMR re-ranking: 0.0=pure relevance, 1.0=max diversity (default 0.3)"
    )]
    pub diversity: Option<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetRulesForFileParams {
    #[schemars(description = "File path to look up (must match indexed code_chunks file_path)")]
    pub file_path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListChunksNeedingSummaryParams {
    #[schemars(description = "Max chunks to return per call (default 20, min 1, max 100)")]
    pub limit: Option<i64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SubmitChunkSummariesParams {
    #[schemars(description = "Chunk UUIDs from list_chunks_needing_summary. Max 100.")]
    pub ids: Vec<String>,
    #[schemars(description = "One-sentence summaries, aligned 1:1 with `ids`.")]
    pub summaries: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FindCallersParams {
    #[schemars(description = "Function or method name to find callers of")]
    pub entity: String,
    #[schemars(description = "Edge type filter (default: 'calls')")]
    pub edge_type: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FindCalleesParams {
    #[schemars(description = "Function or method name to find callees of")]
    pub entity: String,
    #[schemars(description = "Edge type filter (default: 'calls')")]
    pub edge_type: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ShortestCodePathParams {
    #[schemars(description = "Starting entity (function/method name)")]
    pub from: String,
    #[schemars(description = "Target entity (function/method name)")]
    pub to: String,
    #[schemars(description = "Max traversal depth (default 5, max 10)")]
    pub max_depth: Option<i32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetCommunityMembersParams {
    #[schemars(description = "Community ID to inspect")]
    pub community_id: i32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DetectCrossCommunityChangesParams {
    #[schemars(description = "File paths that were changed (relative to project root)")]
    pub file_paths: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetFileContextParams {
    #[schemars(description = "File path (must match indexed code_chunks file_path)")]
    pub file_path: String,
}

// -- Memory tools --
#[tool_router]
impl LoreServer {
    #[tool(description = "Store a long-term rule/preference/fact/lesson in memory")]
    pub async fn remember_rule(
        &self,
        Parameters(RememberRuleParams {
            category,
            content,
            tags,
        }): Parameters<RememberRuleParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let content = self.maybe_scrub(content);
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
        Parameters(RecallRulesParams {
            query,
            limit,
            category,
            tags,
            cross_project,
            compact,
        }): Parameters<RecallRulesParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        let scored_rules = db::semantic::search_rules_hybrid(
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

        if compact.unwrap_or(false) {
            let compact_results: Vec<serde_json::Value> = scored_rules
                .iter()
                .map(|sr| {
                    let preview: String = sr.rule.content.chars().take(80).collect();
                    serde_json::json!({
                        "id": sr.rule.id.to_string(),
                        "category": serde_json::to_value(&sr.rule.category).unwrap_or_default(),
                        "preview": preview,
                        "score": sr.score,
                        "tags": sr.rule.tags,
                        "hit_count": sr.rule.hit_count,
                        "last_used_at": sr.rule.last_used_at,
                    })
                })
                .collect();
            return Self::json_content_with_nudge(
                &compact_results,
                "Use get_rule(id) to fetch full content for specific rules.",
            );
        }

        Self::json_content_with_nudge(&scored_rules, "Apply these rules to your current task.")
    }

    #[tool(
        description = "Fetch a single rule by ID with full content. Also increments its hit count. Use after recall_rules(compact=true) to drill into specific rules."
    )]
    pub async fn get_rule(
        &self,
        Parameters(GetRuleParams { rule_id }): Parameters<GetRuleParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let id = Self::parse_uuid(&rule_id)?;
        let rule = db::semantic::get_rule(self.pool(), id)
            .await
            .map_err(Self::db_err)?
            .ok_or_else(|| {
                rmcp::ErrorData::invalid_params(format!("Rule not found: {rule_id}"), None)
            })?;
        db::semantic::increment_hit_counts(self.pool(), &[id]);
        Self::json_content_with_nudge(&rule, "Apply this rule to your current task.")
    }

    #[tool(
        description = "Delete or supersede a rule. If supersede=true, marks rule as superseded (sets valid_until) instead of deleting — preserving history."
    )]
    pub async fn forget_rule(
        &self,
        peer: Peer<RoleServer>,
        Parameters(params): Parameters<ForgetRuleParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if !params.force.unwrap_or(false) {
            let msg = if params.supersede.unwrap_or(false) {
                format!(
                    "Confirm supersede of rule {}. The rule will be hidden from search but preserved in history.",
                    params.rule_id
                )
            } else {
                format!(
                    "Confirm delete of rule {}. This cannot be undone.",
                    params.rule_id
                )
            };
            let ctx = format!("forget_rule({})", params.rule_id);
            if let Some(early) =
                Self::handle_confirm(elicit::confirm(&peer, msg).await, &ctx, false)?
            {
                return Ok(early);
            }
        }
        self.forget_rule_impl(params).await
    }

    pub async fn forget_rule_impl(
        &self,
        ForgetRuleParams {
            rule_id, supersede, ..
        }: ForgetRuleParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        Parameters(ListRulesParams { category, tags }): Parameters<ListRulesParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        Parameters(GetDuplicateRulesParams { limit }): Parameters<GetDuplicateRulesParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        Parameters(UpdateRuleParams {
            rule_id,
            category,
            content,
            tags,
        }): Parameters<UpdateRuleParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if category.is_none() && content.is_none() && tags.is_none() {
            return Err(rmcp::ErrorData::invalid_params(
                "Provide at least one of: category, content, tags",
                None,
            ));
        }
        let content = content.map(|c| self.maybe_scrub(c));
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
        Parameters(StartTaskParams {
            description,
            parent_task_id,
            priority,
            task_type,
        }): Parameters<StartTaskParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let description = self.maybe_scrub(description);
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
        peer: Peer<RoleServer>,
        Parameters(params): Parameters<ProposeAttemptParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if params.request_confirmation.unwrap_or(false) {
            let msg = format!(
                "Proposed approach:\n\n{}\n\nConfirm to proceed with code generation.",
                params.approach_summary
            );
            // Non-destructive opt-in: fall through silently on clients that
            // don't support elicitation.
            if let Some(early) =
                Self::handle_confirm(elicit::confirm(&peer, msg).await, "propose_attempt", true)?
            {
                return Ok(early);
            }
        }
        self.propose_attempt_impl(params).await
    }

    pub async fn propose_attempt_impl(
        &self,
        ProposeAttemptParams {
            task_id,
            approach_summary,
            agent_id,
            ..
        }: ProposeAttemptParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let approach_summary = self.maybe_scrub(approach_summary);
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
        Parameters(LogOutcomeParams {
            attempt_id,
            outcome,
            reasoning,
            git_ref,
            code_snippet,
        }): Parameters<LogOutcomeParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let reasoning = self.maybe_scrub(reasoning);
        let code_snippet = code_snippet.map(|cs| self.maybe_scrub(cs));
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
        Parameters(ReviewLedgerParams {
            task_id,
            outcome_filter,
        }): Parameters<ReviewLedgerParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        Parameters(LinkTasksParams {
            source_task_id,
            target_task_id,
            link_type,
        }): Parameters<LinkTasksParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let source = Self::parse_uuid(&source_task_id)?;
        let target = Self::parse_uuid(&target_task_id)?;
        let valid_types = ["blocks", "related_to", "caused_by", "duplicate_of"];
        let lt = link_type.to_lowercase();
        if !valid_types.contains(&lt.as_str()) {
            return Err(rmcp::ErrorData::invalid_params(
                format!(
                    "Invalid link_type '{lt}'. Must be one of: {}",
                    valid_types.join(", ")
                ),
                None,
            ));
        }
        if source == target {
            return Err(rmcp::ErrorData::invalid_params(
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
        Parameters(AddEdgeParams {
            source_entity,
            target_entity,
            edge_type,
            confidence,
            source_task_id,
        }): Parameters<AddEdgeParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Self::validate_len("source_entity", &source_entity, 512)?;
        Self::validate_len("target_entity", &target_entity, 512)?;
        Self::validate_len("edge_type", &edge_type, 128)?;
        if let Some(c) = confidence {
            if !(0.0..=1.0).contains(&c) {
                return Err(rmcp::ErrorData::invalid_params(
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
        Parameters(QueryNeighborsParams {
            entity,
            edge_type,
            depth,
        }): Parameters<QueryNeighborsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        Parameters(FindPathParams {
            from_entity,
            to_entity,
            max_depth,
        }): Parameters<FindPathParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        Parameters(UpdateTaskParams {
            task_id,
            priority,
            task_type,
            description,
        }): Parameters<UpdateTaskParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let description = description.map(|d| self.maybe_scrub(d));
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
        Parameters(CompleteTaskParams {
            task_id,
            lesson,
            resolved_attempt_id,
        }): Parameters<CompleteTaskParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let lesson = lesson.map(|l| self.maybe_scrub(l));
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
        Parameters(AbandonTaskParams {
            task_id,
            reason,
            save_lesson,
        }): Parameters<AbandonTaskParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let reason = self.maybe_scrub(reason);
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
        Parameters(ListTasksParams { status }): Parameters<ListTasksParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        Parameters(ListSubtasksParams { parent_task_id }): Parameters<ListSubtasksParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        Parameters(GetTaskStatsParams { status }): Parameters<GetTaskStatsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        Parameters(FindSimilarFailuresParams {
            error_description,
            limit,
            cross_project,
        }): Parameters<FindSimilarFailuresParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
    pub async fn get_active_context(&self) -> Result<CallToolResult, rmcp::ErrorData> {
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
        Parameters(LogContextWipeParams {
            task_id,
            token_count,
            last_attempt_id,
        }): Parameters<LogContextWipeParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        Parameters(SwitchProjectParams { name, root_path }): Parameters<SwitchProjectParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        Parameters(ExportMemoryParams { format }): Parameters<ExportMemoryParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let fmt = format.to_lowercase();
        if fmt != "json" && fmt != "markdown" {
            return Err(rmcp::ErrorData::invalid_params(
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
        Parameters(GetNextStepsParams { tier }): Parameters<GetNextStepsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
    pub async fn get_protocol(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(CallToolResult::success(vec![Content::text(
            Self::protocol_text(),
        )]))
    }

    #[tool(
        description = "Generate a handoff packet for session transitions. Call this before context exhaustion to create a dense briefing that the next session can ingest via get_next_steps. Automatically logs a context wipe event."
    )]
    pub async fn generate_handoff(
        &self,
        Parameters(GenerateHandoffParams { token_count }): Parameters<GenerateHandoffParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        description = "Fetch a structured session briefing: active + blocked tasks with recent attempts, plus recent lessons. Returns raw data plus a `client_prompt` and JSON schema so the MCP client LLM can synthesize the summary itself (no external LLM needed). If the client wants to persist the distilled lessons, it should call `remember_rule` with category='lesson' afterward."
    )]
    pub async fn generate_session_summary(
        &self,
        Parameters(GenerateSessionSummaryParams {
            max_tasks,
            max_attempts_per_task,
        }): Parameters<GenerateSessionSummaryParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_id = self.project_id().await?;
        let max_tasks = max_tasks.unwrap_or(10).clamp(1, 25);
        let max_attempts = max_attempts_per_task.unwrap_or(5).clamp(1, 10);

        let active_tasks =
            db::tasks::list_tasks(self.pool(), project_id, Some(db::TaskStatus::Active))
                .await
                .map_err(Self::db_err)?;
        let blocked_tasks =
            db::tasks::list_tasks(self.pool(), project_id, Some(db::TaskStatus::Blocked))
                .await
                .map_err(Self::db_err)?;

        let mut task_contexts: Vec<TaskContext> = Vec::new();
        for task in active_tasks
            .iter()
            .chain(blocked_tasks.iter())
            .take(max_tasks as usize)
        {
            let attempts = db::attempts::list_attempts(self.pool(), task.id, None)
                .await
                .unwrap_or_default();
            let recent: Vec<AttemptSnippet> = attempts
                .iter()
                .rev()
                .take(max_attempts as usize)
                .rev()
                .map(|a| AttemptSnippet {
                    outcome: format!("{:?}", a.outcome).to_lowercase(),
                    approach: a.approach_summary.clone(),
                    reasoning: a.reasoning.clone(),
                })
                .collect();
            task_contexts.push(TaskContext {
                description: task.description.clone(),
                status: format!("{:?}", task.status).to_lowercase(),
                priority: task.priority.clone(),
                attempts: recent,
            });
        }

        let lessons = db::semantic::list_rules(
            self.pool(),
            project_id,
            Some(db::RuleCategory::Lesson),
            None,
        )
        .await
        .unwrap_or_default();
        let recent_lessons: Vec<String> = lessons
            .iter()
            .rev()
            .take(5)
            .map(|l| l.content.chars().take(240).collect::<String>())
            .collect();

        let tasks_json: Vec<serde_json::Value> = task_contexts
            .iter()
            .map(|t| {
                serde_json::json!({
                    "description": t.description,
                    "status": t.status,
                    "priority": t.priority,
                    "recent_attempts": t.attempts.iter().map(|a| serde_json::json!({
                        "outcome": a.outcome,
                        "approach": a.approach,
                        "reasoning": a.reasoning,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();

        let client_prompt = build_session_summary_client_prompt();
        let schema = session_summary_schema();

        Self::json_content_with_nudge(
            &serde_json::json!({
                "tasks": tasks_json,
                "recent_lessons": recent_lessons,
                "tasks_considered": task_contexts.len(),
                "client_prompt": client_prompt,
                "schema": schema,
            }),
            "Synthesize the summary yourself following `client_prompt` and `schema`. Optionally persist the distilled learnings via `remember_rule(category='lesson', content=...)`.",
        )
    }

    #[tool(
        description = "Index a project's codebase into vector storage for semantic code search. Scans files respecting .gitignore, chunks by language-aware boundaries, embeds via ONNX, stores in pgvector. Incremental: only re-indexes changed files (SHA-256 fingerprinting). Call at session start for fast code retrieval."
    )]
    pub async fn index_codebase(
        &self,
        Parameters(IndexCodebaseParams { root_path }): Parameters<IndexCodebaseParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_id = self.project_id().await?;

        let project = db::projects::get_project(self.pool(), project_id)
            .await
            .map_err(Self::db_err)?
            .ok_or_else(|| rmcp::ErrorData::internal_error("Project not found", None))?;

        let path = if let Some(ref p) = root_path {
            // Restrict to subdirectories of the project's registered root
            let canonical = std::path::Path::new(p)
                .canonicalize()
                .map_err(|e| rmcp::ErrorData::internal_error(format!("Invalid path: {e}"), None))?;
            let project_root = std::path::Path::new(&project.root_path)
                .canonicalize()
                .map_err(|e| {
                    rmcp::ErrorData::internal_error(format!("Invalid project root: {e}"), None)
                })?;
            if !canonical.starts_with(&project_root) {
                return Err(rmcp::ErrorData::internal_error(
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
        .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;

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
        Parameters(SearchCodebaseParams {
            query,
            limit,
            file_pattern,
            diversity,
        }): Parameters<SearchCodebaseParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
    pub async fn get_index_status(&self) -> Result<CallToolResult, rmcp::ErrorData> {
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
        Parameters(GetRulesForFileParams { file_path }): Parameters<GetRulesForFileParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        description = "Return up to `limit` indexed code chunks that don't yet have a 1-sentence summary. The client LLM is expected to summarize each chunk and write the results back via `submit_chunk_summaries`. The response includes `returned` (size of this batch) and `remaining_after` (chunks still needing a summary after this call) — loop until `remaining_after` is 0."
    )]
    pub async fn list_chunks_needing_summary(
        &self,
        Parameters(ListChunksNeedingSummaryParams { limit }): Parameters<
            ListChunksNeedingSummaryParams,
        >,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_id = self.project_id().await?;
        let limit = limit.unwrap_or(20).clamp(1, 100);

        let chunks = db::codebase::get_chunks_needing_summary(self.pool(), project_id, limit)
            .await
            .map_err(Self::db_err)?;
        let total_pending = db::codebase::count_chunks_needing_summary(self.pool(), project_id)
            .await
            .map_err(Self::db_err)?;
        let returned = chunks.len() as i64;
        let remaining_after = (total_pending - returned).max(0);

        let payload: Vec<serde_json::Value> = chunks
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.id.to_string(),
                    "language": c.language,
                    "file_path": c.file_path,
                    "chunk_name": c.chunk_name,
                    "start_line": c.start_line,
                    "end_line": c.end_line,
                    "content": c.content,
                })
            })
            .collect();

        Self::json_content_with_nudge(
            &serde_json::json!({
                "chunks": payload,
                "returned": returned,
                "remaining_after": remaining_after,
                "client_prompt": "For each chunk, write exactly ONE short sentence (max 15 words) describing what the code does. Then call `submit_chunk_summaries` with two aligned arrays: `ids` (the chunk UUIDs you saw) and `summaries` (your sentences).",
            }),
            "Call submit_chunk_summaries, then call list_chunks_needing_summary again until `remaining_after` is 0.",
        )
    }

    #[tool(
        description = "Persist LLM-generated 1-sentence summaries for indexed code chunks. Pass two parallel arrays: `ids` (chunk UUIDs from `list_chunks_needing_summary`) and `summaries` (one sentence each, max 512 chars). Arrays must be the same length; max 100 pairs per call."
    )]
    pub async fn submit_chunk_summaries(
        &self,
        Parameters(SubmitChunkSummariesParams { ids, summaries }): Parameters<
            SubmitChunkSummariesParams,
        >,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if ids.is_empty() {
            return Self::json_content_with_nudge(
                &serde_json::json!({ "updated": 0 }),
                "No summaries submitted.",
            );
        }
        if ids.len() != summaries.len() {
            return Err(rmcp::ErrorData::invalid_params(
                "`ids` and `summaries` must have the same length.",
                None,
            ));
        }
        if ids.len() > 100 {
            return Err(rmcp::ErrorData::invalid_params(
                "Too many summaries in one call (max 100).",
                None,
            ));
        }

        let mut updates: Vec<(Uuid, String)> = Vec::with_capacity(ids.len());
        for (raw_id, raw_summary) in ids.iter().zip(summaries.iter()) {
            Self::validate_len("summary", raw_summary, 512)?;
            let id = Self::parse_uuid(raw_id)?;
            let summary = self.maybe_scrub(raw_summary.trim().to_string());
            if !summary.is_empty() {
                updates.push((id, summary));
            }
        }

        let updated = db::codebase::update_summaries(self.pool(), &updates)
            .await
            .map_err(Self::db_err)?;

        Self::json_content_with_nudge(
            &serde_json::json!({
                "updated": updated,
                "submitted": ids.len(),
            }),
            "Run `list_chunks_needing_summary` again to keep filling coverage.",
        )
    }

    // -- Codebase edge tools --

    #[tool(
        description = "Find all callers of a function/method in the codebase edge graph. Returns entities that call (or reference) the given entity, with source file info."
    )]
    pub async fn find_callers(
        &self,
        Parameters(FindCallersParams { entity, edge_type }): Parameters<FindCallersParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        Parameters(FindCalleesParams { entity, edge_type }): Parameters<FindCalleesParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        Parameters(ShortestCodePathParams {
            from,
            to,
            max_depth,
        }): Parameters<ShortestCodePathParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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

    #[tool(
        description = "Run Louvain community detection on codebase edges to identify module clusters. Assigns community_id to code_chunks based on call/import graph structure. Run after index_codebase to detect module boundaries."
    )]
    pub async fn detect_communities(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_id = self.project_id().await?;
        let result = crate::community::detect_communities(self.pool(), project_id)
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        let communities = db::communities::get_communities(self.pool(), project_id)
            .await
            .map_err(Self::db_err)?;
        Self::json_content_with_nudge(
            &serde_json::json!({
                "num_communities": result.num_communities,
                "num_assigned": result.num_assigned,
                "modularity": result.modularity,
                "communities": communities,
            }),
            "Use get_community_members(community_id) to inspect a specific community.",
        )
    }

    #[tool(
        description = "Get all code chunks belonging to a specific community. Returns chunk names, file paths, and line ranges."
    )]
    pub async fn get_community_members(
        &self,
        Parameters(GetCommunityMembersParams { community_id }): Parameters<
            GetCommunityMembersParams,
        >,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_id = self.project_id().await?;
        let members = db::communities::get_community_members(self.pool(), project_id, community_id)
            .await
            .map_err(Self::db_err)?;
        Self::json_content_with_nudge(
            &serde_json::json!({
                "community_id": community_id,
                "members": members,
                "count": members.len(),
            }),
            "Use detect_cross_community_changes to find impact across modules.",
        )
    }

    #[tool(
        description = "Detect which communities are affected by changes to given files. Returns affected community IDs with member counts. Changes spanning multiple communities may need cross-module review."
    )]
    pub async fn detect_cross_community_changes(
        &self,
        Parameters(DetectCrossCommunityChangesParams { file_paths }): Parameters<
            DetectCrossCommunityChangesParams,
        >,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_id = self.project_id().await?;
        let affected =
            db::communities::get_affected_communities(self.pool(), project_id, &file_paths)
                .await
                .map_err(Self::db_err)?;
        let cross_module = affected.len() > 1;
        Self::json_content_with_nudge(
            &serde_json::json!({
                "file_paths": file_paths,
                "affected_communities": affected,
                "community_count": affected.len(),
                "cross_module_impact": cross_module,
            }),
            if cross_module {
                "Changes span multiple communities — consider cross-module review."
            } else {
                "Changes are contained within a single community."
            },
        )
    }

    #[tool(
        description = "Get full context for a file: code structure, linked rules, call dependencies, and community info. Use before reading or modifying a file to understand its role in the codebase."
    )]
    pub async fn get_file_context(
        &self,
        Parameters(GetFileContextParams { file_path }): Parameters<GetFileContextParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Self::validate_len("file_path", &file_path, 1024)?;
        let project_id = self.project_id().await?;

        // Code structure
        let chunks = db::codebase::get_file_chunks_metadata(self.pool(), project_id, &file_path)
            .await
            .map_err(Self::db_err)?;

        let code_structure: Vec<serde_json::Value> = chunks
            .iter()
            .map(|c| {
                serde_json::json!({
                    "chunk_name": c.chunk_name,
                    "chunk_kind": c.chunk_kind,
                    "lines": format!("{}-{}", c.start_line, c.end_line),
                    "community_id": c.community_id,
                })
            })
            .collect();

        // Linked rules
        let file_rules =
            db::rule_chunk_links::get_rules_for_file(self.pool(), &file_path, project_id)
                .await
                .map_err(Self::db_err)?;

        let linked_rules: Vec<serde_json::Value> = file_rules
            .iter()
            .map(|r| {
                let preview: String = r.content.chars().take(80).collect();
                serde_json::json!({
                    "id": r.rule_id.to_string(),
                    "category": r.category,
                    "preview": preview,
                })
            })
            .collect();

        // Dependencies: callers/callees for first 5 named chunks
        let mut all_callers: Vec<serde_json::Value> = Vec::new();
        let mut all_callees: Vec<serde_json::Value> = Vec::new();
        let mut seen_callers: std::collections::HashSet<(String, Option<String>)> =
            std::collections::HashSet::new();
        let mut seen_callees: std::collections::HashSet<(String, Option<String>)> =
            std::collections::HashSet::new();

        let named_chunks: Vec<_> = chunks
            .iter()
            .filter_map(|c| c.chunk_name.as_ref())
            .take(5)
            .collect();

        for entity in &named_chunks {
            let callers = db::codebase_edges::get_callers(self.pool(), project_id, entity)
                .await
                .map_err(Self::db_err)?;
            for e in callers {
                let key = (e.source_entity.clone(), e.source_file.clone());
                if seen_callers.insert(key) {
                    all_callers.push(serde_json::json!({
                        "entity": e.source_entity,
                        "file": e.source_file,
                    }));
                }
            }
            let callees = db::codebase_edges::get_callees(self.pool(), project_id, entity)
                .await
                .map_err(Self::db_err)?;
            for e in callees {
                let key = (e.target_entity.clone(), e.target_file.clone());
                if seen_callees.insert(key) {
                    all_callees.push(serde_json::json!({
                        "entity": e.target_entity,
                        "file": e.target_file,
                    }));
                }
            }
        }

        // Communities
        let file_paths = vec![file_path.clone()];
        let communities =
            db::communities::get_affected_communities(self.pool(), project_id, &file_paths)
                .await
                .map_err(Self::db_err)?;

        let communities_json: Vec<serde_json::Value> = communities
            .iter()
            .map(|c| {
                serde_json::json!({
                    "community_id": c.community_id,
                    "member_count": c.member_count,
                })
            })
            .collect();

        Self::json_content_with_nudge(
            &serde_json::json!({
                "file_path": file_path,
                "code_structure": code_structure,
                "linked_rules": linked_rules,
                "dependencies": {
                    "callers": all_callers,
                    "callees": all_callees,
                },
                "communities": communities_json,
            }),
            "Apply these rules and context when reading or modifying this file.",
        )
    }
}

#[derive(Debug)]
struct AttemptSnippet {
    outcome: String,
    approach: String,
    reasoning: String,
}

#[derive(Debug)]
struct TaskContext {
    description: String,
    status: String,
    priority: Option<String>,
    attempts: Vec<AttemptSnippet>,
}

fn build_session_summary_client_prompt() -> &'static str {
    "You are synthesizing a session briefing from structured data the Lore server returned. \
     Read `tasks` (active/blocked with recent attempts) and `recent_lessons`. \
     Produce a single JSON object that matches `schema`. No prose, no markdown fences. \
     Each array entry is a short factual bullet (max 25 words). \
     - investigated: topics, files, or questions explored. \
     - learned: concrete technical insights (bias toward reusable lessons). \
     - completed: tasks/subtasks that reached an accepted outcome. \
     - blocked: tasks stuck on rejections or missing info. \
     - next_steps: what to do next, ordered by priority. \
     If you want to persist distilled lessons, call `remember_rule(category='lesson', content=...)` afterward."
}

fn session_summary_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "investigated": {"type": "array", "items": {"type": "string"}},
            "learned":      {"type": "array", "items": {"type": "string"}},
            "completed":    {"type": "array", "items": {"type": "string"}},
            "blocked":      {"type": "array", "items": {"type": "string"}},
            "next_steps":   {"type": "array", "items": {"type": "string"}},
        },
        "required": ["investigated", "learned", "completed", "blocked", "next_steps"],
    })
}

impl ServerHandler for LoreServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(Self::protocol_text().into()),
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
            ..Default::default()
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, rmcp::ErrorData> {
        use rmcp::model::AnnotateAble;

        Ok(ListResourcesResult {
            resources: vec![
                RawResource {
                    uri: "lore://protocol".into(),
                    name: "Lore Protocol".into(),
                    title: None,
                    description: Some("Mandatory episodic memory protocol rules".into()),
                    mime_type: Some("text/plain".into()),
                    size: None,
                    icons: None,
                }
                .no_annotation(),
                RawResource {
                    uri: "lore://active-context".into(),
                    name: "Active Context".into(),
                    title: None,
                    description: Some(
                        "Current project, active tasks, and context wipe count".into(),
                    ),
                    mime_type: Some("application/json".into()),
                    size: None,
                    icons: None,
                }
                .no_annotation(),
                RawResource {
                    uri: "lore://tasks/active".into(),
                    name: "Active Tasks".into(),
                    title: None,
                    description: Some(
                        "Currently active tasks for the current project with attempt counts".into(),
                    ),
                    mime_type: Some("application/json".into()),
                    size: None,
                    icons: None,
                }
                .no_annotation(),
                RawResource {
                    uri: "lore://lessons/recent".into(),
                    name: "Recent Lessons".into(),
                    title: None,
                    description: Some(
                        "Ten most recent Lesson-category rules for the current project".into(),
                    ),
                    mime_type: Some("application/json".into()),
                    size: None,
                    icons: None,
                }
                .no_annotation(),
                RawResource {
                    uri: "lore://rules".into(),
                    name: "Semantic Rules".into(),
                    title: None,
                    description: Some(
                        "All active semantic rules for the current project, grouped by category"
                            .into(),
                    ),
                    mime_type: Some("application/json".into()),
                    size: None,
                    icons: None,
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
    ) -> Result<ReadResourceResult, rmcp::ErrorData> {
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
            "lore://tasks/active" => {
                let project_id = *self.inner.current_project_id.read().await;
                let text = if let Some(pid) = project_id {
                    let tasks =
                        db::tasks::list_tasks(self.pool(), pid, Some(db::TaskStatus::Active))
                            .await
                            .unwrap_or_default();
                    let mut out = Vec::with_capacity(tasks.len());
                    for t in &tasks {
                        let attempts = db::attempts::list_attempts(self.pool(), t.id, None)
                            .await
                            .unwrap_or_default();
                        let pending = attempts
                            .iter()
                            .filter(|a| matches!(a.outcome, db::attempts::AttemptOutcome::Pending))
                            .count();
                        let rejected = attempts
                            .iter()
                            .filter(|a| matches!(a.outcome, db::attempts::AttemptOutcome::Rejected))
                            .count();
                        out.push(serde_json::json!({
                            "id": t.id,
                            "description": t.description,
                            "priority": t.priority,
                            "task_type": t.task_type,
                            "created_at": t.created_at,
                            "total_attempts": attempts.len(),
                            "pending_attempts": pending,
                            "rejected_attempts": rejected,
                        }));
                    }
                    serde_json::to_string_pretty(&serde_json::json!({ "tasks": out }))
                        .unwrap_or_default()
                } else {
                    r#"{"tasks": [], "hint": "Call switch_project first"}"#.to_string()
                };
                Ok(ReadResourceResult {
                    contents: vec![ResourceContents::text(text, "lore://tasks/active")],
                })
            }
            "lore://lessons/recent" => {
                let project_id = *self.inner.current_project_id.read().await;
                let text = if let Some(pid) = project_id {
                    let rules = db::semantic::list_rules(
                        self.pool(),
                        pid,
                        Some(db::semantic::RuleCategory::Lesson),
                        None,
                    )
                    .await
                    .unwrap_or_default();
                    let recent: Vec<_> = rules.into_iter().rev().take(10).collect();
                    serde_json::to_string_pretty(&serde_json::json!({ "lessons": recent }))
                        .unwrap_or_default()
                } else {
                    r#"{"lessons": [], "hint": "Call switch_project first"}"#.to_string()
                };
                Ok(ReadResourceResult {
                    contents: vec![ResourceContents::text(text, "lore://lessons/recent")],
                })
            }
            "lore://rules" => {
                let project_id = *self.inner.current_project_id.read().await;
                let text = if let Some(pid) = project_id {
                    let rules = db::semantic::list_rules(self.pool(), pid, None, None)
                        .await
                        .unwrap_or_default();
                    let mut by_cat: std::collections::BTreeMap<String, Vec<_>> =
                        std::collections::BTreeMap::new();
                    for r in rules {
                        let key = format!("{:?}", r.category).to_lowercase();
                        by_cat.entry(key).or_default().push(r);
                    }
                    serde_json::to_string_pretty(
                        &serde_json::json!({ "rules_by_category": by_cat }),
                    )
                    .unwrap_or_default()
                } else {
                    r#"{"rules_by_category": {}, "hint": "Call switch_project first"}"#.to_string()
                };
                Ok(ReadResourceResult {
                    contents: vec![ResourceContents::text(text, "lore://rules")],
                })
            }
            _ => Err(ErrorData::resource_not_found(
                "Unknown resource URI",
                Some(serde_json::Value::String(request.uri)),
            )),
        }
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, rmcp::ErrorData> {
        Ok(ListPromptsResult {
            prompts: Self::prompt_catalog()
                .into_iter()
                .map(|p| p.to_prompt())
                .collect(),
            next_cursor: None,
        })
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, rmcp::ErrorData> {
        let def = Self::prompt_catalog()
            .into_iter()
            .find(|p| p.name == request.name)
            .ok_or_else(|| {
                ErrorData::invalid_params(
                    "Unknown prompt name",
                    Some(serde_json::json!({ "name": request.name })),
                )
            })?;

        let args = request.arguments.unwrap_or_default();
        for a in def.arguments {
            if a.required && !args.contains_key(a.name) {
                return Err(ErrorData::invalid_params(
                    "Missing required argument",
                    Some(serde_json::json!({
                        "prompt": def.name,
                        "argument": a.name,
                    })),
                ));
            }
        }

        let text = def.render(&args);
        Ok(GetPromptResult {
            description: Some(def.description.to_string()),
            messages: vec![PromptMessage::new_text(PromptMessageRole::User, text)],
        })
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        let disabled = &self.config().disabled_tools;
        let all = self.inner.tool_router.list_all();
        let tools = if disabled.is_empty() {
            all
        } else {
            all.into_iter()
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
    ) -> Result<CallToolResult, rmcp::ErrorData> {
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
        let result = self.inner.tool_router.call(ctx).await?;

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
        let tools = LoreServer::tool_router().list_all();
        assert!(
            tools.len() >= 30,
            "Expected at least 30 tools, got {}",
            tools.len()
        );
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(names.contains(&"remember_rule"));
        assert!(names.contains(&"forget_rule"));
        assert!(names.contains(&"get_rule"));
        assert!(names.contains(&"generate_handoff"));
        assert!(names.contains(&"link_tasks"));
        assert!(names.contains(&"add_edge"));
        assert!(names.contains(&"query_neighbors"));
        assert!(names.contains(&"find_path"));
        assert!(names.contains(&"get_rules_for_file"));
        assert!(names.contains(&"find_callers"));
        assert!(names.contains(&"find_callees"));
        assert!(names.contains(&"shortest_code_path"));
        assert!(names.contains(&"detect_communities"));
        assert!(names.contains(&"get_community_members"));
        assert!(names.contains(&"detect_cross_community_changes"));
        assert!(names.contains(&"get_file_context"));
        assert!(names.contains(&"generate_session_summary"));
        assert!(names.contains(&"list_chunks_needing_summary"));
        assert!(names.contains(&"submit_chunk_summaries"));
        assert!(!names.contains(&"generate_summaries"));
    }

    #[test]
    fn test_session_summary_client_prompt_mentions_schema() {
        let p = build_session_summary_client_prompt();
        assert!(p.contains("schema"));
        assert!(p.contains("investigated"));
        assert!(p.contains("next_steps"));
        assert!(p.contains("remember_rule"));
    }

    #[test]
    fn test_session_summary_schema_has_required_fields() {
        let s = session_summary_schema();
        let required = s["required"].as_array().unwrap();
        let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        for f in [
            "investigated",
            "learned",
            "completed",
            "blocked",
            "next_steps",
        ] {
            assert!(names.contains(&f), "schema missing required field {f}");
        }
    }

    #[test]
    fn test_disabled_tools_filter() {
        let all = LoreServer::tool_router().list_all();
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

    #[test]
    fn test_prompt_catalog_has_all_templates() {
        let names: Vec<&str> = LoreServer::prompt_catalog()
            .iter()
            .map(|p| p.name)
            .collect();
        for expected in [
            "plan_task",
            "resume_work",
            "review_ledger",
            "diagnose_failure",
            "record_lesson",
        ] {
            assert!(
                names.contains(&expected),
                "prompt catalog missing {expected}"
            );
        }
    }

    #[test]
    fn test_prompt_def_to_prompt_encodes_required_args() {
        let plan = LoreServer::prompt_catalog()
            .into_iter()
            .find(|p| p.name == "plan_task")
            .expect("plan_task present");
        let prompt = plan.to_prompt();
        assert_eq!(prompt.name, "plan_task");
        let args = prompt.arguments.expect("arguments set");
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].name, "goal");
        assert_eq!(args[0].required, Some(true));
    }

    #[test]
    fn test_prompt_render_substitutes_args() {
        let plan = LoreServer::prompt_catalog()
            .into_iter()
            .find(|p| p.name == "plan_task")
            .unwrap();
        let mut args = serde_json::Map::new();
        args.insert("goal".into(), serde_json::json!("Add OAuth login"));
        let out = plan.render(&args);
        assert!(out.contains("Add OAuth login"));
        assert!(out.contains("start_task"));
    }

    #[test]
    fn test_prompt_resume_work_has_no_required_args() {
        let resume = LoreServer::prompt_catalog()
            .into_iter()
            .find(|p| p.name == "resume_work")
            .unwrap();
        let prompt = resume.to_prompt();
        assert!(prompt.arguments.is_none());
    }

    #[test]
    fn test_diagnose_failure_renders_both_args() {
        let diag = LoreServer::prompt_catalog()
            .into_iter()
            .find(|p| p.name == "diagnose_failure")
            .unwrap();
        let mut args = serde_json::Map::new();
        args.insert("task_id".into(), serde_json::json!("abc-123"));
        args.insert("error".into(), serde_json::json!("connection refused"));
        let out = diag.render(&args);
        assert!(out.contains("abc-123"));
        assert!(out.contains("connection refused"));
        assert!(out.contains("log_outcome"));
    }
}
