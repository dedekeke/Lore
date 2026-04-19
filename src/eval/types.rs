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
    LogOutcome {
        attempt_ref: String,
        outcome: OutcomeKind,
        reasoning: String,
    },
    RememberRule {
        content: String,
        category: String,
    },
    RecallRules {
        query: String,
        expected_hits: Vec<String>,
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
