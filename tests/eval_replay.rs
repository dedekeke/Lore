#![cfg(feature = "eval")]

mod common;

use lore::config::Config;
use lore::embeddings::fake::FakeEmbeddingProvider;
use lore::embeddings::AnyEmbeddingProvider;
use lore::eval::{aggregate, load_mini, replay_case};
use lore::server::LoreServer;

/// End-to-end smoke: every case in the mini fixture replays against a real
/// `LoreServer` + pgvector, every `expected_hit` label is accounted for
/// (resolved or deduplicated), and the aggregate metrics compute cleanly
/// over all cases.
#[tokio::test]
async fn mini_fixture_replays_without_error() {
    let (pool, _container) = common::setup_db().await;
    let config = Config::from_env();
    let embeddings = AnyEmbeddingProvider::Fake(FakeEmbeddingProvider::hashed(384));
    let server = LoreServer::new(pool, embeddings, config);

    let cases = load_mini().expect("mini fixture loads");
    assert!(!cases.is_empty(), "mini fixture must be non-empty");

    let mut runs = Vec::with_capacity(cases.len());

    for case in &cases {
        let run = replay_case(&server, case)
            .await
            .unwrap_or_else(|e| panic!("replay failed for {}: {e}", case.id));

        assert_eq!(run.case_id, case.id);
        // Every expected label the case references must either resolve to a
        // real UUID or be accounted for in `deduplicated_labels` (rule was
        // short-circuited by the server's near-duplicate guard). Otherwise
        // P1-T3's grader has nothing to compare against.
        for recall in &run.recalls {
            for label in &recall.expected_labels {
                let resolved = run.labels.contains_key(label);
                let deduped = run.deduplicated_labels.contains(label);
                assert!(
                    resolved || deduped,
                    "case {}: expected label {label} neither resolved nor \
                     recorded as deduplicated. Known labels: {:?}. \
                     Deduplicated: {:?}",
                    case.id,
                    run.labels.keys().collect::<Vec<_>>(),
                    run.deduplicated_labels,
                );
            }
        }

        runs.push(run);
    }

    // Aggregate metrics over the full mini set. Values aren't asserted against
    // a baseline (P1-T4), but we do sanity-check the shape: metrics compute
    // cleanly (no NaN), every recall was scored, and precision is in [0, 1].
    let metrics = aggregate(&runs, 10);
    assert_eq!(metrics.num_cases, runs.len());
    let total_recalls: usize = runs.iter().map(|r| r.recalls.len()).sum();
    assert_eq!(metrics.num_recalls, total_recalls);
    for field in [
        metrics.mean_precision_at_k,
        metrics.mean_recall_at_k,
        metrics.mean_mrr,
    ] {
        assert!(field.is_finite(), "aggregate metric NaN/inf");
        assert!((0.0..=1.0).contains(&field), "metric out of range: {field}");
    }
    assert!(
        (0.0..=1.0).contains(&metrics.negative_pass_rate),
        "negative_pass_rate out of range: {}",
        metrics.negative_pass_rate,
    );
    assert_eq!(
        metrics.num_scored + metrics.num_negative,
        metrics.num_recalls,
        "scored + negative must partition all recalls"
    );
    eprintln!(
        "mini aggregate @k=10: precision={:.3} recall={:.3} mrr={:.3} \
         neg_pass={:.3} (cases={}, scored={}, negative={})",
        metrics.mean_precision_at_k,
        metrics.mean_recall_at_k,
        metrics.mean_mrr,
        metrics.negative_pass_rate,
        metrics.num_cases,
        metrics.num_scored,
        metrics.num_negative,
    );
}
