# Memory Management MCP Server — Implementation Plan

## Concept: The AI Decision Ledger (Episodic Memory Tracker)

Standard AI memory systems focus only on **Semantic Memory** (final facts/rules). The flaw: when the AI summarizes its context to save tokens, it loses the journey. If the AI forgets *why* a specific approach failed, it repeats the mistake when the context window is wiped.

**The Solution:** An **Episodic Memory** system via a relational database. Instead of saving only the final answer, the MCP server forces the AI to log its workflow step-by-step:

1. **The Goal** (Task)
2. **The Attempt** (Approach + Code)
3. **The Outcome** (Accepted / Rejected)
4. **The Reason** (Why it failed or succeeded)

The AI doesn't need to read 5,000 tokens of messy chat history. It queries the ledger and reads a dense ~200-token JSON array of past failures and successes for the current task.

---

## 1. Architecture Stack

| Component             | Choice                                                   | Rationale                                                                  |
|-----------------------|----------------------------------------------------------|----------------------------------------------------------------------------|
| **Language**          | Rust                                                     | Strict schema validation, memory safety, zero-cost abstractions            |
| **Database**          | PostgreSQL + `pgvector`                                  | Relational structure for episodic data, vector search for semantic recall  |
| **ORM/Query Builder** | `sqlx` (compile-time checked SQL)                        | Catches schema drift at build time, not runtime                            |
| **Embeddings**        | `fastembed-rs` (local, default) or Gemini API (optional) | Local-first avoids API costs and latency; Gemini opt-in for higher quality |
| **Protocol SDK**      | `rmcp` (Official Rust MCP SDK)                           | First-class MCP support                                                    |
| **Transport**         | `stdio` (primary), `SSE` (optional for remote)           | stdio for local CLI integration; SSE for dashboard/remote clients          |
| **Migrations**        | `sqlx migrate`                                           | Versioned, reversible migrations checked into source control               |
| **Methodology**       | Test-Driven Development (TDD)                            |                                                                            |

---

## 2. Database Schema

### Schema: `ai_memory`

#### Table: `projects` (Multi-tenancy)

Scopes all memory to a specific project/workspace so multiple codebases can share one server.

| Column       | Type      | Notes                         |
|--------------|-----------|-------------------------------|
| `id`         | UUID PK   |                               |
| `name`       | String    | e.g. "memo", "my-web-app"     |
| `root_path`  | String    | Absolute path to project root |
| `created_at` | Timestamp |                               |

#### Table: `semantic_rules` (Long-term facts)

| Column           | Type                                               | Notes                                            |
|------------------|----------------------------------------------------|--------------------------------------------------|
| `id`             | UUID PK                                            |                                                  |
| `project_id`     | UUID FK -> projects                                |                                                  |
| `category`       | Enum('preference', 'fact', 'constraint', 'lesson') | `lesson` added for auto-extracted rules          |
| `content`        | Text                                               |                                                  |
| `embedding`      | Vector(1536)                                       | pgvector                                         |
| `source_task_id` | UUID FK -> tasks (nullable)                        | Links lesson back to the episode that created it |
| `created_at`     | Timestamp                                          |                                                  |
| `expires_at`     | Timestamp (nullable)                               | Optional TTL for time-bound rules                |

**Indexes:**
- `idx_semantic_project` on `(project_id)`
- `idx_semantic_category` on `(project_id, category)`
- `idx_semantic_embedding` HNSW index on `embedding` using cosine distance

#### Table: `tasks` (Current goals)

| Column           | Type                                                | Notes                                   |
|------------------|-----------------------------------------------------|-----------------------------------------|
| `id`             | UUID PK                                             |                                         |
| `project_id`     | UUID FK -> projects                                 |                                         |
| `description`    | Text                                                |                                         |
| `status`         | Enum('active', 'completed', 'abandoned', 'blocked') | `blocked` added for dependency tracking |
| `parent_task_id` | UUID FK -> tasks (nullable)                         | Supports subtask hierarchies            |
| `created_at`     | Timestamp                                           |                                         |
| `completed_at`   | Timestamp (nullable)                                |                                         |

**Indexes:**
- `idx_tasks_project_status` on `(project_id, status)`

#### Table: `attempts` (The Episodic Ledger)

| Column                | Type                                    | Notes                                    |
|-----------------------|-----------------------------------------|------------------------------------------|
| `id`                  | UUID PK                                 |                                          |
| `task_id`             | UUID FK -> tasks                        |                                          |
| `approach_summary`    | Text                                    |                                          |
| `code_snippet`        | Text (nullable)                         |                                          |
| `outcome`             | Enum('pending', 'accepted', 'rejected') |                                          |
| `reasoning`           | Text                                    | Why it succeeded or failed               |
| `reasoning_embedding` | Vector(1536) (nullable)                 | For cross-task failure similarity search |
| `git_ref`             | String (nullable)                       | Commit hash or stash ref for rollback    |
| `token_cost`          | Integer (nullable)                      | Tokens consumed during this attempt      |
| `created_at`          | Timestamp                               |                                          |
| `resolved_at`         | Timestamp (nullable)                    |                                          |

**Indexes:**
- `idx_attempts_task` on `(task_id)`
- `idx_attempts_outcome` on `(task_id, outcome)`
- `idx_attempts_reasoning_embedding` HNSW on `reasoning_embedding`

#### Table: `context_snapshots` (Context Wipe Bookmarks)

Tracks when context wipes happen so the resume injection knows exactly what to load.

| Column               | Type      | Notes                                |
|----------------------|-----------|--------------------------------------|
| `id`                 | UUID PK   |                                      |
| `task_id`            | UUID FK   |                                      |
| `wiped_at`           | Timestamp |                                      |
| `token_count_before` | Integer   | Tokens in context before wipe        |
| `last_attempt_id`    | UUID FK   | Last attempt visible before the wipe |

---

## 3. The Compaction Strategy

The chat context window is now a temporary UI. The *real* state lives in the `attempts` table.

### Flow

1. **Aggressive Context Wiping:** Because the ledger is structured, wipe the LLM context frequently (every 5-10 messages, or at ~4,000 tokens).
2. **The "Resume" Injection:** After a wipe, the next prompt triggers:
   ```sql
   SELECT approach_summary, outcome, reasoning
   FROM attempts
   WHERE task_id = $current AND outcome = 'rejected'
   ORDER BY created_at DESC;
   ```
   The AI gets a clean JSON list of what *not* to do — bypassing the N^2 token trap.
3. **Task Closeout:** When `log_outcome(status: 'accepted')` is called, the AI extracts the core lesson, saves it to `semantic_rules` (category: `lesson`), and marks the task `completed`.

### Retention Policy

| Data Type                       | Retention                                                                                 |
|---------------------------------|-------------------------------------------------------------------------------------------|
| `semantic_rules`                | Permanent (unless explicitly deleted or expired)                                          |
| `tasks` (completed)             | Archive after 90 days (move to `tasks_archive`)                                           |
| `attempts` (on completed tasks) | Keep 30 days, then prune — the extracted `lesson` in `semantic_rules` preserves the value |
| `context_snapshots`             | Keep 7 days                                                                               |

A background `CRON`-style cleanup job (Rust `tokio::time::interval`) handles pruning.

---

## 4. MCP Tools API

### Long-Term Memory

| Tool            | Parameters                     | Returns   | Description                                   |
|-----------------|--------------------------------|-----------|-----------------------------------------------|
| `remember_rule` | `category`, `content`          | `rule_id` | Stores a semantic rule with embedding         |
| `recall_rules`  | `query`, `limit?`, `category?` | `Rule[]`  | Vector similarity search, optionally filtered |
| `forget_rule`   | `rule_id`                      | `success` | Soft-delete a rule                            |
| `list_rules`    | `category?`                    | `Rule[]`  | List all rules, optionally by category        |

### Episodic Memory (The Ledger)

| Tool                    | Parameters                                       | Returns      | Description                                                        |
|-------------------------|--------------------------------------------------|--------------|--------------------------------------------------------------------|
| `start_task`            | `description`, `parent_task_id?`                 | `task_id`    | Create a new task                                                  |
| `propose_attempt`       | `task_id`, `approach_summary`, `code_snippet?`   | `attempt_id` | Log an approach before executing it                                |
| `log_outcome`           | `attempt_id`, `outcome`, `reasoning`, `git_ref?` | `success`    | Record what happened and why                                       |
| `review_ledger`         | `task_id`, `outcome_filter?`                     | `Attempt[]`  | Query the ledger, optionally filter by outcome                     |
| `find_similar_failures` | `error_description`, `limit?`                    | `Attempt[]`  | Vector search across `reasoning_embedding` for similar past errors |
| `complete_task`         | `task_id`, `lesson?`                             | `success`    | Close out a task, optionally auto-extract lesson                   |

### System / Introspection

| Tool                    | Parameters                     | Returns          | Description                                                               |
|-------------------------|--------------------------------|------------------|---------------------------------------------------------------------------|
| `inspect_memory_schema` | none                           | `SchemaInfo`     | Returns table definitions so AI can orient itself                         |
| `get_active_context`    | none                           | `ContextSummary` | Returns current task, recent attempts, active rules — the "resume packet" |
| `switch_project`        | `name` or `root_path`          | `project_id`     | Switch project scope                                                      |
| `export_memory`         | `format: 'json' \| 'markdown'` | `string`         | Export all memory for backup/portability                                  |

---

## 5. Configuration

Configuration via environment variables with `.env` file support:

```env
# Database
DATABASE_URL=postgres://memo:password@localhost:5432/ai_memory
DATABASE_MAX_CONNECTIONS=10
DATABASE_STATEMENT_TIMEOUT_SECS=5

# Embeddings
EMBEDDING_PROVIDER=local          # "local" (fastembed) or "gemini"
GEMINI_API_KEY=                   # Required only if EMBEDDING_PROVIDER=gemini
EMBEDDING_MODEL=all-MiniLM-L6-v2 # For local; or gemini-embedding-001 for Gemini
EMBEDDING_DIMENSIONS=384          # Match the model (384 for MiniLM, 768 for Gemini)

# Server
MCP_TRANSPORT=stdio               # "stdio" or "sse"
MCP_SSE_PORT=3100                  # Only if transport=sse
LOG_LEVEL=info

# Retention
RETENTION_ATTEMPTS_DAYS=30
RETENTION_SNAPSHOTS_DAYS=7
RETENTION_TASKS_ARCHIVE_DAYS=90

# Project
DEFAULT_PROJECT_NAME=default
```

---

## 6. Test-Driven Development (TDD) Approach

### Phase 1: DB Operations (Unit Tests)

- Spin up a test Postgres container using `testcontainers-rs`.
- Write failing tests for:
  - Inserting a project, task, and attempt.
  - Updating outcome on an attempt.
  - Cascading project scoping (all queries respect `project_id`).
  - Retention/pruning logic.
- Implement `sqlx` queries until tests pass.

### Phase 2: Vector Search (Unit Tests)

- Generate embeddings using `fastembed-rs` (or mock for speed).
- Test that `recall_rules` correctly sorts by cosine distance (`<=>` operator).
- Test `find_similar_failures` returns relevant cross-task matches.
- Test that embedding dimensions match configuration.

### Phase 3: MCP Tool Handlers (Integration Tests)

- Mock incoming JSON-RPC payloads from the AI client.
- Verify `#[tool]` macros correctly deserialize JSON into strict Rust structs.
- Verify the server returns correct `CallToolResult` format.
- Test the "resume injection" flow: wipe -> `get_active_context` -> verify payload completeness.

### Phase 4: End-to-End (E2E Tests)

- Start the full MCP server over stdio.
- Simulate a multi-turn workflow: `start_task` -> `propose_attempt` -> `log_outcome(rejected)` -> `propose_attempt` -> `log_outcome(accepted)` -> `complete_task`.
- Verify the extracted lesson appears in `semantic_rules`.
- Verify the ledger is queryable after a simulated context wipe.

---

## 7. Production Features

### Security & Sandboxing

- Dedicated Postgres user with `statement_timeout = '5s'` and connection limits.
- Input validation on all tool parameters (max content length, allowed characters).
- No raw SQL execution — all queries are parameterized via `sqlx`.

### Observability & Telemetry

- Log every tool invocation with `tracing` crate (structured JSON logs).
- Track per-task metrics: attempt count, token cost, time-to-resolution.
- Alert threshold: if a task exceeds N rejected attempts, surface a warning to the AI suggesting it ask the human for help.

### Concurrency

- `sqlx` connection pool with configurable max connections.
- Row-level locking on `tasks` table for concurrent session safety.
- Optimistic concurrency on `attempts` (check `outcome = 'pending'` before update).

### Backup & Export

- `export_memory` tool for JSON/Markdown export.
- Optional periodic pg_dump via a configurable schedule.

---

## 8. Advanced Features (Post-MVP)

### 8.1 Vectorized Failure Search

Run `pgvector` embeddings on the `reasoning` column of `attempts`. When a user pastes a new stack trace, the AI instantly searches its ledger for similar past errors across *all* tasks.

### 8.2 "Aha!" Extraction

When a task is marked `completed`, trigger an automated process: the AI reviews the ledger of failed attempts and extracts 1-2 permanent rules (Semantic Memory) from the entire episode, saving them to `semantic_rules` with `category = 'lesson'` and `source_task_id` linked back.

### 8.3 Git Checkpointing

Tie `attempt_id` to a local git stash or commit hash. If an attempt is marked `rejected`, the MCP server can trigger a rollback. The `git_ref` column in `attempts` enables this.

### 8.4 Human-in-the-Loop Dashboard

A lightweight web UI connected to Postgres:
- Browse/edit the ledger.
- Mark attempts as `stale`.
- Inject hints directly into the AI's database without burning chat tokens.
- Visualize task resolution patterns (avg attempts per task, common failure categories).

### 8.5 Cross-Project Learning

When `find_similar_failures` is called, optionally search across *all* projects (not just the current one) to surface patterns the AI learned in other codebases.

---

## 9. Project Structure

```
memo/
├── Cargo.toml
├── .env.example
├── migrations/
│   ├── 001_create_projects.sql
│   ├── 002_create_semantic_rules.sql
│   ├── 003_create_tasks.sql
│   ├── 004_create_attempts.sql
│   └── 005_create_context_snapshots.sql
├── src/
│   ├── main.rs                  # Entry point, MCP server setup
│   ├── config.rs                # Environment/config parsing
│   ├── db/
│   │   ├── mod.rs
│   │   ├── pool.rs              # Connection pool setup
│   │   ├── projects.rs          # Project CRUD
│   │   ├── semantic.rs          # Semantic rules CRUD + vector search
│   │   ├── tasks.rs             # Task CRUD
│   │   ├── attempts.rs          # Attempt CRUD + ledger queries
│   │   └── retention.rs         # Cleanup/archival jobs
│   ├── embeddings/
│   │   ├── mod.rs               # Trait definition
│   │   ├── local.rs             # fastembed-rs provider
│   │   └── gemini.rs            # Gemini API provider
│   ├── tools/
│   │   ├── mod.rs
│   │   ├── memory.rs            # remember_rule, recall_rules, forget_rule, list_rules
│   │   ├── ledger.rs            # start_task, propose_attempt, log_outcome, review_ledger
│   │   ├── search.rs            # find_similar_failures
│   │   └── system.rs            # inspect_memory_schema, get_active_context, export_memory
│   └── server.rs                # MCP transport setup (stdio/sse)
└── tests/
    ├── db_tests.rs
    ├── vector_tests.rs
    ├── tool_tests.rs
    └── e2e_tests.rs
```

---

## 10. Implementation Order

| Phase | Scope                     | Milestone                                                        |
|-------|---------------------------|------------------------------------------------------------------|
| **1** | Project scaffold + DB     | `cargo build` passes, migrations run, DB connected               |
| **2** | Embedding provider        | `fastembed-rs` generates vectors, trait abstraction works        |
| **3** | Core CRUD + tests         | All DB operations tested against real Postgres                   |
| **4** | MCP tool handlers         | Tools callable via stdio, JSON-RPC round-trips pass              |
| **5** | Vector search             | `recall_rules` and `find_similar_failures` return ranked results |
| **6** | Compaction + resume       | `get_active_context` returns correct resume packet               |
| **7** | Retention + cleanup       | Background pruning runs on schedule                              |
| **8** | Configuration + hardening | Env-based config, connection limits, timeouts                    |
| **9** | Advanced features         | Git checkpointing, cross-project search, dashboard               |

## 11. Future Enhancements (Performance & Algorithmic Optimizations) (needs review)

As the database grows from hundreds of ledger entries to thousands, standard I/O and exact vector math will become the primary bottlenecks, increasing latency on MCP tool calls. The following algorithms will be implemented to achieve sub-50ms response times:

### A. HNSW (Hierarchical Navigable Small World) Indexing
* **The Problem:** Standard `pgvector` executes exact K-Nearest Neighbors (KNN), scanning every row. This results in $O(N)$ time complexity, which degrades as memory grows.
* **The Solution:** Implement HNSW, an Approximate Nearest Neighbor (ANN) algorithm that builds a multi-layered graph for vector navigation, reducing search time to $O(\log N)$.
* **Implementation:** Add `CREATE INDEX ON ai_memory USING hnsw (embedding vector_cosine_ops);` to the database migrations.

### B. Hybrid Search with RRF (Reciprocal Rank Fusion)
* **The Problem:** Vector search is excellent at finding concepts but often fails at exact keyword matches (e.g., specific variable names or error codes).
* **The Solution:** Combine Postgres full-text keyword search (BM25) with semantic vector search, merging the results using the RRF algorithm. This prevents the AI from needing multiple `recall` attempts.
* **Algorithm:**
  $$RRF\_Score = \sum \frac{1}{k + rank_i}$$
  *(Where $k$ is a constant, typically 60, and $rank_i$ is the document's rank in its respective result list).*

### C. In-Memory LRU Caching
* **The Problem:** Repeatedly checking the same task ledger or generating embeddings for similar diagnostic questions wastes compute and API calls.
* **The Solution:** Implement a Least Recently Used (LRU) cache using a Rust crate like `moka`. Frequent `task_id` queries will bypass the Postgres database entirely, returning to the LLM in under a millisecond.

### D. Zero-Latency Local Embeddings (ONNX)
* **The Problem:** Relying on external APIs (like Gemini) to embed the AI's query string adds 300ms–800ms of network latency per tool call.
* **The Solution:** Run a lightweight, quantized embedding model (e.g., `all-MiniLM-L6-v2`) natively inside the Rust server using the `ort` (ONNX Runtime) crate. This drops embedding generation time to ~10ms with zero network dependency.

---

## 12. Implementation Status

### Completed

| Phase | What | PR |
|-------|------|----|
| 1 | Project scaffold, DB schema, migrations, connection pool | Initial commits |
| 2 | Embedding provider trait + fastembed local provider (feature-gated) | — |
| 3 | Core CRUD: projects, tasks, attempts, semantic rules, retention | — |
| 4 | MCP tool handlers via `rmcp` `#[tool]` macros on `LoreServer` | — |
| 5 | Vector search: `recall_rules` (cosine), `find_similar_failures` | — |
| 6 | `get_active_context` resume packet, `switch_project`, `export_memory` | — |
| 7 | Retention scheduler: prune old attempts, snapshots, archived tasks | — |
| 8 | Env-based config, connection limits, statement timeouts | — |
| 9 | README with setup, config, and MCP tools reference | PR #7 |
| 10 | Test suite (unit + integration with testcontainers) + GitHub Actions CI | PR #8 |
| 11 | Replace OpenAI embeddings with Google Gemini API | PR #9 |

### Current State

- **Server runs** on stdio transport, connects to PostgreSQL + pgvector
- **All 15 MCP tools** implemented and callable
- **Embedding providers**: fastembed (local, feature-gated) and Gemini API
- **Test suite**: 11 unit tests + 28 integration tests (DB + server)
- **CI pipeline**: lint, check, test jobs in GitHub Actions
- **Lib+bin crate split** enables integration test imports

### Next Steps

| Priority | Task | Notes |
|----------|------|-------|
| High | HNSW index migration | Add `CREATE INDEX ... USING hnsw` for semantic_rules and attempts embeddings |
| High | Input validation | Max content length, category enum validation at tool boundary |
| Medium | Hybrid search (RRF) | Combine full-text BM25 + vector search for `recall_rules` |
| Medium | LRU caching | `moka` crate for hot-path queries (task ledger, recent rules) |
| Medium | SSE transport | Enable remote MCP connections |
| Low | Git checkpointing | Tie `attempt_id` to git stash/commit for rollback |
| Low | Cross-project search | `find_similar_failures` across all projects |
| Low | Web dashboard | Lightweight UI to browse/edit the ledger |
