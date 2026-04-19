//! Evaluation harness scaffold (cfg(feature = "eval")).
//!
//! P1-T1 lands types + a synthetic fixture loader. The deterministic
//! replay runner (P1-T2), metrics (P1-T3), and real dataset ingestion
//! (LoCoMo / LongMemEval / BEAM) come in follow-up PRs.

pub mod fixtures;
pub mod harness;
pub mod loader;
pub mod metrics;
pub mod types;

pub use fixtures::load_mini;
pub use harness::{replay_case, CaseRun, RecallRun, ReplayError};
pub use loader::{DatasetLoader, DatasetSource, EvalError};
pub use metrics::{
    aggregate, score_case, score_recall, CaseMetrics, DatasetMetrics, RecallMetrics,
};
pub use types::{EvalCase, EvalEvent, ExpectedOutcome, OutcomeKind};
