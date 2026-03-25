# Lore v1 — Technical Documentation

## Overview

Lore is an **Episodic Memory** MCP server for AI assistants. It persists the AI's decision-making history — goals, attempts, outcomes, and reasoning — in a structured relational database, surviving context wipes without token waste.

### Core Concept

Standard AI memory stores final facts (semantic memory). Lore adds **episodic memory**: the journey of trial and error. When context is wiped, the AI queries the ledger for a dense summary of what was tried, what failed, and why — instead of re-reading thousands of tokens of chat history.

---

## Architecture

```
┌─────────────────┐     stdio/JSON-RPC     ┌──────────────────────┐
│   MCP Client    │ ◄──────────────────────►│    Lore MCP Server   │
│ (Claude, etc.)  │                         │        (Rust)        │
└─────────────────┘                         └──────────┬───────────┘
                                                       │
                                            ┌──────────▼───────────┐
                                            │   LRU Cache (moka)   │
                                            └──────────┬───────────┘
                                                       │
                                     ┌─────────────────┼─────────────────┐
                                     │                                   │
                              ┌──────▼──────┐                    ┌───────▼───────┐
                              │  Embedding  │                    │  PostgreSQL   │
                              │  Provider   │                    │  + pgvector   │
                              │ (fastembed  │                    │               │
                              │  or Gemini) │                    │  ai_memory.*  │
                              └─────────────┘                    └───────────────┘
```

| Component | Choice | Why |
|-----------|--------|-----|
| Language | Rust | Type safety, zero-cost abstractions, compile-time SQL checks |
| Database | PostgreSQL 15+ + pgvector | Relational episodic data + vector similarity search |
| ORM | sqlx | Compile-time verified queries, no ORM overhead |
| Embeddings | fastembed-rs (local) or Gemini API | Local-first (no network), Gemini opt-in for quality |
| MCP SDK | rmcp | Official Rust MCP SDK |
| Transport | stdio | Direct CLI integration (SSE planned) |
| Cache | moka | Lock-free concurrent LRU cache |

---

## Database Schema

All tables live in the `ai_memory` schema.

### `projects`

Multi-tenancy — scopes all data to a project/workspace.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID PK | |
| `name` | TEXT UNIQUE | e.g. "lore", "my-web-app" |
| `root_path` | TEXT | Absolute path to project root |
| `created_at` | TIMESTAMPTZ | |

### `semantic_rules`

Long-term facts, preferences, constraints, and auto-extracted lessons.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID PK | |
| `project_id` | UUID FK -> projects | |
| `category` | ENUM('preference','fact','constraint','lesson') | |
| `content` | TEXT | |
| `embedding` | vector(384) | pgvector, cosine distance |
| `content_tsv` | tsvector | Generated, for BM25 full-text search |
| `source_task_id` | UUID FK -> tasks (nullable) | Links lesson to originating episode |
| `created_at` | TIMESTAMPTZ | |
| `expires_at` | TIMESTAMPTZ (nullable) | Optional TTL |

**Indexes:**
- `idx_semantic_project` on `(project_id)`
- `idx_semantic_category` on `(project_id, category)`
- HNSW index on `embedding` (cosine, m=16, ef_construction=64)
- GIN index on `content_tsv`

### `tasks`

Current goals with status tracking and subtask support.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID PK | |
| `project_id` | UUID FK -> projects | |
| `description` | TEXT | |
| `status` | ENUM('active','completed','abandoned','blocked') | |
| `parent_task_id` | UUID FK -> tasks (nullable) | Subtask hierarchy |
| `created_at` | TIMESTAMPTZ | |
| `completed_at` | TIMESTAMPTZ (nullable) | |

### `attempts`

The episodic ledger — each row is one trial in the decision-making process.

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID PK | |
| `task_id` | UUID FK -> tasks | |
| `approach_summary` | TEXT | What was tried |
| `code_snippet` | TEXT (nullable) | Optional code |
| `outcome` | ENUM('pending','accepted','rejected','unknown') | |
| `reasoning` | TEXT | Why it succeeded/failed |
| `reasoning_embedding` | vector(384) (nullable) | For cross-task failure similarity |
| `reasoning_tsv` | tsvector | Generated, for BM25 search |
| `git_ref` | TEXT (nullable) | Commit hash or branch for rollback |
| `token_cost` | INTEGER (nullable) | Tokens consumed |
| `created_at` | TIMESTAMPTZ | |
| `resolved_at` | TIMESTAMPTZ (nullable) | |

**Indexes:**
- `idx_attempts_task` on `(task_id)`
- `idx_attempts_outcome` on `(task_id, outcome)`
- HNSW index on `reasoning_embedding` (cosine, m=16, ef_construction=64)
- GIN index on `reasoning_tsv`

### `context_snapshots`

Bookmarks for context wipe events (schema exists, not yet wired).

| Column | Type | Notes |
|--------|------|-------|
| `id` | UUID PK | |
| `task_id` | UUID FK | |
| `wiped_at` | TIMESTAMPTZ | |
| `token_count_before` | INTEGER | Tokens before wipe |
| `last_attempt_id` | UUID FK | Last visible attempt |

---

## MCP Tools (16 total)

### Long-Term Memory

| Tool | Parameters | Description |
|------|-----------|-------------|
| `remember_rule` | `category`, `content` | Store a semantic rule with embedding |
| `recall_rules` | `query`, `limit?`, `category?` | Hybrid search (vector + BM25 via RRF) |
| `forget_rule` | `rule_id` | Delete a rule |
| `list_rules` | `category?` | List all rules |

### Episodic Memory (Decision Ledger)

| Tool | Parameters | Description |
|------|-----------|-------------|
| `start_task` | `description`, `parent_task_id?` | Create a new task |
| `propose_attempt` | `task_id`, `approach_summary`, `code_snippet?` | Log approach before executing |
| `log_outcome` | `attempt_id`, `outcome`, `reasoning`, `git_ref?` | Record result (pending/accepted/rejected/unknown) |
| `review_ledger` | `task_id`, `outcome_filter?` | Query the ledger |
| `complete_task` | `task_id`, `lesson?` | Close task, optionally extract lesson |

### Search

| Tool | Parameters | Description |
|------|-----------|-------------|
| `find_similar_failures` | `error_description`, `limit?` | Vector search across rejection reasoning |

### System

| Tool | Parameters | Description |
|------|-----------|-------------|
| `get_active_context` | — | Resume packet: project, tasks, attempts, rules |
| `switch_project` | `name?`, `root_path?` | Switch/create project scope |
| `export_memory` | `format` | Export all memory as JSON |
| `get_protocol` | — | Re-read the mandatory protocol rules |

### Protocol Enforcement

Every tool response includes a `_next_step` field guiding the AI to the correct next action. The `get_protocol` tool returns the full operating procedure for mid-conversation re-grounding.

**Tool chain:** `switch_project` -> `start_task` -> `propose_attempt` -> `log_outcome` -> `complete_task`

---

## Search Pipeline

Lore uses a hybrid search strategy combining vector similarity and keyword matching:

1. **LRU Cache** — Frequently accessed embeddings and search results served from `moka` in-memory cache
2. **Embedding** — Query text embedded via fastembed (local, ~10ms) or Gemini API
3. **Dual search** — Runs in parallel:
   - **Vector search** — pgvector HNSW cosine distance (semantic similarity)
   - **BM25 full-text** — PostgreSQL tsvector/tsquery (exact keyword matches)
4. **RRF merge** — Results fused via Reciprocal Rank Fusion: `score = sum(1 / (k + rank_i))` with k=60

This ensures both conceptual matches ("authentication error") and exact matches ("ECONNREFUSED") surface correctly.

---

## Retention Policy

Automated background cleanup runs hourly:

| Data | Retention | Default |
|------|-----------|---------|
| `pending` attempts | Auto-escalated to `unknown` | After 72 hours |
| `unknown` attempts | Deleted | After 7 days |
| All attempts | Deleted | After 30 days |
| Context snapshots | Deleted | After 7 days |
| Completed tasks | Deleted | After 90 days |

All thresholds configurable via environment variables.

---

## Configuration

All settings via environment variables (`.env` file supported via `dotenvy`):

| Variable | Default | Description |
|----------|---------|-------------|
| `DATABASE_URL` | `postgres://lore:password@localhost:5432/ai_memory` | PostgreSQL connection string |
| `DATABASE_MAX_CONNECTIONS` | `10` | Connection pool size |
| `DATABASE_STATEMENT_TIMEOUT_SECS` | `5` | Per-query timeout |
| `EMBEDDING_PROVIDER` | `local` | `local` (fastembed) or `gemini` |
| `GEMINI_API_KEY` | — | Required when provider is `gemini` |
| `EMBEDDING_MODEL` | `all-MiniLM-L6-v2` | Model name |
| `EMBEDDING_DIMENSIONS` | `384` | Must match model and DB schema |
| `MCP_TRANSPORT` | `stdio` | `stdio` or `sse` (sse not yet implemented) |
| `LOG_LEVEL` | `info` | Tracing filter level |
| `RETENTION_ATTEMPTS_DAYS` | `30` | Auto-delete all attempts older than N days |
| `RETENTION_SNAPSHOTS_DAYS` | `7` | Auto-delete context snapshots |
| `RETENTION_TASKS_ARCHIVE_DAYS` | `90` | Auto-delete completed tasks |
| `RETENTION_UNKNOWN_DAYS` | `7` | Auto-delete unknown/stale attempts |
| `RETENTION_PENDING_ESCALATION_HOURS` | `72` | Escalate stale pending to unknown |
| `DEFAULT_PROJECT_NAME` | `default` | Fallback project name |

---

## Setup

### Prerequisites

- Rust 1.75+
- PostgreSQL 15+ with [pgvector](https://github.com/pgvector/pgvector)

### Database Setup

**Local PostgreSQL:**

```bash
psql postgres -c "CREATE USER lore WITH PASSWORD 'password';"
psql postgres -c "CREATE DATABASE ai_memory OWNER lore;"
psql ai_memory -c "CREATE EXTENSION IF NOT EXISTS vector;"
```

**Docker:**

```bash
docker run -d --name lore-db \
  -e POSTGRES_USER=lore \
  -e POSTGRES_PASSWORD=password \
  -e POSTGRES_DB=ai_memory \
  -p 5432:5432 \
  pgvector/pgvector:pg16
```

### Build

```bash
cp .env.example .env  # edit as needed
cargo build --release
```

Without local embeddings (smaller binary):

```bash
cargo build --release --no-default-features
# Set EMBEDDING_PROVIDER=gemini and GEMINI_API_KEY in .env
```

### Run

```bash
./target/release/lore
```

Migrations run automatically on startup via sqlx.

### MCP Client Config

```json
{
  "mcpServers": {
    "lore": {
      "command": "/path/to/lore",
      "env": {
        "DATABASE_URL": "postgres://lore:password@localhost:5432/ai_memory"
      }
    }
  }
}
```

---

## Project Structure

```
src/
├── main.rs              # Entry point, server lifecycle
├── lib.rs               # Crate root (lib+bin split for tests)
├── config.rs            # Env-based configuration
├── cache.rs             # LRU cache (moka)
├── server.rs            # LoreServer + all MCP tool handlers
├── db/
│   ├── mod.rs           # Re-exports
│   ├── pool.rs          # Connection pool + migrations
│   ├── projects.rs      # Project CRUD
│   ├── semantic.rs      # Semantic rules + hybrid search (RRF)
│   ├── tasks.rs         # Task CRUD + status
│   ├── attempts.rs      # Attempt CRUD + outcome logging + failure search
│   └── retention.rs     # Background cleanup scheduler
├── embeddings/
│   ├── mod.rs           # EmbeddingProvider trait + enum dispatch
│   ├── local.rs         # fastembed provider (feature-gated)
│   └── gemini.rs        # Gemini API provider
migrations/              # sqlx migrations (auto-run on startup)
tests/                   # Integration tests (testcontainers)
```

---

## Testing

```bash
# Unit tests (no DB required)
cargo test --lib

# Integration tests (requires Docker for testcontainers)
cargo test --test '*'

# All tests
cargo test
```

Integration tests use `testcontainers-rs` to spin up ephemeral PostgreSQL + pgvector containers. No manual DB setup needed for CI.
