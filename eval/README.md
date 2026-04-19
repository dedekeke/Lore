# Lore Evaluation Harness

Deterministic replay harness that measures retrieval precision, recall, and task-completion accuracy against canned case files. Every PR that touches retrieval (`src/embeddings/**`, `src/db/semantic.rs`, `src/tools/search.rs`) should report a delta vs `baselines.json`.

This directory lands across five tasks (P1-T1..P1-T5). P1-T1 and P1-T2 are merged; metrics, baselines, and CI wiring follow.

## Status

| Task | Ships |
|------|-------|
| P1-T1 ✅ | Types, loader trait, `fixtures/mini.json` (12 synthetic cases), unit test |
| P1-T2 ✅ | `src/eval/harness.rs` replay runner + `tests/eval_replay.rs` |
| P1-T3 ✅ | `src/eval/metrics.rs` (precision@k, recall@k, MRR) + smoke assertions |
| P1-T4 | `eval/baselines.json` captured from develop HEAD |
| P1-T5 | Nightly GitHub Actions workflow + PR delta bot |
| Follow-up | Real dataset loaders: LoCoMo, LongMemEval, BEAM |

## Run locally

```bash
cargo test --features eval --test eval_replay -- --nocapture
```

Requires Docker (testcontainers spins up a fresh pgvector/pg17 per run).

## Embedding provider in the harness

P1-T2 uses `FakeEmbeddingProvider::hashed(384)` — deterministic sha256-seeded vectors that are distinct per text. The older `FakeEmbeddingProvider::new(384)` returns a constant vector, which collapses every rule under the 0.95 cosine dedup threshold; do not use it for multi-rule replays. When real datasets (P1-T5) run on CI, swap to `LocalEmbeddingProvider` and pin ORT thread count to 1 for determinism.

## Fixtures

- `fixtures/mini.json` — 12 hand-crafted synthetic cases committed in-tree. Used for CI smoke tests. < 10 KB.
- Real datasets (LoCoMo, LongMemEval, BEAM) are downloaded at first run into `~/.cache/lore/eval/` and sha256-verified via the `fetch_cached` helper. They are **never** committed.

## Dataset licensing (planned loaders)

| Dataset | License | Source |
|---------|---------|--------|
| LoCoMo  | MIT / permissive | https://github.com/snap-stanford/locomo |
| LongMemEval | Apache-2.0 | https://arxiv.org/abs/2410.10813 |
| BEAM    | Permissive (HF dataset card) | https://huggingface.co/datasets |

Each loader must declare `sha256` and `license` on `DatasetSource` before being merged.

## Case schema

See `src/eval/types.rs`. An `EvalCase` is a list of `EvalEvent`s (replay tape) plus an `ExpectedOutcome` (expected recall hits, optional task completion). The replay runner (P1-T2) stands up a fresh `LoreServer`, executes the events against the real MCP surface, and grades the recall output against `expected.recall_hit_ids`.

### ID resolution (implemented in P1-T2)

`expected_hits` and `recall_hit_ids` are **case-local labels**, not real DB IDs. Inserted rules/tasks/attempts get UUIDs at runtime. The replay runner builds a per-case `label → UUID` map as events fire:

- `StartTask { task_ref }` → `task-{task_ref}` → task UUID
- `ProposeAttempt { task_ref }` → `attempt-{task_ref}-a{N}` (N = 1-based index of attempts for that task) → attempt UUID
- `RememberRule { label }` → `{label}` (must be declared on the event) → rule UUID (absent if the server short-circuits on duplicate detection)

P1-T3 grades by mapping the UUIDs returned by `recall_rules` back to labels via this map and comparing to `expected_hits`.

An empty `expected_hits` (see `mini-011`) asserts **zero hits** — a precision signal, not a skip. Cases whose `expected_hits` reference `task-*` or `attempt-*` labels (e.g. `mini-002`, `mini-004`, `mini-007`, `mini-009`, `mini-012`) will score zero against the current `recall_rules` implementation because that tool searches `semantic_rules` only; a dedicated `FindSimilarFailures` event type is a planned follow-up.

### Metrics (P1-T3)

`src/eval/metrics.rs` computes **precision@k**, **recall@k**, and **MRR** over `CaseRun`s. Means are taken across individual `RecallRun`s, so a case with more queries contributes proportionally to the aggregate. Labels that did not resolve to a UUID at replay time (near-duplicate short-circuit, or `task-*`/`attempt-*` references that `recall_rules` cannot return) stay in the recall denominator but can never enter the numerator — they score as misses. Negative cases (empty `expected_hits`) contribute to precision (1.0 when the server returned nothing, 0.0 otherwise) and are excluded from recall/MRR means, which are undefined without gold references.

## Baseline update policy

`baselines.json` is the source of truth. Only update it in a review-gated PR that explicitly calls out why the delta is intended (model swap, schema change, scoring tweak). Accidental baseline drift from unrelated PRs must be reverted.
