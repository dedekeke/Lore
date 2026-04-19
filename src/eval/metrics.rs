//! Retrieval metrics for eval replay runs.
//!
//! Pure, deterministic scoring over the `CaseRun` output from
//! `super::harness::replay_case`. No I/O, no async. The harness produces
//! raw inputs/outputs; this module grades them.
//!
//! Definitions:
//!
//! - **precision@k** = |retrieved_top_k ∩ relevant| / k
//! - **recall@k** = |retrieved_top_k ∩ relevant| / |relevant|
//! - **MRR** = mean of 1/rank of the first relevant hit per query (0 when
//!   no relevant result appears in top-k)
//!
//! Labels referenced by `expected_hits` that did NOT resolve to a UUID at
//! replay time (server's near-duplicate guard short-circuited, or the label
//! points at a task/attempt that `recall_rules` cannot return) count in
//! recall's denominator but can never contribute to the numerator — they
//! score as misses, as documented in `eval/README.md`.

use std::collections::{HashMap, HashSet};

use super::harness::{CaseRun, RecallRun};

/// Metrics for a single `RecallRules` event.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallMetrics {
    pub k: usize,
    pub precision_at_k: f64,
    /// `None` when `expected_hits` is empty — recall is undefined.
    pub recall_at_k: Option<f64>,
    /// `None` when `expected_hits` is empty.
    pub mrr: Option<f64>,
}

/// Metrics rolled up per case (mean across its `RecallRun`s).
#[derive(Debug, Clone, PartialEq)]
pub struct CaseMetrics {
    pub case_id: String,
    pub per_recall: Vec<RecallMetrics>,
    pub mean_precision_at_k: f64,
    pub mean_recall_at_k: Option<f64>,
    pub mean_mrr: Option<f64>,
}

/// Dataset-level aggregate. Means are taken over individual `RecallRun`s,
/// not over cases, so a case with more queries contributes proportionally.
#[derive(Debug, Clone, PartialEq)]
pub struct DatasetMetrics {
    pub k: usize,
    pub num_cases: usize,
    pub num_recalls: usize,
    pub mean_precision_at_k: f64,
    pub mean_recall_at_k: f64,
    pub mean_mrr: f64,
    pub per_case: Vec<CaseMetrics>,
}

/// Score one recall query. `labels` is the case-local `label -> UUID` map
/// built by the harness.
pub fn score_recall(run: &RecallRun, labels: &HashMap<String, String>, k: usize) -> RecallMetrics {
    assert!(k > 0, "k must be >= 1");

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

    // Empty expected set: precision is 1.0 when nothing returned, 0.0 otherwise;
    // recall/MRR are undefined. This matches the mini-011 "assert no hits" case.
    if run.expected_labels.is_empty() {
        let precision = if run.returned_ids.is_empty() {
            1.0
        } else {
            0.0
        };
        return RecallMetrics {
            k,
            precision_at_k: precision,
            recall_at_k: None,
            mrr: None,
        };
    }

    // Denominator includes unresolved labels (deduplicated or task-/attempt-*
    // that recall_rules can't return) — they score as misses.
    let relevant_total = run.expected_labels.len() as f64;
    let precision = hits as f64 / k as f64;
    let recall = hits as f64 / relevant_total;

    let rr = top_k
        .iter()
        .position(|id| expected_uuids.contains(*id))
        .map(|i| 1.0 / (i + 1) as f64)
        .unwrap_or(0.0);

    RecallMetrics {
        k,
        precision_at_k: precision,
        recall_at_k: Some(recall),
        mrr: Some(rr),
    }
}

/// Score all recalls in a case and compute the case-level mean. Recall/MRR
/// means skip queries whose metric is `None` (empty expected set).
pub fn score_case(run: &CaseRun, k: usize) -> CaseMetrics {
    let per_recall: Vec<RecallMetrics> = run
        .recalls
        .iter()
        .map(|r| score_recall(r, &run.labels, k))
        .collect();

    let mean_precision = mean(per_recall.iter().map(|m| m.precision_at_k));
    let mean_recall = mean_opt(per_recall.iter().filter_map(|m| m.recall_at_k));
    let mean_mrr = mean_opt(per_recall.iter().filter_map(|m| m.mrr));

    CaseMetrics {
        case_id: run.case_id.clone(),
        per_recall,
        mean_precision_at_k: mean_precision,
        mean_recall_at_k: mean_recall,
        mean_mrr,
    }
}

/// Aggregate metrics across all cases. Means are over individual recalls.
pub fn aggregate(runs: &[CaseRun], k: usize) -> DatasetMetrics {
    let per_case: Vec<CaseMetrics> = runs.iter().map(|r| score_case(r, k)).collect();

    let all_recalls: Vec<&RecallMetrics> =
        per_case.iter().flat_map(|c| c.per_recall.iter()).collect();
    let num_recalls = all_recalls.len();

    let mean_precision = mean(all_recalls.iter().map(|m| m.precision_at_k));
    // Recall/MRR: only recalls with a defined metric (expected_labels non-empty)
    // contribute. If every case is a "negative" case, return 0.0 rather than NaN.
    let mean_recall = mean_opt(all_recalls.iter().filter_map(|m| m.recall_at_k)).unwrap_or(0.0);
    let mean_mrr = mean_opt(all_recalls.iter().filter_map(|m| m.mrr)).unwrap_or(0.0);

    DatasetMetrics {
        k,
        num_cases: runs.len(),
        num_recalls,
        mean_precision_at_k: mean_precision,
        mean_recall_at_k: mean_recall,
        mean_mrr,
        per_case,
    }
}

fn mean<I: Iterator<Item = f64>>(iter: I) -> f64 {
    let (sum, n) = iter.fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    if n == 0 {
        0.0
    } else {
        sum / n as f64
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

    #[test]
    fn perfect_top1_hit() {
        // expected = {rule-a} → uuid-a. Returned [uuid-a, uuid-b, uuid-c].
        // p@3 = 1/3, r@3 = 1/1 = 1.0, mrr = 1.0.
        let l = labels(&[("rule-a", "uuid-a")]);
        let r = recall("q", &["rule-a"], &["uuid-a", "uuid-b", "uuid-c"]);
        let m = score_recall(&r, &l, 3);
        assert!((m.precision_at_k - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(m.recall_at_k, Some(1.0));
        assert_eq!(m.mrr, Some(1.0));
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
    fn miss_scores_zero_mrr() {
        let l = labels(&[("rule-a", "uuid-a")]);
        let r = recall("q", &["rule-a"], &["uuid-x", "uuid-y"]);
        let m = score_recall(&r, &l, 5);
        assert_eq!(m.precision_at_k, 0.0);
        assert_eq!(m.recall_at_k, Some(0.0));
        assert_eq!(m.mrr, Some(0.0));
    }

    #[test]
    fn unresolved_label_counts_as_miss() {
        // rule-a resolves; rule-b is deduplicated / unknown.
        let l = labels(&[("rule-a", "uuid-a")]);
        let r = recall("q", &["rule-a", "rule-b"], &["uuid-a"]);
        let m = score_recall(&r, &l, 5);
        // 1 hit / 5 = 0.2
        assert!((m.precision_at_k - 0.2).abs() < 1e-9);
        // 1 hit out of 2 relevant → 0.5
        assert_eq!(m.recall_at_k, Some(0.5));
        assert_eq!(m.mrr, Some(1.0));
    }

    #[test]
    fn empty_expected_with_empty_returns_is_precision_one() {
        let l = HashMap::new();
        let r = recall("q", &[], &[]);
        let m = score_recall(&r, &l, 5);
        assert_eq!(m.precision_at_k, 1.0);
        assert_eq!(m.recall_at_k, None);
        assert_eq!(m.mrr, None);
    }

    #[test]
    fn empty_expected_with_nonempty_returns_is_precision_zero() {
        let l = HashMap::new();
        let r = recall("q", &[], &["uuid-x"]);
        let m = score_recall(&r, &l, 5);
        assert_eq!(m.precision_at_k, 0.0);
        assert_eq!(m.recall_at_k, None);
        assert_eq!(m.mrr, None);
    }

    #[test]
    fn topk_truncates_returned_list() {
        // k=2 with a hit at rank 3 → MRR = 0 (outside window), recall = 0.
        let l = labels(&[("rule-a", "uuid-a")]);
        let r = recall("q", &["rule-a"], &["uuid-x", "uuid-y", "uuid-a"]);
        let m = score_recall(&r, &l, 2);
        assert_eq!(m.precision_at_k, 0.0);
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
            deduplicated_labels: vec![],
        };
        let agg = aggregate(&[case_a, case_b], 1);
        assert_eq!(agg.num_cases, 2);
        assert_eq!(agg.num_recalls, 4);
        assert!((agg.mean_precision_at_k - 0.25).abs() < 1e-9);
    }

    #[test]
    fn aggregate_skips_undefined_recall_metrics() {
        // Case with only "negative" (empty-expected) recalls → recall/MRR
        // have no defined samples; aggregate reports 0.0 not NaN.
        let case = CaseRun {
            case_id: "neg".into(),
            labels: HashMap::new(),
            recalls: vec![recall("q", &[], &[])],
            deduplicated_labels: vec![],
        };
        let agg = aggregate(&[case], 5);
        assert_eq!(agg.mean_recall_at_k, 0.0);
        assert_eq!(agg.mean_mrr, 0.0);
        assert_eq!(agg.mean_precision_at_k, 1.0);
    }
}
