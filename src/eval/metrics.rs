//! Retrieval metrics for eval replay runs.
//!
//! Pure, deterministic scoring over the `CaseRun` output from
//! `super::harness::replay_case`. No I/O, no async. The harness produces
//! raw inputs/outputs; this module grades them.
//!
//! Definitions:
//!
//! - **precision@k** = |retrieved_top_k ∩ relevant| / min(k, |retrieved|)
//!   (TREC convention — a list shorter than `k` is not penalised by empty
//!   slots; a zero-length return yields precision 0).
//! - **recall@k** = |retrieved_top_k ∩ relevant| / |relevant|
//! - **MRR** = mean of 1/rank of the first relevant hit per query (0 when
//!   no relevant result appears in top-k).
//!
//! Labels referenced by `expected_hits` that did NOT resolve to a UUID at
//! replay time — either because the server's near-duplicate guard
//! short-circuited insertion (see `CaseRun::deduplicated_labels`) or because
//! the label points at a task/attempt that `recall_rules` cannot return —
//! stay in recall's denominator but can never enter the numerator. They
//! score as misses, as documented in `eval/README.md`.
//!
//! Negative cases (empty `expected_hits`) have undefined precision/recall/MRR;
//! all three fields are `None`. The query still reports a `negative_pass`
//! boolean (`true` iff the server returned nothing).

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::harness::{CaseRun, ContextRun, RecallRun};

/// Metrics for a single `RecallRules` event. All three ranking metrics are
/// `Option` because a negative case (empty `expected_hits`) has no defined
/// precision/recall/MRR — consult `negative_pass` instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[must_use]
pub struct RecallMetrics {
    pub k: usize,
    pub precision_at_k: Option<f64>,
    pub recall_at_k: Option<f64>,
    pub mrr: Option<f64>,
    /// `Some(true)` when the case expected no hits and got none;
    /// `Some(false)` when it expected none but got some; `None` for
    /// positive cases.
    pub negative_pass: Option<bool>,
}

/// Metrics for a single `GetActiveContext` event. Precision/recall are
/// `Option` for the same reason as `RecallMetrics`: a negative case (empty
/// `expected_procedural_labels`) has no ranking metric, only a pass bit.
/// Rank-agnostic (the procedural block is a set, not a ranked list) so no
/// MRR is reported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[must_use]
pub struct ContextMetrics {
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    /// `Some(true)` when the case expected no rules and got none;
    /// `Some(false)` when it expected none but got some; `None` for
    /// positive cases.
    pub negative_pass: Option<bool>,
}

/// Metrics rolled up per case. Means are taken only over recalls/contexts
/// whose metric is `Some(_)` — negative cases don't count toward the
/// precision/recall/MRR means.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[must_use]
pub struct CaseMetrics {
    pub case_id: String,
    pub per_recall: Vec<RecallMetrics>,
    pub mean_precision_at_k: Option<f64>,
    pub mean_recall_at_k: Option<f64>,
    pub mean_mrr: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_context: Vec<ContextMetrics>,
    #[serde(default)]
    pub mean_context_precision: Option<f64>,
    #[serde(default)]
    pub mean_context_recall: Option<f64>,
}

/// Dataset-level aggregate. Means are taken over individual `RecallRun`s
/// (not cases), so a case with more queries contributes proportionally.
/// Negative-case pass rate is tracked separately from the ranking metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[must_use]
pub struct DatasetMetrics {
    pub k: usize,
    pub num_cases: usize,
    pub num_recalls: usize,
    /// Number of recalls whose `precision_at_k` / `recall_at_k` / `mrr` are
    /// defined (positive cases). The means below are taken over this count.
    pub num_scored: usize,
    pub num_negative: usize,
    /// Fraction of negative cases that passed (server returned nothing).
    /// 0.0 when `num_negative == 0`.
    pub negative_pass_rate: f64,
    /// Mean over positive recalls. 0.0 when `num_scored == 0`.
    pub mean_precision_at_k: f64,
    pub mean_recall_at_k: f64,
    pub mean_mrr: f64,
    /// Total number of `GetActiveContext` events across all cases.
    #[serde(default)]
    pub num_contexts: usize,
    /// Positive context events (expected at least one procedural rule).
    #[serde(default)]
    pub num_contexts_scored: usize,
    /// Negative context events (expected no procedural rules).
    #[serde(default)]
    pub num_contexts_negative: usize,
    /// Mean precision over positive context events. 0.0 when
    /// `num_contexts_scored == 0`.
    #[serde(default)]
    pub mean_context_precision: f64,
    /// Mean recall over positive context events. 0.0 when
    /// `num_contexts_scored == 0`.
    #[serde(default)]
    pub mean_context_recall: f64,
    /// Fraction of negative context events that passed (server returned no
    /// procedural rules). 0.0 when `num_contexts_negative == 0`.
    #[serde(default)]
    pub context_negative_pass_rate: f64,
    pub per_case: Vec<CaseMetrics>,
}

/// Score a single recall query.
///
/// `k = 0` is clamped to `1`: meaningful for well-formed callers via
/// `debug_assert!`, silent in release so a bad CLI flag doesn't panic the
/// process.
pub fn score_recall(run: &RecallRun, labels: &HashMap<String, String>, k: usize) -> RecallMetrics {
    debug_assert!(k > 0, "k must be >= 1");
    let k = k.max(1);

    // Note: `labels` only contains entries for rules that were actually
    // stored — `CaseRun::deduplicated_labels` labels and task-/attempt-*
    // references will be absent by design. They stay in `expected_labels`
    // so they count against recall's denominator as misses.
    let expected_uuids: HashSet<&str> = run
        .expected_labels
        .iter()
        .filter_map(|label| labels.get(label).map(String::as_str))
        .collect();

    let top_k: Vec<&str> = run
        .returned_ids
        .iter()
        .take(k)
        .map(String::as_str)
        .collect();
    let hits: usize = top_k
        .iter()
        .filter(|id| expected_uuids.contains(*id))
        .count();

    // Negative case: precision/recall/MRR undefined. Pass-rate signal lives
    // in `negative_pass`. Aggregation tracks it in its own bucket so the
    // ranking means stay uncontaminated.
    if run.expected_labels.is_empty() {
        return RecallMetrics {
            k,
            precision_at_k: None,
            recall_at_k: None,
            mrr: None,
            negative_pass: Some(run.returned_ids.is_empty()),
        };
    }

    // TREC: precision@k divides by retrieved length (capped at k), not k.
    // An empty return yields 0 precision rather than a div-by-zero.
    let precision = if top_k.is_empty() {
        0.0
    } else {
        hits as f64 / top_k.len() as f64
    };
    // Denominator is the raw `expected_labels` count, NOT `expected_uuids.len()`:
    // unresolved labels (deduplicated or never stored) stay as misses — see
    // module-level docs.
    let recall = hits as f64 / run.expected_labels.len() as f64;

    let rr = top_k
        .iter()
        .position(|id| expected_uuids.contains(*id))
        .map_or(0.0, |i| 1.0 / (i + 1) as f64);

    RecallMetrics {
        k,
        precision_at_k: Some(precision),
        recall_at_k: Some(recall),
        mrr: Some(rr),
        negative_pass: None,
    }
}

/// Score a single `GetActiveContext` event. The procedural block is a set
/// (not a ranked list) so precision/recall are computed over the full
/// returned ID list without a `k` cutoff.
pub fn score_context(run: &ContextRun, labels: &HashMap<String, String>) -> ContextMetrics {
    // Same resolution rule as `score_recall`: labels that never bound to a
    // UUID (deduplicated or never stored) stay in the denominator as misses.
    let expected_uuids: HashSet<&str> = run
        .expected_labels
        .iter()
        .filter_map(|label| labels.get(label).map(String::as_str))
        .collect();
    let returned: HashSet<&str> = run
        .returned_procedural_ids
        .iter()
        .map(String::as_str)
        .collect();

    if run.expected_labels.is_empty() {
        return ContextMetrics {
            precision: None,
            recall: None,
            negative_pass: Some(run.returned_procedural_ids.is_empty()),
        };
    }

    let hits = expected_uuids
        .iter()
        .filter(|id| returned.contains(*id))
        .count();
    let precision = if returned.is_empty() {
        0.0
    } else {
        hits as f64 / returned.len() as f64
    };
    // Same denominator policy as `score_recall`: unresolved labels count as misses.
    let recall = hits as f64 / run.expected_labels.len() as f64;

    ContextMetrics {
        precision: Some(precision),
        recall: Some(recall),
        negative_pass: None,
    }
}

/// Score every recall + context in a case and compute per-case means.
/// Means skip negative (empty-expected) events.
pub fn score_case(run: &CaseRun, k: usize) -> CaseMetrics {
    let per_recall: Vec<RecallMetrics> = run
        .recalls
        .iter()
        .map(|r| score_recall(r, &run.labels, k))
        .collect();

    let mean_precision = mean_opt(per_recall.iter().filter_map(|m| m.precision_at_k));
    let mean_recall = mean_opt(per_recall.iter().filter_map(|m| m.recall_at_k));
    let mean_mrr = mean_opt(per_recall.iter().filter_map(|m| m.mrr));

    let per_context: Vec<ContextMetrics> = run
        .contexts
        .iter()
        .map(|c| score_context(c, &run.labels))
        .collect();
    let mean_context_precision = mean_opt(per_context.iter().filter_map(|m| m.precision));
    let mean_context_recall = mean_opt(per_context.iter().filter_map(|m| m.recall));

    CaseMetrics {
        case_id: run.case_id.clone(),
        per_recall,
        mean_precision_at_k: mean_precision,
        mean_recall_at_k: mean_recall,
        mean_mrr,
        per_context,
        mean_context_precision,
        mean_context_recall,
    }
}

/// Aggregate across all cases. Means are weighted by recall count, not case
/// count.
pub fn aggregate(runs: &[CaseRun], k: usize) -> DatasetMetrics {
    let per_case: Vec<CaseMetrics> = runs.iter().map(|r| score_case(r, k)).collect();

    let all_recalls: Vec<&RecallMetrics> =
        per_case.iter().flat_map(|c| c.per_recall.iter()).collect();
    let num_recalls = all_recalls.len();

    let mean_precision =
        mean_opt(all_recalls.iter().filter_map(|m| m.precision_at_k)).unwrap_or(0.0);
    let mean_recall = mean_opt(all_recalls.iter().filter_map(|m| m.recall_at_k)).unwrap_or(0.0);
    let mean_mrr = mean_opt(all_recalls.iter().filter_map(|m| m.mrr)).unwrap_or(0.0);

    let num_scored = all_recalls
        .iter()
        .filter(|m| m.precision_at_k.is_some())
        .count();
    let num_negative = all_recalls
        .iter()
        .filter(|m| m.negative_pass.is_some())
        .count();
    let negative_passes = all_recalls
        .iter()
        .filter(|m| matches!(m.negative_pass, Some(true)))
        .count();
    let negative_pass_rate = if num_negative == 0 {
        0.0
    } else {
        negative_passes as f64 / num_negative as f64
    };

    let all_contexts: Vec<&ContextMetrics> =
        per_case.iter().flat_map(|c| c.per_context.iter()).collect();
    let num_contexts = all_contexts.len();
    let num_contexts_scored = all_contexts
        .iter()
        .filter(|m| m.precision.is_some())
        .count();
    let num_contexts_negative = all_contexts
        .iter()
        .filter(|m| m.negative_pass.is_some())
        .count();
    let mean_context_precision =
        mean_opt(all_contexts.iter().filter_map(|m| m.precision)).unwrap_or(0.0);
    let mean_context_recall = mean_opt(all_contexts.iter().filter_map(|m| m.recall)).unwrap_or(0.0);
    let context_negative_passes = all_contexts
        .iter()
        .filter(|m| matches!(m.negative_pass, Some(true)))
        .count();
    let context_negative_pass_rate = if num_contexts_negative == 0 {
        0.0
    } else {
        context_negative_passes as f64 / num_contexts_negative as f64
    };

    DatasetMetrics {
        k,
        num_cases: runs.len(),
        num_recalls,
        num_scored,
        num_negative,
        negative_pass_rate,
        mean_precision_at_k: mean_precision,
        mean_recall_at_k: mean_recall,
        mean_mrr,
        num_contexts,
        num_contexts_scored,
        num_contexts_negative,
        mean_context_precision,
        mean_context_recall,
        context_negative_pass_rate,
        per_case,
    }
}

fn mean_opt<I: Iterator<Item = f64>>(iter: I) -> Option<f64> {
    let (sum, n) = iter.fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    if n == 0 {
        None
    } else {
        Some(sum / n as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn recall(query: &str, expected: &[&str], returned: &[&str]) -> RecallRun {
        RecallRun {
            query: query.to_string(),
            expected_labels: expected.iter().map(|s| s.to_string()).collect(),
            returned_ids: returned.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn context(expected: &[&str], returned: &[&str]) -> ContextRun {
        ContextRun {
            expected_labels: expected.iter().map(|s| s.to_string()).collect(),
            returned_procedural_ids: returned.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn perfect_top1_hit() {
        // expected = {rule-a} → uuid-a. Returned [uuid-a, uuid-b, uuid-c].
        // p@3 = 1/3 (hits / min(k, |ret|) = 1/3), r@3 = 1/1, mrr = 1.0.
        let l = labels(&[("rule-a", "uuid-a")]);
        let r = recall("q", &["rule-a"], &["uuid-a", "uuid-b", "uuid-c"]);
        let m = score_recall(&r, &l, 3);
        assert!((m.precision_at_k.unwrap() - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(m.recall_at_k, Some(1.0));
        assert_eq!(m.mrr, Some(1.0));
        assert_eq!(m.negative_pass, None);
    }

    #[test]
    fn second_rank_mrr() {
        let l = labels(&[("rule-a", "uuid-a")]);
        let r = recall("q", &["rule-a"], &["uuid-x", "uuid-a", "uuid-y"]);
        let m = score_recall(&r, &l, 3);
        assert_eq!(m.mrr, Some(0.5));
        assert_eq!(m.recall_at_k, Some(1.0));
    }

    #[test]
    fn miss_scores_zero() {
        let l = labels(&[("rule-a", "uuid-a")]);
        let r = recall("q", &["rule-a"], &["uuid-x", "uuid-y"]);
        let m = score_recall(&r, &l, 5);
        assert_eq!(m.precision_at_k, Some(0.0));
        assert_eq!(m.recall_at_k, Some(0.0));
        assert_eq!(m.mrr, Some(0.0));
    }

    #[test]
    fn precision_denominator_caps_at_returned_len() {
        // B1 regression: one hit in two returned at k=5 must give
        // precision = 1/2 (not 1/5). TREC convention.
        let l = labels(&[("rule-a", "uuid-a")]);
        let r = recall("q", &["rule-a"], &["uuid-a", "uuid-y"]);
        let m = score_recall(&r, &l, 5);
        assert_eq!(m.precision_at_k, Some(0.5));
    }

    #[test]
    fn empty_returns_is_precision_zero_on_positive_case() {
        let l = labels(&[("rule-a", "uuid-a")]);
        let r = recall("q", &["rule-a"], &[]);
        let m = score_recall(&r, &l, 5);
        assert_eq!(m.precision_at_k, Some(0.0));
        assert_eq!(m.recall_at_k, Some(0.0));
        assert_eq!(m.mrr, Some(0.0));
    }

    #[test]
    fn unresolved_label_counts_as_miss() {
        // rule-a resolves; rule-b is deduplicated / unknown → denominator 2.
        let l = labels(&[("rule-a", "uuid-a")]);
        let r = recall("q", &["rule-a", "rule-b"], &["uuid-a"]);
        let m = score_recall(&r, &l, 5);
        // 1 hit / 1 returned = 1.0 precision under TREC.
        assert_eq!(m.precision_at_k, Some(1.0));
        // 1 hit out of 2 relevant → 0.5.
        assert_eq!(m.recall_at_k, Some(0.5));
        assert_eq!(m.mrr, Some(1.0));
    }

    #[test]
    fn negative_case_empty_returns_passes() {
        let l = HashMap::new();
        let r = recall("q", &[], &[]);
        let m = score_recall(&r, &l, 5);
        assert_eq!(m.precision_at_k, None);
        assert_eq!(m.recall_at_k, None);
        assert_eq!(m.mrr, None);
        assert_eq!(m.negative_pass, Some(true));
    }

    #[test]
    fn negative_case_nonempty_returns_fails() {
        let l = HashMap::new();
        let r = recall("q", &[], &["uuid-x"]);
        let m = score_recall(&r, &l, 5);
        assert_eq!(m.precision_at_k, None);
        assert_eq!(m.negative_pass, Some(false));
    }

    #[test]
    fn topk_truncates_returned_list() {
        // k=2, hit at rank 3 → outside window, so precision=0, recall=0, MRR=0.
        let l = labels(&[("rule-a", "uuid-a")]);
        let r = recall("q", &["rule-a"], &["uuid-x", "uuid-y", "uuid-a"]);
        let m = score_recall(&r, &l, 2);
        assert_eq!(m.precision_at_k, Some(0.0));
        assert_eq!(m.recall_at_k, Some(0.0));
        assert_eq!(m.mrr, Some(0.0));
    }

    #[test]
    fn aggregate_means_over_recalls_not_cases() {
        // Case A: 1 recall, precision 1.0.
        // Case B: 3 recalls, precisions 0, 0, 0.
        // Recall-weighted mean = (1 + 0 + 0 + 0) / 4 = 0.25.
        let case_a = CaseRun {
            case_id: "a".into(),
            labels: labels(&[("rule-a", "uuid-a")]),
            recalls: vec![recall("q", &["rule-a"], &["uuid-a"])],
            contexts: vec![],
            deduplicated_labels: vec![],
        };
        let case_b = CaseRun {
            case_id: "b".into(),
            labels: labels(&[("rule-b", "uuid-b")]),
            recalls: vec![
                recall("q", &["rule-b"], &["uuid-z"]),
                recall("q", &["rule-b"], &["uuid-z"]),
                recall("q", &["rule-b"], &["uuid-z"]),
            ],
            contexts: vec![],
            deduplicated_labels: vec![],
        };
        let agg = aggregate(&[case_a, case_b], 1);
        assert_eq!(agg.num_cases, 2);
        assert_eq!(agg.num_recalls, 4);
        assert_eq!(agg.num_scored, 4);
        assert_eq!(agg.num_negative, 0);
        assert!((agg.mean_precision_at_k - 0.25).abs() < 1e-9);
    }

    #[test]
    fn context_perfect_hit() {
        let l = labels(&[("rule-a", "uuid-a"), ("rule-b", "uuid-b")]);
        let c = context(&["rule-a", "rule-b"], &["uuid-a", "uuid-b"]);
        let m = score_context(&c, &l);
        assert_eq!(m.precision, Some(1.0));
        assert_eq!(m.recall, Some(1.0));
        assert_eq!(m.negative_pass, None);
    }

    #[test]
    fn context_partial_precision() {
        // 1 hit out of 2 returned → precision 0.5; 1 hit out of 1 expected → recall 1.0.
        let l = labels(&[("rule-a", "uuid-a")]);
        let c = context(&["rule-a"], &["uuid-a", "uuid-noise"]);
        let m = score_context(&c, &l);
        assert_eq!(m.precision, Some(0.5));
        assert_eq!(m.recall, Some(1.0));
    }

    #[test]
    fn context_partial_recall() {
        // 1 hit out of 1 returned → precision 1.0; 1 hit out of 2 expected → recall 0.5.
        let l = labels(&[("rule-a", "uuid-a"), ("rule-b", "uuid-b")]);
        let c = context(&["rule-a", "rule-b"], &["uuid-a"]);
        let m = score_context(&c, &l);
        assert_eq!(m.precision, Some(1.0));
        assert_eq!(m.recall, Some(0.5));
    }

    #[test]
    fn context_empty_return_is_precision_zero_on_positive() {
        let l = labels(&[("rule-a", "uuid-a")]);
        let c = context(&["rule-a"], &[]);
        let m = score_context(&c, &l);
        assert_eq!(m.precision, Some(0.0));
        assert_eq!(m.recall, Some(0.0));
    }

    #[test]
    fn context_unresolved_label_counts_as_miss() {
        let l = labels(&[("rule-a", "uuid-a")]);
        let c = context(&["rule-a", "rule-b-deduped"], &["uuid-a"]);
        let m = score_context(&c, &l);
        assert_eq!(m.precision, Some(1.0));
        assert_eq!(m.recall, Some(0.5));
    }

    #[test]
    fn context_negative_empty_return_passes() {
        let l = HashMap::new();
        let c = context(&[], &[]);
        let m = score_context(&c, &l);
        assert_eq!(m.precision, None);
        assert_eq!(m.recall, None);
        assert_eq!(m.negative_pass, Some(true));
    }

    #[test]
    fn context_negative_nonempty_return_fails() {
        let l = HashMap::new();
        let c = context(&[], &["uuid-x"]);
        let m = score_context(&c, &l);
        assert_eq!(m.negative_pass, Some(false));
    }

    #[test]
    fn aggregate_counts_context_channel_separately() {
        let case = CaseRun {
            case_id: "ctx".into(),
            labels: labels(&[("rule-a", "uuid-a"), ("rule-b", "uuid-b")]),
            recalls: vec![recall("q", &["rule-a"], &["uuid-a"])],
            contexts: vec![
                context(&["rule-a"], &["uuid-a"]),
                context(&["rule-b"], &["uuid-x"]),
                context(&[], &[]),
            ],
            deduplicated_labels: vec![],
        };
        let agg = aggregate(&[case], 10);
        assert_eq!(agg.num_contexts, 3);
        assert_eq!(agg.num_contexts_scored, 2);
        assert_eq!(agg.num_contexts_negative, 1);
        // Two positives: (1.0, 0.0) → mean 0.5.
        assert!((agg.mean_context_precision - 0.5).abs() < 1e-9);
        assert!((agg.mean_context_recall - 0.5).abs() < 1e-9);
        assert_eq!(agg.context_negative_pass_rate, 1.0);
    }

    #[test]
    fn aggregate_context_channel_zero_events_is_zero() {
        // Lock in `0.0` (not `None`) when no context events exist, mirroring
        // how `mean_precision_at_k` degrades when no recalls are scored.
        let case = CaseRun {
            case_id: "no-ctx".into(),
            labels: labels(&[("rule-a", "uuid-a")]),
            recalls: vec![recall("q", &["rule-a"], &["uuid-a"])],
            contexts: vec![],
            deduplicated_labels: vec![],
        };
        let agg = aggregate(&[case], 1);
        assert_eq!(agg.num_contexts, 0);
        assert_eq!(agg.num_contexts_scored, 0);
        assert_eq!(agg.num_contexts_negative, 0);
        assert_eq!(agg.mean_context_precision, 0.0);
        assert_eq!(agg.mean_context_recall, 0.0);
        assert_eq!(agg.context_negative_pass_rate, 0.0);
    }

    #[test]
    fn aggregate_separates_negative_from_ranking_means() {
        // Two positive (p=1.0 each) + two negative (1 pass, 1 fail). Ranking
        // mean should stay at 1.0 uncontaminated by the negatives.
        let case = CaseRun {
            case_id: "mixed".into(),
            labels: labels(&[("rule-a", "uuid-a")]),
            recalls: vec![
                recall("q", &["rule-a"], &["uuid-a"]),
                recall("q", &["rule-a"], &["uuid-a"]),
                recall("q", &[], &[]),
                recall("q", &[], &["uuid-noise"]),
            ],
            contexts: vec![],
            deduplicated_labels: vec![],
        };
        let agg = aggregate(&[case], 1);
        assert_eq!(agg.num_scored, 2);
        assert_eq!(agg.num_negative, 2);
        assert_eq!(agg.mean_precision_at_k, 1.0);
        assert!((agg.negative_pass_rate - 0.5).abs() < 1e-9);
    }
}
