# Lore — Episodic Memory for AI Agents

Lore is an MCP server that gives AI assistants persistent, structured memory. It records every decision, failure, and lesson learned — then retrieves them at the right moment so agents never repeat the same mistakes, even after context resets.

---

## The Problem

AI assistants lose everything when their context window resets. They repeat failed approaches, forget hard-won lessons, and waste time rediscovering what worked before. In long-running projects, this cycle compounds — each session starts from scratch.

## How Lore Solves It

Lore acts as an external episodic memory ledger. Every task, approach, and outcome is logged to PostgreSQL with vector embeddings. When the AI starts a new session, it queries Lore for past failures, active work, and relevant lessons — resuming intelligently instead of blindly.

---

## Architecture

```
┌─────────────────┐     MCP (stdio/SSE)       ┌──────────────┐
│   AI Assistant  │ ◄──────────────────────►  │  Lore Server │
│  (Claude, etc.) │    27 tools, 2 resources  │   (Rust)     │
└─────────────────┘                           └──────┬───────┘
                                                     │
                              ┌──────────────────────┼──────────────────────┐
                              │                      │                      │
                     ┌────────▼────────┐    ┌────────▼────────┐    ┌────────▼────────┐
                     │   PostgreSQL    │    │   Embedding     │    │   Dashboard     │
                     │   + pgvector    │    │   Provider      │    │   (optional)    │
                     │   HNSW indexes  │    │  Local ONNX or  │    │   Axum + HTML   │
                     │   BM25 (GIN)    │    │  Gemini API     │    │   port 3101     │
                     └─────────────────┘    └─────────────────┘    └─────────────────┘
```

**Transport:** Stdio (default, for local clients) or SSE (remote HTTP server for multi-client access).

**Embedding:** Choose between fully offline ONNX inference (`all-MiniLM-L6-v2`, 384 dimensions) or Google Gemini API. Embeddings power vector search across rules, task descriptions, failure reasoning, and code chunks.

**Database:** All state in PostgreSQL with pgvector. HNSW indexes for sub-100ms vector search. GIN indexes for full-text keyword matching. Foreign keys and constraints enforce data integrity.

---

## Core Concepts

### Decision Ledger

Every AI task follows a structured lifecycle:

```
start_task → propose_attempt → log_outcome → complete_task
                  ↑                 │
                  └── retry ────────┘ (if rejected)
```

- **Task** — A goal the AI is working toward. Supports priority (P1-P4), type (Bug, Feature, Security), and subtask hierarchies.
- **Attempt** — A proposed approach before writing code. Captures the strategy, outcome, reasoning, and optional code snippet.
- **Outcome** — `pending`, `accepted`, `rejected`, or `unknown`. Only the user marks accepted — the AI never auto-accepts.
- **Lesson** — Extracted on task completion or auto-generated from consolidated old attempts. Persists as a searchable semantic rule.

### Semantic Rules

Long-term knowledge stored with vector embeddings, organized by category:

| Category | Priority Weight | Decay Rate | Use Case |
|----------|:-:|:-:|---|
| **Constraint** | 1.0 (highest) | None | Hard rules that must always apply |
| **Lesson** | 0.75 | Slow | Insights learned from past tasks |
| **Fact** | 0.50 | Very slow | Domain knowledge and context |
| **Preference** | 0.25 | Fast | Style and workflow preferences |

Rules support **tags** (AND-semantics filtering), **expiry dates**, **custom weights**, and **task-type affinity** for targeted retrieval.

---

## Features

### Hybrid Search (RRF + BM25 + Vector)

Lore doesn't rely on vector similarity alone. Every search fuses five scoring signals:

| Signal | Weight | Method |
|--------|:------:|--------|
| Vector similarity | 35% | HNSW cosine distance via pgvector |
| Keyword relevance | 25% | BM25 via PostgreSQL tsvector + GIN |
| Category priority | 20% | Constraints ranked above preferences |
| Recency | 10% | Exponential decay based on last use |
| Popularity | 10% | Hit count normalized against project max |

Results are fused using **Reciprocal Rank Fusion (RRF)** — a technique that combines ranked lists without requiring score calibration. The final score is multiplied by any custom rule weight.

### Cross-Project Search

Rules default to project-scoped search. Enable `cross_project=true` to search across all projects, with a **2x boost** for current-project results — other projects' lessons surface without drowning out local context.

### Proactive Context Injection

When enabled, `get_active_context()` automatically surfaces:
- **Top 5 relevant rules** based on the active task's description embedding
- **Top 3 similar past failures** from the attempts history

The AI gets context-aware hints without making explicit search calls.

### Tiered Context Loading

Cold-start briefings come in two tiers:

| Tier | Tokens | Contents |
|------|:------:|----------|
| **L0** | ~100 | Counts only: active tasks, blocked tasks, lesson count |
| **L1** | ~2000 | Full: task summaries, action items, recent lessons, attempt stats |

L0 is designed for token-constrained environments. L1 is the default full briefing.

### Task Hierarchy & Auto-Rollup

Tasks can be decomposed into subtasks via `parent_task_id`. When all subtasks of a parent complete (or are abandoned), the parent **automatically rolls up** to completed status — up to 10 levels deep.

### Context Wipe Recovery

When the AI's context window fills up:
1. Call `generate_handoff()` to create a dense markdown briefing
2. Lore saves a `context_snapshot` (task state, token count, last attempt)
3. Next session calls `get_next_steps()` to resume from exactly where it left off

No progress is lost between sessions.

### Failure Search

`find_similar_failures()` embeds an error description and searches past rejection reasoning. The AI sees what went wrong before, why, and what eventually worked — before writing a single line of code.

### Codebase Indexing

Index your project's source code for semantic search:
- **Language-aware chunking** — Splits by functions, structs, classes (Rust, Python, TypeScript, Go, Java, SQL, and more)
- **Incremental** — SHA-256 fingerprinting skips unchanged files
- **Respects .gitignore** — Never indexes build artifacts or dependencies
- **MMR diversity** — Configurable re-ranking prevents redundant results
- **Optional LLM summaries** — Generate one-sentence descriptions per chunk for high-level queries

### Automatic Retention & Lesson Consolidation

A background job runs hourly:

| Policy | Default | Action |
|--------|:-------:|--------|
| Stale pending attempts | 72h | Escalate to `unknown` |
| Unknown attempts | 7 days | Delete |
| Old attempts | 30 days | Delete |
| Old snapshots | 7 days | Delete |
| Completed tasks | 90 days | Archive (delete) |
| Accepted attempts | 14 days, 2+ accepted | **Consolidate into structured lesson** |

Consolidation generates a markdown lesson preserving the full failure narrative:

```markdown
## Task: build auth middleware
### Rejected approaches:
- use JWT in cookies: CSRF risk
### Accepted approach:
- use JWT in Authorization header: stateless, CSRF-safe
```

This lesson is embedded and stored as a searchable `Lesson` rule — the team's decisions survive even after the raw attempts are cleaned up.

### Webhook Integration

Fire HTTP webhooks on key events:
- `task_completed` — When a task finishes
- `task_abandoned` — When a task is dropped
- `rejection_threshold` — When an attempt fails N times (default 3)

Integrate with Slack, Jira, PagerDuty, or any HTTP endpoint for real-time visibility into AI decision-making.

### Web Dashboard

Optional browser UI for managing Lore data:
- Browse projects, tasks, and rules
- View attempt history with outcomes and reasoning
- Batch operations (bulk delete, bulk status update)
- Analytics page with task resolution stats

### Multi-Agent Support

The `agent_id` field on attempts tracks which AI agent proposed each approach. In multi-agent workflows, each agent's decision history is preserved and searchable independently.

---

## MCP Tools (27)

### Long-Term Memory
| Tool | Description |
|------|-------------|
| `remember_rule` | Store a rule with category, tags, and duplicate detection (cosine >= 0.95) |
| `recall_rules` | Hybrid search with category/tag filters and cross-project option |
| `forget_rule` | Delete a rule |
| `list_rules` | List rules with optional category and tag filters |
| `update_rule` | Update rule content, category, or tags (re-embeds on change) |

### Decision Ledger
| Tool | Description |
|------|-------------|
| `start_task` | Create task with description, priority, type, and optional parent |
| `propose_attempt` | Log an approach before execution (auto-captures git ref) |
| `log_outcome` | Record result: pending, accepted, rejected, or unknown |
| `review_ledger` | Read past attempts for a task to avoid repeating failures |
| `update_task` | Update priority, type, or description (re-embeds on change) |
| `complete_task` | Mark done with optional lesson extraction |
| `abandon_task` | Mark abandoned with reason (optionally saved as lesson) |
| `list_tasks` | List tasks filtered by status |
| `list_subtasks` | List children of a parent task |
| `get_task_stats` | Analytics: attempt counts, rejection rate, resolution time |

### Search
| Tool | Description |
|------|-------------|
| `find_similar_failures` | Semantic search on past rejection reasoning |
| `search_codebase` | Hybrid code search with MMR diversity re-ranking |
| `index_codebase` | Incremental, language-aware code indexing |
| `get_index_status` | Index statistics (file count, chunk count, coverage) |

### System
| Tool | Description |
|------|-------------|
| `switch_project` | Set active project (creates if new) |
| `get_active_context` | Current state + proactive rule/failure injection |
| `get_next_steps` | Cold-start briefing (L0 or L1 tier) |
| `log_context_wipe` | Record context window exhaustion |
| `generate_handoff` | Dense session handoff with auto-snapshot |
| `export_memory` | Export all data as JSON or Markdown |
| `get_protocol` | Re-read the operating protocol |
| `generate_summaries` | LLM-generate code chunk descriptions |

---

## Database Schema

Five tables in the `ai_memory` schema:

| Table | Records | Key Indexes |
|-------|---------|-------------|
| `projects` | Multi-tenant project scope | Unique name, unique root_path |
| `semantic_rules` | Long-term knowledge with embeddings | HNSW (embedding), GIN (content_tsv, tags), category, hit_count |
| `tasks` | Goals with status and hierarchy | project+status, parent, priority, type, HNSW (description_embedding) |
| `attempts` | Decision history per task | task+outcome, HNSW (reasoning_embedding), GIN (reasoning_tsv) |
| `context_snapshots` | Context wipe bookmarks | task_id |
| `code_chunks` | Indexed source code | HNSW (embedding), GIN (content_tsv), unique (project, file, line) |

---

## Technology

| Component | Technology |
|-----------|------------|
| Runtime | Rust + Tokio |
| MCP | rmcp |
| Database | PostgreSQL 15+ with pgvector |
| Vector Search | HNSW (m=16, ef=64) |
| Full-Text | PostgreSQL tsvector + GIN |
| Local Embeddings | ONNX Runtime (all-MiniLM-L6-v2) |
| Cloud Embeddings | Google Gemini API |
| HTTP/Dashboard | Axum |
| Caching | Moka (in-memory embedding cache) |
| Git Integration | git2 (commit hash capture) |

---

## Why Lore

| Without Lore | With Lore |
|-------------|-----------|
| AI retries the same failed approach 3 times | AI checks `review_ledger`, skips known failures |
| Session reset = start from scratch | `get_next_steps()` resumes exactly where you left off |
| Lessons learned vanish after context window | Lessons persist as searchable semantic rules |
| No visibility into AI decision-making | Dashboard + webhooks + full attempt audit trail |
| Each project is isolated | Cross-project search shares lessons across codebases |
| Manual "remember this" notes | Automatic lesson consolidation from resolved tasks |
| One-shot vector search | Hybrid RRF: vector + BM25 + category + recency + popularity |
