# Lore — Implementation Plan

## Current State (v1)

Server runs on stdio + SSE transports with PostgreSQL + pgvector. 21 MCP tools + 2 MCP resources. Performance pipeline: LRU cache, Gemini/ONNX local embedding, HNSW + BM25 hybrid search (RRF). Cross-project search, multi-agent support, intelligent decay, batch embedding backfill. Web dashboard (axum + htmx). Test suite: 18 unit + 34 integration tests. CI via GitHub Actions.

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
| 14 | Unknown outcome variant + stale pending escalation + retention | Done |
| 15 | Auto-project detection, input validation, `update_rule`, `list_tasks`, `abandon_task`, `get_next_steps` | PR #13 |
| 16 | Markdown export format, `get_task_stats` analytics tool | PR #14 |
| 17 | Context snapshots (`log_context_wipe`), rule deduplication on insert | PR #16 |
| 18 | README env var docs, embedding debug logging | PR #17 |
| 19 | Intelligent decay (consolidate accepted attempts into lessons), batch embedding backfill on startup | PR #18 |
| 20 | SSE transport: remote MCP connections via HTTP+SSE, multi-session support | PR #19 |
| 21 | MCP Resources: expose protocol and active-context as readable resources | PR #19 |
| 22 | Cross-project search: find_similar_failures with cross_project flag | PR #19 |
| 23 | Multi-agent support: agent_id column on attempts, wired into propose_attempt tool | PR #19 |
| 24 | Git checkpointing: auto-capture git HEAD on propose_attempt, store as git_ref | PR #20 |
| 25 | Conversation handoff: generate_handoff tool for seamless session transitions | PR #20 |
| 26 | Local ONNX embedding: replace fastembed with ort + tokenizers, mean pooling, auto-download | PR #23 |
| 27 | Configurable tool visibility: DISABLED_TOOLS env var to hide/reject specific tools | PR #24 |
| 28 | ~~Auto context snapshots: track cumulative response bytes, escalating nudges, auto-snapshot safety net~~ | Reverted (phase 32) — MCP server can't observe full context |
| 29 | ~~Fix context counter reset: only reset on true session boundaries~~ | Reverted (phase 32) — part of removed auto-snapshot system |
| 30 | Webhook notifications: fire HTTP webhooks on task_completed, task_abandoned, rejection_threshold | PR #27 |
| 31 | Web dashboard: axum + minijinja + htmx browser UI for ledger browsing/editing | PR #28 |
| 32 | Remove broken auto-snapshot system, delegate context preservation to AI client via CLAUDE.md protocol | PR #28 |
| 33 | Move code_snippet to log_outcome, add resolved_attempt_id to tasks, subtask protocol | PR #28 |
| 34 | Dashboard UX: content modals, project names, effectiveness analytics (first-try rate, knowledge tokens) | In progress |

---

## Next Steps

### High

| Task | Notes |
|------|-------|
| **~~MCP Resources~~** | ~~Expose `active_context` and `protocol` as subscriptions~~ Done (phase 21) |
| **~~SSE transport~~** | ~~Enable remote MCP connections (HTTP+SSE or Streamable HTTP)~~ Done (phase 20) |

### Medium

| Task | Notes |
|------|-------|
| **~~Context snapshots~~** | ~~Wire into `get_active_context` to track context wipes~~ Done (phase 17) |
| **~~Intelligent decay~~** | ~~Auto-consolidate old accepted attempts into lessons~~ Done (phase 19) |
| **~~Local ONNX embedding~~** | ~~Run `all-MiniLM-L6-v2` via `ort` for ~10ms embeddings~~ Done (phase 26) |
| **~~Rule deduplication~~** | ~~Detect cosine > 0.95 duplicates on insert, warn or merge~~ Done (phase 17) |
| **~~Batch embedding on startup~~** | ~~Re-embed stale rules/attempts missing embeddings~~ Done (phase 19) |

### Low

| Task | Notes |
|------|-------|
| ~~Git checkpointing~~ | ~~Tie `attempt_id` to git stash/commit for rollback~~ Done (phase 24) |
| ~~Cross-project search~~ | ~~`find_similar_failures` across all projects~~ Done (phase 22) |
| ~~Web dashboard~~ | ~~Lightweight UI to browse/edit the ledger~~ Done (phase 31) |
| ~~Multi-agent support~~ | ~~Tag attempts with `agent_id` for multi-agent workflows~~ Done (phase 23) |

---

## Future Features

### Side-Prompt Prioritization

Classify attempts by relevance to active task. Schema additions: `description_embedding` on tasks, `interaction_type` enum + `relevance_score` on attempts. Resume packet filters to `task_aligned` + `clarification` only.

### ~~Conversation Handoff Protocol~~

~~When AI context window is about to be exhausted, auto-export a handoff packet (active task, last N attempts, key lessons) that a fresh session can ingest via `get_next_steps`. Enables seamless multi-session workflows.~~ Done (phase 25)

### ~~Configurable Tool Visibility~~

~~Allow projects to enable/disable specific tools via config (e.g., disable `forget_rule` in production). Reduces tool surface area for simpler use cases.~~ Done (phase 27)

### ~~Webhook Notifications~~

~~Fire HTTP webhooks on events (task completed, attempt rejected N times, blocked task). Enables Slack/Discord integration for team awareness.~~ Done (phase 30)

### Token Savings / Efficiency Analytics (Needs Research)

Measure the actual value Lore provides: how many tokens are saved by having episodic memory vs re-discovering solutions from scratch. Research needed on:
- How to quantify "tokens saved" — e.g., rejected attempts that Lore surfaced via `review_ledger`/`find_similar_failures` preventing the AI from re-trying the same approach
- Before/after comparison model: estimate retry cost without Lore vs actual cost with Lore
- Dashboard visualization of efficiency over time (trend lines, per-project comparison)
- Whether to track this passively (instrument existing tools) or require explicit measurement points

### Follow-up Improvements

| Item | Context |
|------|---------|
| Share `reqwest::Client` in webhooks | Currently builds a new client per `fire()` call — should store in `LoreServerInner` |
| Resolve actual project name in webhook payload | Uses `default_project_name` config instead of current project from DB |
| Atomic file writes for ONNX model download | Write to `.tmp` then rename for crash safety during model download |
| Zero-norm guard in `l2_normalize` | Defensive check for zero-length vectors in local embedding normalization |
| Track `ort` 2.0 stable release | Currently on `2.0.0-rc.12` — move to stable when available |

### Research Needed for Further Improvement

All major planned features are implemented. Next improvements require research into:
- Token savings analytics (see above)
- Side-prompt prioritization (relevance scoring for attempts)
- Better embedding models as ecosystem matures (track `ort` 2.0, newer MiniLM variants)
- Dashboard real-time updates (WebSocket/SSE push instead of page reload)

### Architecture Reference

See `docs/doc-v1.md` for full schema, tools API, and configuration reference.
