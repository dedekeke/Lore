use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvalCase {
    pub id: String,
    pub dataset: String,
    pub events: Vec<EvalEvent>,
    pub expected: ExpectedOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvalEvent {
    StartTask {
        task_ref: String,
        description: String,
    },
    ProposeAttempt {
        task_ref: String,
        approach: String,
    },
    /// `attempt_ref` MUST equal `"{task_ref}-a{N}"` where N is the 1-based
    /// index of the referenced `ProposeAttempt` within the case (first
    /// attempt for `t1` is `t1-a1`, second is `t1-a2`). The harness errors
    /// with `ReplayError::UnknownAttemptRef` if the ref does not resolve.
    LogOutcome {
        attempt_ref: String,
        outcome: OutcomeKind,
        reasoning: String,
    },
    RememberRule {
        content: String,
        category: String,
        /// Case-local label (e.g. "rule-go-tabs"). Referenced by a later
        /// `RecallRules.expected_hits` entry. Required so the harness can
        /// resolve expected labels to real rule UUIDs after insertion.
        label: String,
        /// Optional override for the `always_inject` flag. Tri-state:
        /// - `None`: preserve server-side category inference
        ///   (true iff category is `instruction`).
        /// - `Some(true)`: force always-inject on.
        /// - `Some(false)`: force always-inject off, even for instructions.
        #[serde(default)]
        always_inject: Option<bool>,
    },
    RecallRules {
        query: String,
        expected_hits: Vec<String>,
    },
    /// Calls `get_active_context` and asserts that the `procedural.rules[].id`
    /// array contains the rules resolved from `expected_procedural_labels`.
    /// Used to verify always-inject surfacing for Phase 3 fixtures.
    GetActiveContext {
        expected_procedural_labels: Vec<String>,
    },
}

/// Mirrors the `outcome` values accepted by `log_outcome` in `src/server.rs`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    Accepted,
    Rejected,
    Pending,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExpectedOutcome {
    pub recall_hit_ids: Vec<String>,
    pub task_completion: Option<bool>,
}
