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
use std::fmt;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::metrics::DatasetMetrics;

/// Default absolute tolerance for metric drift. Picked to swallow
/// floating-point noise + minor embedding variance without masking real
/// quality regressions.
///
/// A drop is a regression iff `current < baseline - tolerance`, i.e.
/// strictly greater than `tolerance` in magnitude. `current == baseline - tolerance`
/// exactly is treated as passing (boundary is inclusive on the "safe" side).
/// NaN deltas never flag a regression — that case must be caught upstream.
pub const DEFAULT_TOLERANCE: f64 = 0.02;

/// Current baseline schema version. Bump on any breaking change to the
/// on-disk format; the loader refuses unknown versions.
pub const BASELINE_SCHEMA_VERSION: u32 = 1;

/// Wrapper around one or more `DatasetMetrics` keyed by dataset name
/// (`"mini"`, `"locomo"`, etc.). `BTreeMap` keeps the JSON stable under
/// reordering so the baseline file is diff-friendly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    let mut raw = serde_json::to_string_pretty(baseline)?;
    // Trailing newline keeps editors and diff tools from reporting churn.
    raw.push('\n');
    fs::write(path, raw)?;
    Ok(())
}

/// Per-metric diff. Positive `delta` = current improved over baseline.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricDelta {
    /// Metric name, e.g. `"mean_precision_at_k"`. Static so diff output
    /// can cite it directly in CI logs without allocation.
    pub name: &'static str,
    pub baseline: f64,
    pub current: f64,
    /// `current - baseline`. Positive = improvement, negative = drop.
    pub delta: f64,
    /// `true` when `delta < -tolerance` (strict). Improvements and
    /// non-finite deltas never flag regression.
    pub regression: bool,
}

/// Report for a single dataset in the baseline.
#[derive(Debug, Clone, PartialEq)]
pub struct DatasetReport {
    /// Dataset name matching a key in `Baseline::datasets`.
    pub name: String,
    /// One entry per scored aggregate (precision, recall, MRR, negative
    /// pass rate). Order is stable across runs.
    pub deltas: Vec<MetricDelta>,
    /// Shape mismatches (k, case/recall/scored/negative counts). Any
    /// non-empty entry fails hard — the fixture or scoring semantics
    /// changed, not just the model.
    pub shape_mismatches: Vec<String>,
}

impl DatasetReport {
    pub fn has_regression(&self) -> bool {
        self.deltas.iter().any(|d| d.regression) || !self.shape_mismatches.is_empty()
    }
}

/// Top-level diff of current-run metrics vs stored baseline. Suitable for
/// direct consumption by the P1-T5 CI delta bot — `Display` produces a
/// human-readable summary; the structured fields support machine output.
#[derive(Debug, Clone, PartialEq)]
pub struct CompareReport {
    pub tolerance: f64,
    pub datasets: Vec<DatasetReport>,
    /// Datasets referenced by the baseline but absent from the current run.
    /// Always a hard failure.
    pub missing_datasets: Vec<String>,
    /// Datasets in the current run that are not in the baseline. Always a
    /// hard failure — baseline coverage should be explicit, not implicit.
    pub extra_datasets: Vec<String>,
}

impl CompareReport {
    pub fn has_regression(&self) -> bool {
        !self.missing_datasets.is_empty()
            || !self.extra_datasets.is_empty()
            || self.datasets.iter().any(|d| d.has_regression())
    }
}

impl fmt::Display for CompareReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "baseline compare (tolerance={:.4}):", self.tolerance)?;
        if !self.missing_datasets.is_empty() {
            writeln!(f, "  missing datasets: {:?}", self.missing_datasets)?;
        }
        if !self.extra_datasets.is_empty() {
            writeln!(f, "  extra datasets:   {:?}", self.extra_datasets)?;
        }
        for d in &self.datasets {
            writeln!(f, "  [{}]", d.name)?;
            for m in &d.deltas {
                writeln!(
                    f,
                    "    {:<20} baseline={:.4} current={:.4} delta={:+.4}{}",
                    m.name,
                    m.baseline,
                    m.current,
                    m.delta,
                    if m.regression { "  REGRESSION" } else { "" },
                )?;
            }
            for s in &d.shape_mismatches {
                writeln!(f, "    SHAPE: {s}")?;
            }
        }
        if self.has_regression() {
            writeln!(f, "  => REGRESSION")?;
        } else {
            writeln!(f, "  => ok")?;
        }
        Ok(())
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

// Note: `per_case` is intentionally snapshot-only and NOT diffed here.
// Diffing per-case `Option<f64>` means with unstable ordering/tie-breaks
// would generate false regressions that dwarf the real aggregate signal.
// If per-case drift needs investigation, eyeball `eval/baselines.json`.
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
    // NaN-safe: only finite negative drifts past tolerance count as regressions.
    // A NaN metric upstream is a bug in `aggregate`, not a quality regression;
    // surfacing it here would mask the real failure mode.
    let regression = delta.is_finite() && delta < -tol;
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
    fn drop_exactly_equal_to_tolerance_passes() {
        // Boundary case: delta = -tolerance exactly. Policy: boundary is
        // inclusive on the "safe" side; only strictly-greater magnitudes
        // regress. Uses exact binary fractions (0.5, 0.25) so the delta is
        // representable and the test is not hostage to FP rounding.
        let base = base_with(dm(0.5, 0.6, 0.7));
        let cur = current_with(dm(0.25, 0.6, 0.7));
        let report = compare(&base, &cur, 0.25);
        assert!(!report.has_regression());
    }

    #[test]
    fn drop_just_past_tolerance_regresses() {
        // Drop magnitude strictly exceeds tolerance.
        let base = base_with(dm(0.5, 0.6, 0.7));
        let cur = current_with(dm(0.125, 0.6, 0.7));
        let report = compare(&base, &cur, 0.25);
        assert!(report.has_regression());
    }

    #[test]
    fn nan_delta_does_not_regress() {
        // Safety: NaN should never silently flag OR silently pass as a regression —
        // regression stays false (the caller should fail on the upstream NaN).
        let base = base_with(dm(0.5, 0.6, 0.7));
        let cur = current_with(dm(f64::NAN, 0.6, 0.7));
        let report = compare(&base, &cur, DEFAULT_TOLERANCE);
        let precision = report.datasets[0]
            .deltas
            .iter()
            .find(|d| d.name == "mean_precision_at_k")
            .unwrap();
        assert!(!precision.regression);
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
    fn num_scored_shape_drift_regresses() {
        let base = base_with(dm(0.5, 0.6, 0.7));
        let mut drifted = dm(0.5, 0.6, 0.7);
        drifted.num_scored = 10; // was 11
        let cur = current_with(drifted);
        let report = compare(&base, &cur, DEFAULT_TOLERANCE);
        assert!(report.has_regression());
        assert!(report.datasets[0]
            .shape_mismatches
            .iter()
            .any(|m| m.contains("num_scored")));
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

    #[test]
    fn unknown_field_rejected_on_load() {
        // `deny_unknown_fields` on Baseline means CI catches forward-incompatible
        // changes instead of silently ignoring them.
        let bad = r#"{"version":1,"metadata":{},"datasets":{},"rogue":true}"#;
        let err = serde_json::from_str::<Baseline>(bad).unwrap_err();
        assert!(
            err.to_string().contains("rogue") || err.to_string().contains("unknown"),
            "expected unknown-field rejection, got: {err}"
        );
    }

    #[test]
    fn display_summarises_regression() {
        let base = base_with(dm(0.5, 0.6, 0.7));
        let cur = current_with(dm(0.2, 0.6, 0.7));
        let report = compare(&base, &cur, DEFAULT_TOLERANCE);
        let s = format!("{report}");
        assert!(s.contains("REGRESSION"), "{s}");
        assert!(s.contains("mean_precision_at_k"), "{s}");
    }

    #[test]
    fn display_ok_on_clean_compare() {
        let base = base_with(dm(0.5, 0.6, 0.7));
        let cur = current_with(dm(0.5, 0.6, 0.7));
        let report = compare(&base, &cur, DEFAULT_TOLERANCE);
        let s = format!("{report}");
        assert!(s.contains("=> ok"), "{s}");
        assert!(!s.contains("REGRESSION"), "{s}");
    }
}
