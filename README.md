# Lore — AI Decision Ledger & Code Graph (MCP Server)

Persistent **episodic memory + codebase graph** for AI coding assistants, exposed over the [Model Context Protocol](https://modelcontextprotocol.io/). Lore keeps what the AI tried, why it failed, why it worked, and how the codebase is wired together — all in a structured Postgres store with vector search.

Instead of re-loading thousands of tokens of chat history on every context wipe, the AI queries a dense, typed ledger and a per-file context packet.

---

## Highlights

- **Episodic ledger** — tasks, attempts, outcomes, reasoning; subtask hierarchies; cross-task links (`blocks`, `related_to`, `caused_by`, `duplicate_of`).
- **Long-term rules** — preferences / facts / constraints / lessons with pgvector similarity, temporal validity (`valid_from`/`valid_until`), contradiction + near-duplicate detection, auto-consolidation of accepted attempts into lessons.
- **Progressive disclosure search** — `recall_rules(compact=true)` returns `{id, category, preview, score, tags, hit_count, last_used_at}` (~50 tokens/hit); `get_rule(id)` fetches full content on demand. ~10× token cut on retrieval.
- **Secret scrubber** — regex redaction of AWS keys, API tokens, PEM blocks, connection strings, JWTs, Bearer tokens, GitHub PATs on every store path. Toggle via `LORE_SCRUB_SECRETS`.
- **Codebase indexing** — tree-sitter AST chunking (Rust, TS/TSX, JS, Python, Go, Java), incremental re-index via SHA-256 fingerprints, pgvector + FTS hybrid search with MMR diversity.
- **Call graph + communities** — `calls` / `imports` / `references` edges, caller/callee lookup, BFS shortest path, Louvain community detection, cross-community change detection.
- **Rule ↔ code links** — accepted attempts auto-link rules to the code chunks they apply to; `get_rules_for_file` surfaces them on pre-read.
- **`get_file_context`** — one call returns code structure + linked rules + callers/callees + community info for a file. Pre-read injection.
- **Handoffs** — `generate_handoff` produces a dense briefing before context exhaustion; `get_next_steps` ingests it in the next session.
- **Priority-scored next steps** — pending attempts and active tasks ranked by P1–P4 + age.
- **Optional webhooks + dashboard** — notify on task completion / rejection threshold; web UI on configurable port.

---

## Why persistent memory?

Lore is not independently benchmarked yet, so the numbers below come from published research on closely-related systems (episodic memory, reflection loops, temporal KGs, retrieval-augmented agents). They show the *direction* and *magnitude* of gains reported for the primitives Lore implements — not a guarantee of identical results in your setup.

| Stat | From | Relevance to Lore |
|------|------|-------------------|
| **+26 pp task accuracy** — 93.4 vs ~67.4 on LongMemEval with structured memory vs full-context baseline | [Mem0 production algorithm](https://mem0.ai/research) | Mirrors Lore's rules + episodic ledger vs re-loading chat history |
| **3–4× token reduction** per retrieval call — <7K tokens with memory vs 25K+ full-context | [Mem0 production algorithm](https://mem0.ai/research) | `recall_rules(compact=true)` + `get_rule` follow the same progressive disclosure pattern |
| **30% accuracy drop without memory** on sustained multi-session interaction (LongMemEval, 500 questions) | [Si et al. 2024 — LongMemEval](https://arxiv.org/abs/2410.10813) | Quantifies the cost of losing context on every wipe — what Lore's ledger prevents |
| **+11 pp pass@1 on HumanEval** (91% vs 80%) with verbal reflection over failed attempts | [Shinn et al. 2023 — Reflexion](https://arxiv.org/abs/2303.11366) | Structurally similar (within-session) to Lore's `log_outcome('rejected', reasoning)` + auto-consolidation into lessons |
| **+18.5 pp temporal reasoning** on LongMemEval with a temporal knowledge graph | [Rasmussen et al. 2025 — Zep Graphiti](https://arxiv.org/abs/2501.13956) | Supports Lore's temporal rule validity (`valid_from`/`valid_until`) and typed edges |
| **~90% response latency reduction** vs the paper's own RAG baseline (no absolute numbers published) | [Rasmussen et al. 2025 — Zep Graphiti](https://arxiv.org/abs/2501.13956) | Directional — structured memory avoids re-processing long histories; magnitude will depend on your setup |
| **0.40 → 0.66 multi-hop accuracy** on FRAMES with retrieval-augmented vs no-retrieval baseline | [FRAMES 2024](https://arxiv.org/abs/2409.12941) | Directionally analogous — cross-task links (`blocks`, `caused_by`) + community-aware search target the same multi-hop synthesis |

**Caveats you should read.** Mem0 and Zep are self-reported on benchmarks they selected. Reflexion is a prompting technique, not a persistent store — the gain is within-session. LongMemEval and FRAMES are third-party benchmarks but measure chat/RAG, not coding agents. Treat these as lower bounds on "structured memory helps" rather than predictions for Lore specifically. A project-local eval harness is on the roadmap.

---

## Quick Start

```bash
cp .env.example .env
cargo build --release
./target/release/lore
```

Requires Rust 1.75+, Postgres 15+ with [pgvector](https://github.com/pgvector/pgvector).

### Docker Postgres

```bash
docker run -d --name lore-db \
  -e POSTGRES_USER=lore \
  -e POSTGRES_PASSWORD=password \
  -e POSTGRES_DB=ai_memory \
  -p 5432:5432 \
  pgvector/pgvector:pg16
```

### Without local ONNX embeddings

Building with `--no-default-features` skips the `ort` runtime (smaller binary, faster build) but disables every feature that relies on vector search — rule recall, failure search, codebase indexing, and hybrid/MMR code search will all return an error at runtime. Build with default features unless you're certain you don't need them.

---

## MCP Client Config

Claude Code — drop in `.mcp.json` at project root:

```json
{
  "mcpServers": {
    "lore": {
      "command": "/path/to/lore",
      "env": {
        "DATABASE_URL": "postgres://lore:password@localhost:5432/ai_memory",
        "EMBEDDING_MODEL": "all-MiniLM-L6-v2",
        "EMBEDDING_DIMENSIONS": "384"
      }
    }
  }
}
```

Claude Desktop uses the same shape in `claude_desktop_config.json`.

> The binary loads `.env` from its CWD via `dotenvy`, but MCP clients launch it from elsewhere — **always pass required env vars explicitly** in the MCP config.

### SSE transport (remote)

```bash
MCP_TRANSPORT=sse MCP_SSE_PORT=3100 ./target/release/lore
```

Clients connect at `http://host:3100/sse`. Config for SSE-capable clients:

```json
{ "mcpServers": { "lore": { "url": "http://localhost:3100/sse" } } }
```

---

## MCP Tools

### Long-term rules

| Tool                 | Description                                                                 |
|----------------------|-----------------------------------------------------------------------------|
| `remember_rule`      | Store rule with embedding; warns on near-duplicates (cosine ≥ 0.95) and contradictions. Runs scrubber on content. |
| `recall_rules`       | Hybrid vector + keyword search; `compact=true` returns preview + score + hit stats (~10× cheaper). |
| `get_rule`           | Fetch single rule by ID with full content; increments `hit_count`, sets `last_used_at`. |
| `forget_rule`        | Delete, or supersede (sets `valid_until`) to preserve history.              |
| `list_rules`         | List rules, optional category / tag filter.                                 |
| `update_rule`        | Update category, content, or tags of an existing rule.                      |
| `get_duplicate_rules`| Find near-duplicate pairs (cosine ≥ 0.88) for manual cleanup.               |

### Episodic ledger

| Tool              | Description                                                         |
|-------------------|---------------------------------------------------------------------|
| `start_task`      | Create a task; supports subtasks, priority (P1–P4), task_type.      |
| `propose_attempt` | Log an approach before executing (captures git HEAD).               |
| `log_outcome`     | Record outcome: `pending` / `accepted` / `rejected` / `unknown`, with reasoning + code. Never auto-accepts. |
| `review_ledger`   | List attempts for a task; filter by outcome.                        |
| `link_tasks`      | Link two tasks via `blocks` / `related_to` / `caused_by` / `duplicate_of`. |
| `update_task`     | Update priority, task_type, or description.                         |
| `complete_task`   | Close task; extract lesson; auto-detects resolved attempt.          |
| `abandon_task`    | Abandon with reason; optional save-as-lesson.                       |
| `list_tasks`      | List for current project; filter by status.                         |
| `list_subtasks`   | List children of a parent task.                                     |
| `get_task_stats`  | Attempt counts, rejection rate, time-to-resolution.                 |

### Codebase graph

| Tool                              | Description                                                          |
|-----------------------------------|----------------------------------------------------------------------|
| `index_codebase`                  | Scan files (respects `.gitignore`), tree-sitter AST-chunk, embed via ONNX, store in pgvector. Incremental (SHA-256). |
| `search_codebase`                 | Hybrid vector + keyword search with MMR diversity re-ranking; optional file glob.|
| `get_index_status`                | File count, chunk count, last indexed time, summary coverage.        |
| `list_chunks_needing_summary`     | Return chunks lacking a 1-sentence summary plus a client prompt — the client LLM writes the summaries. |
| `submit_chunk_summaries`          | Persist client-generated summaries via parallel `ids` / `summaries` arrays. |
| `get_rules_for_file`              | Rules auto-linked to a file's code chunks via accepted outcomes.     |
| `get_file_context`                | **All-in-one**: code structure + linked rules + callers/callees + community for a file. |
| `find_callers` / `find_callees`   | Inbound / outbound edges from the call graph for a given entity.     |
| `shortest_code_path`              | BFS between two entities through call/import edges.                  |
| `detect_communities`              | Louvain clustering on codebase edges; assigns `community_id`.        |
| `get_community_members`           | Chunks belonging to a community (name, file, line range).            |
| `detect_cross_community_changes`  | For a set of changed files, flag affected communities (cross-module review signal). |

### Knowledge graph

| Tool              | Description                                                          |
|-------------------|----------------------------------------------------------------------|
| `add_edge`        | Generic edge between two entities (`depends_on`, `uses`, custom).    |
| `query_neighbors` | Multi-hop traversal (depth ≤ 5), optional edge-type filter.          |
| `find_path`       | Shortest path between entities via BFS.                              |

### Search & session

| Tool                    | Description                                                            |
|-------------------------|------------------------------------------------------------------------|
| `find_similar_failures` | Semantic search over past rejection reasoning; supports cross-project. |
| `get_active_context`    | Resume packet: current project, active task, recent attempts, wipe count. |
| `log_context_wipe`      | Mark a context exhaustion event.                                       |
| `generate_handoff`      | Dense handoff briefing for next session; auto-logs wipe.               |
| `generate_session_summary` | Structured session briefing (tasks + attempts + lessons) plus a client prompt + JSON schema — the client LLM synthesizes the summary; no server-side LLM call. |
| `get_next_steps`        | Cold-start briefing: ranked pending work + recent lessons (L0 minimal / L1 full). |
| `get_protocol`          | Re-read mandatory episodic memory protocol rules.                      |
| `switch_project`        | Switch project scope (creates if missing).                             |
| `export_memory`         | Dump rules + tasks + attempts as JSON or markdown.                     |

### MCP Resources

| URI                     | Description                                           |
|-------------------------|-------------------------------------------------------|
| `lore://protocol`       | Episodic memory protocol (text/plain).                |
| `lore://active-context` | Active project, tasks, wipe count (JSON).             |

---

## Configuration

All via environment variables (see `.env.example`):

| Variable                             | Default                                             | Description                                                    |
|--------------------------------------|-----------------------------------------------------|----------------------------------------------------------------|
| `DATABASE_URL`                       | `postgres://lore:password@localhost:5432/ai_memory` | Postgres connection string                                     |
| `DATABASE_MAX_CONNECTIONS`           | `10`                                                | Pool size                                                      |
| `DATABASE_STATEMENT_TIMEOUT_SECS`    | `5`                                                 | Per-query timeout                                              |
| `EMBEDDING_MODEL`                    | `all-MiniLM-L6-v2`                                  | Local ONNX embedding model name                                |
| `EMBEDDING_DIMENSIONS`               | `384`                                               | Must match model and DB schema                                 |
| `MCP_TRANSPORT`                      | `stdio`                                             | `stdio` or `sse`                                               |
| `MCP_SSE_PORT`                       | `3100`                                              | SSE port                                                       |
| `LOG_LEVEL`                          | `warn`                                              | Tracing filter (adds `ort=warn` automatically)                 |
| `RETENTION_ATTEMPTS_DAYS`            | `30`                                                | Auto-delete attempts older than N days                         |
| `RETENTION_SNAPSHOTS_DAYS`           | `7`                                                 | Auto-delete context snapshots older than N days                |
| `RETENTION_TASKS_ARCHIVE_DAYS`       | `90`                                                | Auto-delete completed tasks older than N days                  |
| `RETENTION_UNKNOWN_DAYS`             | `7`                                                 | Auto-delete unknown/stale attempts                             |
| `RETENTION_PENDING_ESCALATION_HOURS` | `72`                                                | Escalate pending attempts to `unknown` after N hours           |
| `DECAY_AFTER_DAYS`                   | `14`                                                | Consolidate accepted attempts into lessons after N days        |
| `DECAY_MIN_ACCEPTED`                 | `2`                                                 | Min accepted attempts before consolidation                     |
| `DEFAULT_PROJECT_NAME`               | `default`                                           | Fallback name for `switch_project`                             |
| `DISABLED_TOOLS`                     | —                                                   | Comma-separated tools to hide/reject                           |
| `CAPTURE_GIT_REF`                    | `false`                                             | Capture HEAD on `propose_attempt`                              |
| `LORE_PROACTIVE_CONTEXT`             | `false`                                             | Inject context into responses proactively                      |
| `LORE_SCRUB_SECRETS`                 | `true`                                              | Redact secrets on every store path                             |
| `DASHBOARD_ENABLED`                  | `false`                                             | Enable web dashboard                                           |
| `DASHBOARD_PORT`                     | `3101`                                              | Dashboard HTTP port                                            |
| `WEBHOOK_URL`                        | —                                                   | HTTP endpoint for event notifications                          |
| `WEBHOOK_EVENTS`                     | `task_completed,task_abandoned,rejection_threshold` | Event types to fire                                            |
| `WEBHOOK_REJECTION_THRESHOLD`        | `3`                                                 | Fire webhook after N rejections on same task                   |

> Changing `EMBEDDING_DIMENSIONS` requires a migration to alter the pgvector column size.

---

## Database Schema

Tables in the `ai_memory` schema:

- `projects` — multi-tenancy, scopes all data
- `semantic_rules` — rules with embeddings, temporal validity, hit counters
- `tasks` — goals with status, priority, subtask hierarchies
- `attempts` — episodic ledger: approach, outcome, reasoning, code, git ref
- `task_links` — cross-task edges (`blocks`, `related_to`, `caused_by`, `duplicate_of`)
- `context_snapshots` — context wipe bookmarks
- `code_chunks` — AST-level code chunks with embeddings and community IDs
- `codebase_edges` — `calls` / `imports` / `references` between chunks
- `knowledge_edges` — generic typed edges between arbitrary entities
- `rule_chunk_links` — rule ↔ code chunk links (auto-created on accepted outcomes)

## Workflow (Protocol)

Lore ships a mandatory operating protocol fetched via `get_protocol` or the `lore://protocol` resource. Gist:

1. `switch_project(name, root_path)` at session start.
2. `get_next_steps()` for cold-start briefing.
3. `start_task(description)` before writing any code.
4. Decompose into subtasks when a task has 3+ steps.
5. `propose_attempt(task_id, approach)` before presenting code.
6. On user failure report → `log_outcome(attempt_id, 'rejected', reasoning, code)` **before** proposing a fix.
7. Only `log_outcome(..., 'accepted', ...)` when the user explicitly confirms. Default to `pending`.
8. On user confirmation → `complete_task(task_id, lesson)`.
9. `review_ledger(task_id)` if lost.
10. `get_active_context()` every ~5 messages.
11. At ~97% context → `generate_handoff()` immediately.
