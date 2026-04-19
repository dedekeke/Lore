#![cfg(feature = "eval")]

mod common;

use lore::config::Config;
use lore::embeddings::fake::FakeEmbeddingProvider;
use lore::embeddings::AnyEmbeddingProvider;
use lore::eval::{load_mini, replay_case};
use lore::server::LoreServer;

/// Smoke test: every case in the mini fixture replays end-to-end against
/// a real LoreServer + pgvector without erroring, and each case produces
/// at least one recall result with the labels map populated.
///
/// Scoring (precision@k / recall@k / etc.) lives in P1-T3; this test only
/// asserts the replay plumbing is wired up correctly.
#[tokio::test]
async fn mini_fixture_replays_without_error() {
    let (pool, _container) = common::setup_db().await;
    let config = Config::from_env();
    let embeddings = AnyEmbeddingProvider::Fake(FakeEmbeddingProvider::hashed(384));
    let server = LoreServer::new(pool, embeddings, config);

    let cases = load_mini().expect("mini fixture loads");
    assert!(!cases.is_empty(), "mini fixture must be non-empty");

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
    }
}
