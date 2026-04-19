//! Baseline capture + comparison for eval metrics.
//!
//! `eval/baselines.json` is the source of truth. CI replays the fixtures
//! against develop HEAD and compares the fresh `DatasetMetrics` against the
//! stored baseline; regressions outside `DEFAULT_TOLERANCE` fail the build.
//!
//! **Update policy**: baseline changes land in their own review-gated PR
//! that spells out *why* (model swap, schema change, scoring tweak).
//! Accidental drift from unrelated PRs must be reverted — see
//! `eval/README.md` ("Baseline update policy").

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::metrics::DatasetMetrics;

/// Default absolute tolerance for metric drift. Picked to swallow
/// floating-point noise + minor embedding variance without masking real
/// quality regressions.
pub const DEFAULT_TOLERANCE: f64 = 0.02;

/// Current baseline schema version. Bump on any breaking change to the
/// on-disk format; the loader refuses unknown versions.
pub const BASELINE_SCHEMA_VERSION: u32 = 1;

/// Wrapper around one or more `DatasetMetrics` keyed by dataset name
/// (`"mini"`, `"locomo"`, etc.). `BTreeMap` keeps the JSON stable under
/// reordering so the baseline file is diff-friendly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub version: u32,
    /// Free-form metadata about how the baseline was produced (git sha,
    /// embedding provider, model id, etc.). Not compared.
    pub metadata: BTreeMap<String, String>,
    pub datasets: BTreeMap<String, DatasetMetrics>,
}

impl Baseline {
    pub fn new(datasets: BTreeMap<String, DatasetMetrics>) -> Self {
        Self {
            version: BASELINE_SCHEMA_VERSION,
            metadata: BTreeMap::new(),
            datasets,
        }
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

#[derive(Debug, Error)]
pub enum BaselineError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "baseline schema version mismatch: file reports {found}, binary expects {expected}. \
         Regenerate the baseline or upgrade the loader."
    )]
    VersionMismatch { found: u32, expected: u32 },
}

pub fn load_baseline(path: &Path) -> Result<Baseline, BaselineError> {
    let raw = fs::read_to_string(path)?;
    let baseline: Baseline = serde_json::from_str(&raw)?;
    if baseline.version != BASELINE_SCHEMA_VERSION {
        return Err(BaselineError::VersionMismatch {
            found: baseline.version,
            expected: BASELINE_SCHEMA_VERSION,
        });
    }
    Ok(baseline)
}

pub fn save_baseline(path: &Path, baseline: &Baseline) -> Result<(), BaselineError> {
    let raw = serde_json::to_string_pretty(baseline)?;
    // Trailing newline keeps editors and diff tools from reporting churn.
    fs::write(path, format!("{raw}\n"))?;
    Ok(())
}

/// Per-metric diff. Positive `delta` = current improved over baseline.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricDelta {
    pub name: &'static str,
    pub baseline: f64,
    pub current: f64,
    pub delta: f64,
    /// `true` when `|delta| > tolerance` AND the delta is a regression
    /// (current < baseline). An improvement is never a regression.
    pub regression: bool,
}

/// Report for a single dataset in the baseline.
#[derive(Debug, Clone, PartialEq)]
pub struct DatasetReport {
    pub name: String,
    pub deltas: Vec<MetricDelta>,
    /// Shape mismatches (case count, recall count, etc.) fail hard —
    /// they mean the fixture or schema drifted, not the model.
    pub shape_mismatches: Vec<String>,
}

impl DatasetReport {
    pub fn has_regression(&self) -> bool {
        self.deltas.iter().any(|d| d.regression) || !self.shape_mismatches.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompareReport {
    pub tolerance: f64,
    pub datasets: Vec<DatasetReport>,
    /// Datasets referenced by the baseline but missing from the current run
    /// (or vice versa). Always a hard failure.
    pub missing_datasets: Vec<String>,
    pub extra_datasets: Vec<String>,
}

impl CompareReport {
    pub fn has_regression(&self) -> bool {
        !self.missing_datasets.is_empty()
            || !self.extra_datasets.is_empty()
            || self.datasets.iter().any(|d| d.has_regression())
    }
}

/// Compare current metrics against baseline. `tolerance` is the absolute
/// threshold at which a drop is counted as a regression. Applied to the
/// four aggregate fields; shape mismatches are strict.
pub fn compare(
    baseline: &Baseline,
    current: &BTreeMap<String, DatasetMetrics>,
    tolerance: f64,
) -> CompareReport {
    let missing: Vec<String> = baseline
        .datasets
        .keys()
        .filter(|k| !current.contains_key(*k))
        .cloned()
        .collect();
    let extra: Vec<String> = current
        .keys()
        .filter(|k| !baseline.datasets.contains_key(*k))
        .cloned()
        .collect();

    let datasets = baseline
        .datasets
        .iter()
        .filter_map(|(name, base)| {
            current
                .get(name)
                .map(|cur| diff_dataset(name, base, cur, tolerance))
        })
        .collect();

    CompareReport {
        tolerance,
        datasets,
        missing_datasets: missing,
        extra_datasets: extra,
    }
}

fn diff_dataset(
    name: &str,
    base: &DatasetMetrics,
    cur: &DatasetMetrics,
    tolerance: f64,
) -> DatasetReport {
    let mut shape_mismatches = Vec::new();
    if base.k != cur.k {
        shape_mismatches.push(format!("k: baseline={} current={}", base.k, cur.k));
    }
    if base.num_cases != cur.num_cases {
        shape_mismatches.push(format!(
            "num_cases: baseline={} current={}",
            base.num_cases, cur.num_cases
        ));
    }
    if base.num_recalls != cur.num_recalls {
        shape_mismatches.push(format!(
            "num_recalls: baseline={} current={}",
            base.num_recalls, cur.num_recalls
        ));
    }
    if base.num_scored != cur.num_scored {
        shape_mismatches.push(format!(
            "num_scored: baseline={} current={}",
            base.num_scored, cur.num_scored
        ));
    }
    if base.num_negative != cur.num_negative {
        shape_mismatches.push(format!(
            "num_negative: baseline={} current={}",
            base.num_negative, cur.num_negative
        ));
    }

    let deltas = vec![
        metric_delta(
            "mean_precision_at_k",
            base.mean_precision_at_k,
            cur.mean_precision_at_k,
            tolerance,
        ),
        metric_delta(
            "mean_recall_at_k",
            base.mean_recall_at_k,
            cur.mean_recall_at_k,
            tolerance,
        ),
        metric_delta("mean_mrr", base.mean_mrr, cur.mean_mrr, tolerance),
        metric_delta(
            "negative_pass_rate",
            base.negative_pass_rate,
            cur.negative_pass_rate,
            tolerance,
        ),
    ];

    DatasetReport {
        name: name.to_string(),
        deltas,
        shape_mismatches,
    }
}

fn metric_delta(name: &'static str, base: f64, cur: f64, tol: f64) -> MetricDelta {
    let delta = cur - base;
    let regression = delta < -tol;
    MetricDelta {
        name,
        baseline: base,
        current: cur,
        delta,
        regression,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn dm(p: f64, r: f64, mrr: f64) -> DatasetMetrics {
        DatasetMetrics {
            k: 10,
            num_cases: 12,
            num_recalls: 12,
            num_scored: 11,
            num_negative: 1,
            negative_pass_rate: 0.0,
            mean_precision_at_k: p,
            mean_recall_at_k: r,
            mean_mrr: mrr,
            per_case: vec![],
        }
    }

    fn base_with(metrics: DatasetMetrics) -> Baseline {
        let mut d = BTreeMap::new();
        d.insert("mini".to_string(), metrics);
        Baseline::new(d)
    }

    fn current_with(metrics: DatasetMetrics) -> BTreeMap<String, DatasetMetrics> {
        let mut d = BTreeMap::new();
        d.insert("mini".to_string(), metrics);
        d
    }

    #[test]
    fn identical_metrics_have_no_regression() {
        let base = base_with(dm(0.5, 0.6, 0.7));
        let cur = current_with(dm(0.5, 0.6, 0.7));
        let report = compare(&base, &cur, DEFAULT_TOLERANCE);
        assert!(!report.has_regression());
        assert!(report.missing_datasets.is_empty());
        assert!(report.extra_datasets.is_empty());
    }

    #[test]
    fn improvement_is_not_a_regression() {
        let base = base_with(dm(0.5, 0.6, 0.7));
        let cur = current_with(dm(0.8, 0.6, 0.7));
        let report = compare(&base, &cur, DEFAULT_TOLERANCE);
        assert!(!report.has_regression());
    }

    #[test]
    fn within_tolerance_drop_passes() {
        let base = base_with(dm(0.5, 0.6, 0.7));
        let cur = current_with(dm(0.49, 0.6, 0.7)); // -0.01, under 0.02 tol
        let report = compare(&base, &cur, DEFAULT_TOLERANCE);
        assert!(!report.has_regression());
    }

    #[test]
    fn precision_regression_detected() {
        let base = base_with(dm(0.5, 0.6, 0.7));
        let cur = current_with(dm(0.30, 0.6, 0.7));
        let report = compare(&base, &cur, DEFAULT_TOLERANCE);
        assert!(report.has_regression());
        let d = &report.datasets[0];
        assert!(d
            .deltas
            .iter()
            .any(|m| m.name == "mean_precision_at_k" && m.regression));
    }

    #[test]
    fn shape_mismatch_is_regression() {
        let base = base_with(dm(0.5, 0.6, 0.7));
        let mut drifted = dm(0.5, 0.6, 0.7);
        drifted.num_cases = 13; // fixture grew
        let cur = current_with(drifted);
        let report = compare(&base, &cur, DEFAULT_TOLERANCE);
        assert!(report.has_regression());
        assert!(!report.datasets[0].shape_mismatches.is_empty());
    }

    #[test]
    fn missing_dataset_is_regression() {
        let base = base_with(dm(0.5, 0.6, 0.7));
        let cur: BTreeMap<String, DatasetMetrics> = BTreeMap::new();
        let report = compare(&base, &cur, DEFAULT_TOLERANCE);
        assert!(report.has_regression());
        assert_eq!(report.missing_datasets, vec!["mini".to_string()]);
    }

    #[test]
    fn extra_dataset_is_regression() {
        let base: Baseline = Baseline::new(BTreeMap::new());
        let cur = current_with(dm(0.5, 0.6, 0.7));
        let report = compare(&base, &cur, DEFAULT_TOLERANCE);
        assert!(report.has_regression());
        assert_eq!(report.extra_datasets, vec!["mini".to_string()]);
    }

    #[test]
    fn roundtrip_via_json() {
        let original = base_with(dm(0.5, 0.6, 0.7)).with_metadata("git_sha", "deadbeef");
        let json = serde_json::to_string_pretty(&original).unwrap();
        let back: Baseline = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }
}
