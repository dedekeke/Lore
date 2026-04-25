# Lore Evaluation Harness

Deterministic replay harness that measures retrieval precision, recall, and task-completion accuracy against canned case files. Every PR that touches retrieval (`src/embeddings/**`, `src/db/semantic.rs`, `src/tools/search.rs`, `src/eval/**`) triggers a delta vs `baselines.json` in GitHub Actions and receives a comment summarising the numbers.

This directory landed across six tasks (P1-T1..P1-T6). Real dataset loaders (LoCoMo, LongMemEval, BEAM) are a follow-up.

## Status

| Task | Ships |
|------|-------|
| P1-T1 ✅ | Types, loader trait, `fixtures/mini.json` (12 synthetic cases), unit test |
| P1-T2 ✅ | `src/eval/harness.rs` replay runner + `tests/eval_replay.rs` |
| P1-T3 ✅ | `src/eval/metrics.rs` (precision@k, recall@k, MRR) + smoke assertions |
| P1-T4 ✅ | `src/eval/baseline.rs` + `eval/baselines.json` with regression gate |
| P1-T5 ✅ | `.github/workflows/eval.yml` — nightly replay + PR delta comment bot |
| P1-T6 ✅ | This README — CI surface, artifact schemas, interpretation guide |
| Follow-up | Real dataset loaders: LoCoMo, LongMemEval, BEAM |

## Run locally

```bash
cargo test --features eval --test eval_replay -- --nocapture
```

Requires Docker (testcontainers spins up a fresh pgvector/pg17 per run). ~5 s cold after the image is cached locally.

To reproduce the CI artifact payload locally:

```bash
EVAL_EXPORT_JSON=/tmp/eval-out cargo test --features eval --test eval_replay -- --nocapture
jq . /tmp/eval-out/report.json
```

## CI workflow

`.github/workflows/eval.yml` runs two jobs:

| Job | Trigger | Permissions | Purpose |
|-----|---------|-------------|---------|
| `replay` | `schedule '0 6 * * *'` UTC · `workflow_dispatch` · `pull_request` on filtered paths | default (read) | Replays the mini fixture against a fresh pgvector testcontainer, writes `eval-out/{current,report}.json`, appends a markdown table to the run summary, uploads the artifact. |
| `comment` | PR-only, after `replay` | `pull-requests: write` | Downloads the artifact and upserts a single `<!-- eval-delta-bot -->`-tagged comment on the PR. |

**PR paths filter**: `src/embeddings/**`, `src/db/semantic.rs`, `src/tools/search.rs`, `src/eval/**`, `eval/**`, `tests/eval_replay.rs`, `Cargo.toml`, `Cargo.lock`, the workflow itself.

**Concurrency**: one run per PR ref. Rapid-fire pushes cancel the older run so the comment bot never races a double-post.

**Artifact**: `eval-metrics-<event_name>-<sha>`, 14-day retention. The hard regression gate lives in the `mini_fixture_replays_without_error` integration test; the workflow computes its own `has_regression` verdict from `report.json` only to colour the PR comment header — it never relaxes the gate.

**Where to look**: PR comment for the headline table, the `replay` job's **run summary** for the full table plus any shape-drift details the comment omits.

## Artifact schemas

Both files are emitted only when `EVAL_EXPORT_JSON=<dir>` is set (the CI workflow sets it; local runs are unaffected).

### `current.json`

Fresh aggregate metrics from this replay, keyed by dataset name.

```json
{
  "mini": {
    "k": 10,
    "num_cases": 15,
    "num_recalls": 14,
    "num_scored": 13,
    "num_negative": 1,
    "negative_pass_rate": 0.0,
    "mean_precision_at_k": 0.4231,
    "mean_recall_at_k": 0.6154,
    "mean_mrr": 0.5,
    "num_contexts": 3,
    "num_contexts_scored": 2,
    "num_contexts_negative": 1,
    "mean_context_precision": 0.0,
    "mean_context_recall": 0.0,
    "context_negative_pass_rate": 1.0,
    "per_case": [ ... ]
  }
}
```

### `report.json`

`CompareReport` — output of `compare(&baseline, &current, DEFAULT_TOLERANCE)`. `Serialize` only, not `Deserialize`; this file is a one-way bot input.

```json
{
  "tolerance": 0.02,
  "datasets": [
    {
      "name": "mini",
      "deltas": [
        { "name": "mean_precision_at_k",        "baseline": 0.4231, "current": 0.4231, "delta": 0.0, "regression": false },
        { "name": "mean_recall_at_k",           "baseline": 0.6154, "current": 0.6154, "delta": 0.0, "regression": false },
        { "name": "mean_mrr",                   "baseline": 0.5,    "current": 0.5,    "delta": 0.0, "regression": false },
        { "name": "negative_pass_rate",         "baseline": 0.0,    "current": 0.0,    "delta": 0.0, "regression": false },
        { "name": "mean_context_precision",     "baseline": 0.0,    "current": 0.0,    "delta": 0.0, "regression": false },
        { "name": "mean_context_recall",        "baseline": 0.0,    "current": 0.0,    "delta": 0.0, "regression": false },
        { "name": "context_negative_pass_rate", "baseline": 1.0,    "current": 1.0,    "delta": 0.0, "regression": false }
      ],
      "shape_mismatches": []
    }
  ],
  "missing_datasets": [],
  "extra_datasets": []
}
```

## Interpreting a regression

The bot labels each row with `ok`, `REGRESSION`, or `non-finite`. Triage:

| Signal | Meaning | Action |
|--------|---------|--------|
| All rows `ok` | Current run matches baseline within `DEFAULT_TOLERANCE` (0.02). | Merge when the rest of the review clears. |
| `REGRESSION` on a metric | `delta < -tolerance` (strictly). Retrieval quality dropped. | Check whether *your* diff changed retrieval behaviour. If yes, fix the code. If no — embedding provider, model, or fixture changed — regenerate the baseline in a separate review-gated PR (see below). |
| `non-finite` delta | Upstream `aggregate()` produced NaN or inf. This is a **bug**, not a regression. | Open an issue, trace to the metric source, fix before merging anything that depends on the eval gate. |
| `shape_mismatches` non-empty | `k`, `num_cases`, `num_recalls`, `num_scored`, or `num_negative` drifted. | Always a hard fail. Either the fixture changed (regen baseline) or scoring semantics changed (review both the code and the baseline together). |
| `missing_datasets` / `extra_datasets` non-empty | Dataset keys drifted between baseline and current. | Always a hard fail. Add the dataset to `eval/baselines.json` or remove the stale entry. |

### Boundary policy

A drop of `-tolerance` exactly is **not** a regression — only strictly-greater magnitudes flag. This keeps floating-point rounding from triggering false alarms on clean reruns.

### When to regenerate the baseline

Two regen shapes, two policies:

**Drift regen** (dedicated PR, review-gated). Existing rows' metrics moved because code, model, or semantics changed. The diff must be read row-by-row — reviewer has to decide if each delta is expected.

- Embedding provider swap (e.g. Fake → LocalEmbeddingProvider / Arctic-Embed).
- Scoring semantics change (e.g. TREC precision convention update).
- Retrieval pipeline edit (ranker, filters, thresholds).

**Fixture-add regen** (same PR as the fixture). New rows added to `mini.json`; existing rows' metrics are byte-identical. The diff is purely additive — the signal is "new rows score what we expect", reviewable in the same PR.

- Adding new cases with new event kinds or new coverage.
- Reconciling baseline shape fields (`num_cases`, `num_recalls`) after fixture growth.

If the diff touches both — existing rows moved AND new rows added — split the PR. Land the drift regen first so reviewers can isolate each delta.

The `baselines-additive-guard` job in `.github/workflows/eval.yml` enforces the fixture-add contract automatically: any PR that touches `eval/baselines.json` must keep `version`, `metadata`, and every pre-existing `per_case` row byte-identical vs the base. It runs `scripts/eval_baselines_additive_check.sh` and fails CI on drift, pointing you back to this section.

Regenerate with:

```bash
EVAL_UPDATE_BASELINE=1 cargo test --features eval --test eval_replay -- --nocapture
```

Accidental drift from unrelated PRs must be reverted — the baseline is the source of truth, not downstream.

## Embedding provider in the harness

P1-T2 uses `FakeEmbeddingProvider::hashed(384)` — deterministic sha256-seeded vectors that are distinct per text. The older `FakeEmbeddingProvider::new(384)` returns a constant vector, which collapses every rule under the 0.95 cosine dedup threshold; do not use it for multi-rule replays. When real datasets run on CI (follow-up), swap to `LocalEmbeddingProvider` and pin ORT thread count to 1 for determinism — the CI workflow already sets `OMP_NUM_THREADS=1`.

## Fixtures

- `fixtures/mini.json` — 15 hand-crafted synthetic cases committed in-tree. Used for the baseline gate + CI smoke. < 15 KB.
- Real datasets (LoCoMo, LongMemEval, BEAM) are downloaded at first run into `~/.cache/lore/eval/` and sha256-verified via the `fetch_cached` helper. They are **never** committed.

## Contributing a new dataset

1. Add an `impl DatasetLoader` in `src/eval/loader.rs` or a new submodule.
2. Populate `DatasetSource` with `url`, `sha256`, and `license`. Reviews block on missing license/sha256.
3. If the source format differs from `EvalCase` shape, add a converter in the same module — do not mutate `EvalCase`.
4. Wire the loader into `tests/eval_replay.rs` behind a new top-level test function (`<dataset>_fixture_replays_without_error`) so failures are attributable per-dataset.
5. Run locally with `EVAL_UPDATE_BASELINE=1`, eyeball the delta, commit `eval/baselines.json` in the **same** PR that adds the loader so the baseline is never missing.

## Dataset licensing (planned loaders)

| Dataset | License | Source |
|---------|---------|--------|
| LoCoMo  | MIT / permissive | https://github.com/snap-stanford/locomo |
| LongMemEval | Apache-2.0 | https://arxiv.org/abs/2410.10813 |
| BEAM    | Permissive (HF dataset card) | https://huggingface.co/datasets |

Each loader must declare `sha256` and `license` on `DatasetSource` before being merged.

## Case schema

See `src/eval/types.rs`. An `EvalCase` is a list of `EvalEvent`s (replay tape) plus an `ExpectedOutcome` (expected recall hits, optional task completion). The replay runner stands up a fresh `LoreServer`, executes the events against the real MCP surface, and grades the recall output against `expected.recall_hit_ids`.

### ID resolution

`expected_hits` and `recall_hit_ids` are **case-local labels**, not real DB IDs. Inserted rules/tasks/attempts get UUIDs at runtime. The replay runner builds a per-case `label → UUID` map as events fire:

- `StartTask { task_ref }` → `task-{task_ref}` → task UUID
- `ProposeAttempt { task_ref }` → `attempt-{task_ref}-a{N}` (N = 1-based index of attempts for that task) → attempt UUID
- `RememberRule { label, always_inject? }` → `{label}` (must be declared on the event) → rule UUID (absent if the server short-circuits on duplicate detection). `always_inject` defaults to the server's per-category default when omitted.
- `GetActiveContext { expected_procedural_labels }` — calls `get_active_context` and records `procedural.rules[].id` into `CaseRun.contexts`. Used for Phase 3 procedural-memory surfacing checks (`mini-013`/`014`/`015`). Scored as a separate channel (see "Context channel" below) — rank-agnostic precision/recall plus a negative-pass bit for empty-expected cases.

P1-T3 grades by mapping the UUIDs returned by `recall_rules` back to labels via this map and comparing to `expected_hits`.

> **Scratchpad (Phase 3)** is intentionally **not** exercised by the eval harness. `write_scratch`/`read_scratch` are deterministic key/value I/O and not a retrieval-quality signal — they are covered by `tests/db_scratchpad.rs` + `tests/server_integration.rs` instead.

An empty `expected_hits` (see `mini-011`) asserts **zero hits** — a precision signal, not a skip. Cases whose `expected_hits` reference `task-*` or `attempt-*` labels (e.g. `mini-002`, `mini-004`, `mini-007`, `mini-009`, `mini-012`) score zero against the current `recall_rules` implementation because that tool searches `semantic_rules` only; a dedicated `FindSimilarFailures` event type is a planned follow-up.

### Metrics

`src/eval/metrics.rs` computes **precision@k**, **recall@k**, and **MRR** over `CaseRun`s:

- **precision@k** = `|retrieved_top_k ∩ relevant| / min(k, |retrieved|)` (TREC — a short return is not penalised by empty slots).
- **recall@k** = `|retrieved_top_k ∩ relevant| / |relevant|`.
- **MRR** = reciprocal rank of the first relevant hit in top-k (0 if no hit).
- Aggregate means are taken across individual `RecallRun`s, not cases — a case with more queries contributes proportionally.
- Labels that did not resolve to a UUID at replay time (near-duplicate short-circuit, or `task-*`/`attempt-*` references that `recall_rules` cannot return) stay in recall's denominator but can never enter the numerator — they score as misses.
- Negative cases (empty `expected_hits`, e.g. `mini-011`) have **undefined** precision/recall/MRR and are excluded from those means. They report a `negative_pass: bool` instead (`true` iff the server returned nothing), aggregated separately into `DatasetMetrics::negative_pass_rate`.

### Context channel

`GetActiveContext` events are scored on a separate channel from `RecallRules`:

- **precision** = `|returned ∩ expected| / |returned|`. Rank-agnostic — the procedural block is an injected set, not a ranked list, so there is no `@k` cutoff and no MRR.
- **recall** = `|returned ∩ expected| / |expected|`. Same unresolved-label miss rule as `RecallRules`.
- Negative cases (empty `expected_procedural_labels`) report `negative_pass: bool` (`true` iff the server returned no rules) and are excluded from the precision/recall means.
- Aggregates surface as `mean_context_precision`, `mean_context_recall`, `context_negative_pass_rate`, plus shape counts (`num_contexts`, `num_contexts_scored`, `num_contexts_negative`). All four metrics are subject to the same `DEFAULT_TOLERANCE` regression gate.

## Known follow-ups

Tracked separately; not in P1:

- **Named wrapper struct around `current.json`** — currently serialised from a bare `BTreeMap`. Wrap in a versioned struct (`CurrentRun { schema_version, metrics }`) before anyone parses it back.
- **Per-dataset tolerance override** — parse `Baseline::metadata.tolerance_<dataset>` so LoCoMo and friends can run a looser gate than mini.
- **`LocalEmbeddingProvider` on CI** — prerequisite for real-dataset loaders. Needs ORT model cache + thread pinning dialled in.
- **`ci.yml` trigger on `develop`** — pre-existing bug; `ci.yml` only triggers on `[master, develop/claude]`, missing `develop`.
