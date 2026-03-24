# Lore — AI Decision Ledger (MCP Server)

An **Episodic Memory** system for AI assistants via the [Model Context Protocol](https://modelcontextprotocol.io/). Instead of losing context on every conversation reset, Lore persists the AI's decision-making history in a structured relational database — what was tried, what failed, what succeeded, and why.

## Why

Standard AI memory saves only final facts. When context is wiped, the AI forgets *why* a specific approach failed and repeats the same mistakes. Lore solves this by logging the full episodic workflow:

1. **The Goal** — a task to accomplish
2. **The Attempt** — an approach + optional code
3. **The Outcome** — accepted or rejected
4. **The Reason** — why it succeeded or failed

After a context wipe, the AI queries the ledger and gets a dense summary of past failures and successes — no need to re-read thousands of tokens of chat history.

## Architecture

| Component  | Choice                          |
|------------|---------------------------------|
| Language   | Rust                            |
| Database   | PostgreSQL + pgvector           |
| ORM        | sqlx (compile-time checked SQL) |
| Embeddings | fastembed (local) or Gemini API |
| Protocol   | rmcp (Rust MCP SDK)             |
| Transport  | stdio                           |

## Setup

### Prerequisites

- Rust 1.75+ (for native async fn in traits)
- PostgreSQL 15+ with [pgvector](https://github.com/pgvector/pgvector) extension
- Docker (optional, for running Postgres)

### Database

```bash
docker run -d --name lore-db -e POSTGRES_USER=lore -e POSTGRES_PASSWORD=password -e POSTGRES_DB=ai_memory -p 5432:5432 pgvector/pgvector:pg16
```

### Build & Run

```bash
cp .env.example .env  # edit as needed
cargo build --release
./target/release/lore
```

### Without local embeddings

To skip the fastembed dependency (smaller binary, faster build):

```bash
cargo build --release --no-default-features
```

Set `EMBEDDING_PROVIDER=gemini` and `GEMINI_API_KEY` in your `.env`.

## Configuration

All settings via environment variables (see `.env.example`):

| Variable                          | Default                                             | Description                                     |
|-----------------------------------|-----------------------------------------------------|-------------------------------------------------|
| `DATABASE_URL`                    | `postgres://lore:password@localhost:5432/ai_memory` | PostgreSQL connection string                    |
| `DATABASE_MAX_CONNECTIONS`        | `10`                                                | Connection pool size                            |
| `DATABASE_STATEMENT_TIMEOUT_SECS` | `5`                                                 | Per-query timeout                               |
| `EMBEDDING_PROVIDER`              | `local`                                             | `local` (fastembed) or `gemini`                 |
| `GEMINI_API_KEY`                  | —                                                   | Required when provider is `gemini`              |
| `EMBEDDING_MODEL`                 | `all-MiniLM-L6-v2`                                  | Embedding model name                            |
| `EMBEDDING_DIMENSIONS`            | `384`                                               | Must match model and DB schema                  |
| `MCP_TRANSPORT`                   | `stdio`                                             | `stdio` or `sse`                                |
| `MCP_SSE_PORT`                    | `3100`                                              | SSE port (only when `MCP_TRANSPORT=sse`)        |
| `LOG_LEVEL`                       | `info`                                              | Tracing filter level                            |
| `RETENTION_ATTEMPTS_DAYS`         | `30`                                                | Auto-delete attempts older than N days          |
| `RETENTION_SNAPSHOTS_DAYS`        | `7`                                                 | Auto-delete context snapshots older than N days |
| `RETENTION_TASKS_ARCHIVE_DAYS`    | `90`                                                | Auto-delete completed tasks older than N days   |
| `DEFAULT_PROJECT_NAME`            | `default`                                           | Fallback project name for `switch_project`      |

> **Note:** Changing `EMBEDDING_DIMENSIONS` requires a database migration to alter the vector column size.

## MCP Tools

### Long-Term Memory

| Tool            | Description                                                                 |
|-----------------|-----------------------------------------------------------------------------|
| `remember_rule` | Store a semantic rule (preference, fact, constraint, lesson) with embedding |
| `recall_rules`  | Vector similarity search for relevant rules                                 |
| `forget_rule`   | Delete a rule                                                               |
| `list_rules`    | List all rules, optionally filtered by category                             |

### Episodic Memory (Decision Ledger)

| Tool              | Description                                                      |
|-------------------|------------------------------------------------------------------|
| `start_task`      | Create a new task (supports subtask hierarchies)                 |
| `propose_attempt` | Log an approach before executing it                              |
| `log_outcome`     | Record what happened (accepted/rejected) and why                 |
| `review_ledger`   | Query the ledger for a task, optionally filtered by outcome      |
| `complete_task`   | Close a task, optionally extracting a lesson to long-term memory |

### Search

| Tool                    | Description                                     |
|-------------------------|-------------------------------------------------|
| `find_similar_failures` | Semantic search across past rejection reasoning |

### System

| Tool                 | Description                                                |
|----------------------|------------------------------------------------------------|
| `get_active_context` | Resume packet: current task, recent attempts, active rules |
| `switch_project`     | Switch project scope (creates if not exists)               |
| `export_memory`      | Export all memory as JSON                                  |

## MCP Client Configuration

Add to your MCP client config (e.g. Claude Desktop `claude_desktop_config.json`):

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

## Database Schema

Five tables in the `ai_memory` schema:

- **projects** — multi-tenancy, scopes all data to a project/workspace
- **semantic_rules** — long-term facts/preferences/constraints/lessons with vector embeddings
- **tasks** — current goals with status tracking and subtask hierarchies
- **attempts** — the episodic ledger: approach, outcome, reasoning, optional code/git ref
- **context_snapshots** — bookmarks for context wipe events

## Project Structure

```
src/
├── main.rs              # Entry point, server lifecycle
├── config.rs            # Env-based configuration
├── server.rs            # LoreServer + all MCP tool handlers
├── db/
│   ├── pool.rs          # Connection pool + migrations
│   ├── projects.rs      # Project CRUD
│   ├── semantic.rs      # Semantic rules CRUD + vector search
│   ├── tasks.rs         # Task CRUD + status management
│   ├── attempts.rs      # Attempt CRUD + outcome logging + failure search
│   └── retention.rs     # Background cleanup + retention scheduler
├── embeddings/
│   ├── mod.rs           # EmbeddingProvider trait + enum dispatch
│   ├── local.rs         # fastembed provider (feature-gated)
│   └── gemini.rs        # Gemini API provider
└── tools/               # Domain module stubs (tool impls live on LoreServer in server.rs)
migrations/              # sqlx SQL migrations (auto-run on startup)
```

## License

MIT
