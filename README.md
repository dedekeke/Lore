# Lore — AI Decision Ledger (MCP Server)

An **Episodic Memory** system for AI assistants via the [Model Context Protocol](https://modelcontextprotocol.io/). Instead of losing context on every conversation reset, Lore persists the AI's decision-making history in a structured relational database — what was tried, what failed, what succeeded, and why.

## Why

Standard AI memory saves only final facts. When context is wiped, the AI forgets *why* a specific approach failed and repeats the same mistakes. Lore solves this by logging the full episodic workflow:

1. **The Goal** — a task to accomplish
2. **The Attempt** — an approach + optional code
3. **The Outcome** — accepted or rejected
4. **The Reason** — why it succeeded or failed

After a context wipe, the AI queries the ledger and gets a dense summary of past failures and successes — no need to re-read thousands of tokens of chat history.

## Setup

### Prerequisites

- Rust 1.75+ (for native async fn in traits)
- PostgreSQL 15+ with [pgvector](https://github.com/pgvector/pgvector) extension
- Docker (optional, for running Postgres)

### Database

**Option A: Local PostgreSQL** 

```bash
# Create user and database
psql postgres -c "CREATE USER lore WITH PASSWORD 'password';"
psql postgres -c "CREATE DATABASE ai_memory OWNER lore;"
psql ai_memory -c "CREATE EXTENSION IF NOT EXISTS vector;"
```

**Option B: Docker**

```bash
docker run -d --name lore-db \
  -e POSTGRES_USER=lore \
  -e POSTGRES_PASSWORD=password \
  -e POSTGRES_DB=ai_memory \
  -p 5432:5432 \
  pgvector/pgvector:pg16
```

> Both options use the same default `DATABASE_URL`. Ensure pgvector is installed for local PostgreSQL — see [pgvector install guide](https://github.com/pgvector/pgvector#installation).

### Build & Run

```bash
cp .env.example .env
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
| `RETENTION_UNKNOWN_DAYS`          | `7`                                                 | Auto-delete unknown/stale attempts after N days |
| `RETENTION_PENDING_ESCALATION_HOURS` | `72`                                             | Escalate pending attempts to unknown after N hours |
| `DECAY_AFTER_DAYS`                | `14`                                                | Consolidate accepted attempts into lessons after N days |
| `DECAY_MIN_ACCEPTED`              | `2`                                                 | Min accepted attempts before consolidation      |
| `DEFAULT_PROJECT_NAME`            | `default`                                           | Fallback project name for `switch_project`      |

> **Note:** Changing `EMBEDDING_DIMENSIONS` requires a database migration to alter the vector column size.

## MCP Tools

### Long-Term Memory

| Tool            | Description                                                                 |
|-----------------|-----------------------------------------------------------------------------|
| `remember_rule` | Store a rule with embedding (warns on cosine > 0.95 duplicates)            |
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
| `abandon_task`    | Abandon a task with reason, optionally saving as lesson          |
| `list_tasks`      | List tasks for current project, optionally filtered by status    |

### Search

| Tool                    | Description                                     |
|-------------------------|-------------------------------------------------|
| `find_similar_failures` | Semantic search across past rejection reasoning |

### System

| Tool                 | Description                                                |
|----------------------|------------------------------------------------------------|
| `get_active_context` | Resume packet: current task, attempts, wipe count          |
| `log_context_wipe`   | Record a context window exhaustion event                   |
| `switch_project`     | Switch project scope (creates if not exists)               |
| `get_task_stats`     | Task analytics: attempt counts, rejection rate, resolution |
| `export_memory`      | Export all memory as JSON or markdown                      |
| `get_next_steps`     | Cold-start briefing: pending work, blocked tasks, lessons  |
| `get_protocol`       | Re-read the mandatory episodic memory protocol             |
| `update_rule`        | Update an existing rule's category and/or content          |

## MCP Client Configuration

Add to your MCP client config. For Claude Code, create `.mcp.json` in your project root:

```json
{
  "mcpServers": {
    "lore": {
      "command": "/path/to/lore",
      "env": {
        "DATABASE_URL": "postgres://lore:password@localhost:5432/ai_memory",
        "EMBEDDING_PROVIDER": "gemini",
        "GEMINI_API_KEY": "your-api-key",
        "EMBEDDING_MODEL": "gemini-embedding-001",
        "EMBEDDING_DIMENSIONS": "384"
      }
    }
  }
}
```

For Claude Desktop, use `claude_desktop_config.json` with the same structure.

> **Important:** The binary loads `.env` from the current working directory via `dotenvy`, but MCP clients may launch it from a different directory. Always pass all required env vars explicitly in the MCP config to avoid falling back to defaults.

### SSE Transport (Remote)

To run Lore as a remote HTTP server instead of stdio:

```bash
MCP_TRANSPORT=sse MCP_SSE_PORT=3100 ./target/release/lore
```

Clients connect via SSE at `http://host:3100/sse` and post messages to `http://host:3100/message?sessionId=<id>`. Multiple clients can connect simultaneously — each SSE session gets its own server instance sharing the same database pool.

For MCP clients that support SSE, configure the server URL instead of a command:

```json
{
  "mcpServers": {
    "lore": {
      "url": "http://localhost:3100/sse"
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