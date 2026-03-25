# Lore — Implementation Plan

## Current State (v1)

Server runs on stdio transport with PostgreSQL + pgvector. All 16 MCP tools implemented. Performance pipeline: LRU cache, Gemini/local embedding, HNSW + BM25 hybrid search (RRF). Test suite: 11 unit + 28 integration tests. CI via GitHub Actions.

### Completed Phases

| # | Scope | PR/Status |
|---|-------|-----------|
| 1 | Project scaffold, DB schema, migrations, pool | Initial commits |
| 2 | Embedding provider trait + fastembed (feature-gated) | Done |
| 3 | Core CRUD: projects, tasks, attempts, semantic rules, retention | Done |
| 4 | MCP tool handlers via `rmcp` `#[tool]` macros | Done |
| 5 | Vector search: `recall_rules` (cosine), `find_similar_failures` | Done |
| 6 | `get_active_context` resume packet, `switch_project`, `export_memory` | Done |
| 7 | Retention scheduler: prune old attempts, snapshots, archived tasks | Done |
| 8 | Env-based config, connection limits, statement timeouts | Done |
| 9 | README with setup, config, and MCP tools reference | PR #7 |
| 10 | Test suite + GitHub Actions CI | PR #10, #11 |
| 11 | Replace OpenAI embeddings with Gemini API | PR #9 |
| 12 | HNSW indexes, BM25 hybrid search (RRF), LRU cache (`moka`) | PR #10, #11 |
| 13 | AI protocol enforcement: nudges, `get_protocol` tool | Done |
| 14 | Unknown outcome variant + stale pending escalation + retention | In progress |

---

## Next Steps

### Critical

| Task | Notes |
|------|-------|
| **Auto-project detection** | Infer project from `cwd` on first tool call — skip mandatory `switch_project` |

### High

| Task | Notes |
|------|-------|
| **Input validation** | Max content length (4KB), category enum validation, approach_summary cap |
| **`update_rule` tool** | Edit existing semantic rules (currently delete + re-create) |
| **`list_tasks` tool** | Browse tasks across a project |
| **`abandon_task` tool** | Mark tasks as abandoned with reason |

### Medium

| Task | Notes |
|------|-------|
| **MCP Resources** | Expose `active_context` and `protocol` as subscriptions |
| **Task analytics** | `get_task_stats` — attempt count, rejection rate, time-to-resolution |
| **Context snapshots** | Wire into `get_active_context` to track context wipes |
| **Markdown export** | Add markdown format to `export_memory` |
| **SSE transport** | Enable remote MCP connections |
| **Intelligent decay** | Auto-consolidate old accepted attempts into lessons |
| **Local ONNX embedding** | Run `all-MiniLM-L6-v2` via `ort` for ~10ms embeddings |

### Low

| Task | Notes |
|------|-------|
| Batch embedding on startup | Re-embed stale rules/attempts |
| Rule deduplication | Detect cosine > 0.95 duplicates on insert |
| Git checkpointing | Tie `attempt_id` to git stash/commit for rollback |
| Cross-project search | `find_similar_failures` across all projects |
| Web dashboard | Lightweight UI to browse/edit the ledger |
| Multi-agent support | Tag attempts with `agent_id` |

---

## Future Features

### Side-Prompt Prioritization

Classify attempts by relevance to active task. Schema additions: `description_embedding` on tasks, `interaction_type` enum + `relevance_score` on attempts. Resume packet filters to `task_aligned` + `clarification` only.

### Architecture Reference

See `docs/doc-v1.md` for full schema, tools API, and configuration reference.
