# Lore Evaluation Harness

Deterministic replay harness that measures retrieval precision, recall, and task-completion accuracy against canned case files. Every PR that touches retrieval (`src/embeddings/**`, `src/db/semantic.rs`, `src/tools/search.rs`) should report a delta vs `baselines.json`.

This directory lands across four tasks — this PR (P1-T1) ships the scaffold only.

## Status

| Task | Ships |
|------|-------|
| P1-T1 | Types, loader trait, `fixtures/mini.json` (10 synthetic cases), unit test |
| P1-T2 | `src/eval/harness.rs` replay runner + `tests/eval_replay.rs` |
| P1-T3 | `src/eval/metrics.rs` (precision@k, recall@k, MRR, task accuracy) |
| P1-T4 | `eval/baselines.json` captured from develop HEAD |
| P1-T5 | Nightly GitHub Actions workflow + PR delta bot |
| Follow-up | Real dataset loaders: LoCoMo, LongMemEval, BEAM |

## Run locally (once P1-T2 lands)

```bash
cargo test --features eval --test eval_replay -- --nocapture
```

## Fixtures

- `fixtures/mini.json` — 10 hand-crafted synthetic cases committed in-tree. Used for CI smoke tests and to let P1-T2 iterate without pulling real datasets. < 5 KB.
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

### ID resolution (for P1-T2)

`expected_hits` and `recall_hit_ids` are **human-readable labels**, not real DB IDs. Inserted rules/attempts get UUIDs at runtime that the fixture author cannot predict. The replay runner resolves labels like this:

1. As each `remember_rule` / `propose_attempt` / `start_task` event fires, record the returned real ID plus the label the fixture author would naturally assign (e.g. first rule in `mini-001` → `rule-go-tabs`). Labels are derived by a deterministic slug of the event's content/approach prefix, scoped to the case.
2. When a `recall_rules` event resolves, map each returned UUID back to its label via that per-case table.
3. Grade by label-set equality (or top-K rank) against `expected_hits`.

An empty `expected_hits` / `recall_hit_ids` (see `mini-011`) asserts **zero hits** — a precision signal, not a skip.

## Baseline update policy

`baselines.json` is the source of truth. Only update it in a review-gated PR that explicitly calls out why the delta is intended (model swap, schema change, scoring tweak). Accidental baseline drift from unrelated PRs must be reverted.
