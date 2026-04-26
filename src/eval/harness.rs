//! Deterministic replay runner for eval cases.
//!
//! Given a live `LoreServer` and an `EvalCase`, `replay_case` walks the
//! case's event tape, translates each `EvalEvent` into the corresponding
//! MCP tool call, and builds a per-case `label -> real UUID` map so the
//! `RecallRules` event's `expected_hits` labels can be graded against the
//! UUIDs actually returned by `recall_rules`.
//!
//! Scoring (precision@k / recall@k / MRR) lives in P1-T3; this file only
//! records the raw inputs/outputs needed for grading.

use std::collections::HashMap;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, RawContent};
use thiserror::Error;

use crate::server::{
    LogOutcomeParams, LoreServer, ProposeAttemptParams, RecallRulesParams, RememberRuleParams,
    StartTaskParams, SwitchProjectParams,
};

use super::types::{EvalCase, EvalEvent, OutcomeKind};

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error("mcp: {0}")]
    Mcp(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("missing field {field} in tool result")]
    MissingField { field: &'static str },
    #[error("unknown task_ref: {0}")]
    UnknownTaskRef(String),
    #[error("unknown attempt_ref: {0}")]
    UnknownAttemptRef(String),
}

impl From<rmcp::ErrorData> for ReplayError {
    fn from(e: rmcp::ErrorData) -> Self {
        ReplayError::Mcp(e.to_string())
    }
}

/// Raw inputs + outputs for one `RecallRules` event. Scoring is deferred.
#[derive(Debug, Clone)]
pub struct RecallRun {
    pub query: String,
    pub expected_labels: Vec<String>,
    pub returned_ids: Vec<String>,
}

/// Raw inputs + outputs for one `GetActiveContext` event — captures the
/// procedural-block UUIDs surfaced and the case-local labels we expected
/// to see. Scoring is deferred.
#[derive(Debug, Clone)]
pub struct ContextRun {
    pub expected_labels: Vec<String>,
    pub returned_procedural_ids: Vec<String>,
}

/// Replay output for one case.
#[derive(Debug, Clone)]
pub struct CaseRun {
    pub case_id: String,
    /// Case-local label (e.g. "task-t3", "attempt-t1-a1", "rule-go-tabs")
    /// -> real UUID assigned by the server at insertion time.
    pub labels: HashMap<String, String>,
    pub recalls: Vec<RecallRun>,
    pub contexts: Vec<ContextRun>,
    /// RememberRule labels whose rule was short-circuited by the server's
    /// near-duplicate guard (cosine >= 0.95). The rule was not stored, so
    /// the label cannot be resolved to a UUID. Surfaced here so P1-T3 can
    /// score them as misses without the test asserting a panic.
    pub deduplicated_labels: Vec<String>,
}

/// Replay a single case against `server`. The case gets its own project
/// (`eval-<case.id>`) so rules don't leak between cases.
pub async fn replay_case(server: &LoreServer, case: &EvalCase) -> Result<CaseRun, ReplayError> {
    let project_name = format!("eval-{}", case.id);
    // `projects.root_path` is UNIQUE, so per-case root paths are required
    // to keep cases isolated; the path is synthetic and never touched on disk.
    let root_path = format!("/tmp/lore-eval/{}", case.id);
    server
        .switch_project(Parameters(SwitchProjectParams {
            name: Some(project_name),
            root_path: Some(root_path),
        }))
        .await?;

    let mut labels: HashMap<String, String> = HashMap::new();
    let mut task_uuids: HashMap<String, String> = HashMap::new();
    let mut attempt_uuids: HashMap<String, String> = HashMap::new();
    let mut attempt_counter: HashMap<String, usize> = HashMap::new();
    let mut recalls: Vec<RecallRun> = Vec::new();
    let mut contexts: Vec<ContextRun> = Vec::new();
    let mut deduplicated_labels: Vec<String> = Vec::new();

    for event in &case.events {
        match event {
            EvalEvent::StartTask {
                task_ref,
                description,
            } => {
                let res = server
                    .start_task(Parameters(StartTaskParams {
                        description: description.clone(),
                        parent_task_id: None,
                        priority: None,
                        task_type: None,
                        ticket_number: None,
                    }))
                    .await?;
                let task_id = json_field(&res, "task_id")?;
                task_uuids.insert(task_ref.clone(), task_id.clone());
                labels.insert(format!("task-{task_ref}"), task_id);
            }
            EvalEvent::ProposeAttempt { task_ref, approach } => {
                let task_id = task_uuids
                    .get(task_ref)
                    .ok_or_else(|| ReplayError::UnknownTaskRef(task_ref.clone()))?
                    .clone();
                let res = server
                    .propose_attempt_impl(ProposeAttemptParams {
                        task_id,
                        approach_summary: approach.clone(),
                        agent_id: None,
                        session_id: None,
                        request_confirmation: None,
                    })
                    .await?;
                let attempt_id = json_field(&res, "attempt_id")?;
                let n = attempt_counter.entry(task_ref.clone()).or_insert(0);
                *n += 1;
                let attempt_ref = format!("{task_ref}-a{n}");
                attempt_uuids.insert(attempt_ref.clone(), attempt_id.clone());
                labels.insert(format!("attempt-{attempt_ref}"), attempt_id);
            }
            EvalEvent::LogOutcome {
                attempt_ref,
                outcome,
                reasoning,
            } => {
                let attempt_id = attempt_uuids
                    .get(attempt_ref)
                    .ok_or_else(|| ReplayError::UnknownAttemptRef(attempt_ref.clone()))?
                    .clone();
                let outcome_str = match outcome {
                    OutcomeKind::Accepted => "accepted",
                    OutcomeKind::Rejected => "rejected",
                    OutcomeKind::Pending => "pending",
                };
                server
                    .log_outcome(Parameters(LogOutcomeParams {
                        attempt_id,
                        outcome: outcome_str.to_string(),
                        reasoning: reasoning.clone(),
                        git_ref: None,
                        code_snippet: None,
                        agent_id: None,
                        session_id: None,
                    }))
                    .await?;
            }
            EvalEvent::RememberRule {
                content,
                category,
                label,
                always_inject,
            } => {
                let res = server
                    .remember_rule(Parameters(RememberRuleParams {
                        category: category.clone(),
                        content: content.clone(),
                        tags: None,
                        always_inject: *always_inject,
                    }))
                    .await?;
                // `rule_id` absent when server short-circuits on duplicate_warning
                // (cosine >= 0.95). Surface via `deduplicated_labels` so P1-T3
                // can score as miss without the harness asserting a panic.
                if let Ok(rule_id) = json_field(&res, "rule_id") {
                    labels.insert(label.clone(), rule_id);
                } else {
                    tracing::warn!(
                        case_id = %case.id,
                        label = %label,
                        "remember_rule short-circuited on dedup guard; label unresolved"
                    );
                    deduplicated_labels.push(label.clone());
                }
            }
            EvalEvent::RecallRules {
                query,
                expected_hits,
            } => {
                let res = server
                    .recall_rules(Parameters(RecallRulesParams {
                        query: query.clone(),
                        limit: Some(10),
                        category: None,
                        tags: None,
                        cross_project: None,
                        compact: Some(true),
                        grouped: None,
                    }))
                    .await?;
                let returned_ids = parse_compact_ids(&res)?;
                recalls.push(RecallRun {
                    query: query.clone(),
                    expected_labels: expected_hits.clone(),
                    returned_ids,
                });
            }
            EvalEvent::GetActiveContext {
                expected_procedural_labels,
            } => {
                let res = server.get_active_context().await?;
                let returned_procedural_ids = parse_procedural_ids(&res)?;
                contexts.push(ContextRun {
                    expected_labels: expected_procedural_labels.clone(),
                    returned_procedural_ids,
                });
            }
        }
    }

    Ok(CaseRun {
        case_id: case.id.clone(),
        labels,
        recalls,
        contexts,
        deduplicated_labels,
    })
}

fn tool_text(res: &CallToolResult) -> Result<&str, ReplayError> {
    res.content
        .iter()
        .find_map(|c| match &c.raw {
            RawContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .ok_or(ReplayError::MissingField {
            field: "content[text]",
        })
}

fn json_field(res: &CallToolResult, field: &'static str) -> Result<String, ReplayError> {
    let text = tool_text(res)?;
    let v: serde_json::Value = serde_json::from_str(text)?;
    v.get(field)
        .and_then(|f| f.as_str())
        .map(String::from)
        .ok_or(ReplayError::MissingField { field })
}

/// Pulls `procedural.rules[].id` from a `get_active_context` payload.
/// When the feature flag is off or the fetch failed, the key is absent —
/// treat as empty. Matches the contract in `build_procedural_block`.
///
/// Note: returns `Ok(vec![])` for BOTH "block absent" (flag off, fetch failed)
/// AND "block present but empty" (no always-inject rules). Scoring cannot
/// distinguish them; the `tracing::debug!` on pointer miss surfaces the
/// absent case for debugging.
fn parse_procedural_ids(res: &CallToolResult) -> Result<Vec<String>, ReplayError> {
    let text = tool_text(res)?;
    let v: serde_json::Value = serde_json::from_str(text)?;
    let Some(rules) = v.pointer("/procedural/rules").and_then(|x| x.as_array()) else {
        tracing::debug!("get_active_context response lacks /procedural/rules — treating as empty");
        return Ok(Vec::new());
    };
    Ok(rules
        .iter()
        .filter_map(|r| r.get("id").and_then(|i| i.as_str()).map(String::from))
        .collect())
}

/// `recall_rules(compact=true)` returns a JSON array of `{id, score, ...}`.
/// Format defined by the compact branch in `src/server.rs` (recall_rules
/// handler ~L437-456). If that schema changes, update this parser in lockstep.
fn parse_compact_ids(res: &CallToolResult) -> Result<Vec<String>, ReplayError> {
    let text = tool_text(res)?;
    let v: serde_json::Value = serde_json::from_str(text)?;
    let arr = v.as_array().ok_or(ReplayError::MissingField {
        field: "compact_array",
    })?;
    Ok(arr
        .iter()
        .filter_map(|h| h.get("id").and_then(|i| i.as_str()).map(String::from))
        .collect())
}
