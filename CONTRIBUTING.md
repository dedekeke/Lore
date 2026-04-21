# Contributing to Lore

Thanks for your interest. Lore is an MCP server for episodic memory + codebase
graphs — PRs, bug reports, and design discussions are all welcome.

## Ground rules

- **One focused change per PR.** A bug fix, a feature, a refactor — pick one.
  Mixing surface area makes review slow and rollbacks painful.
- **Keep PRs reviewable.** Large features welcome, but break them into
  sequenced PRs behind a parent issue if they cross ~600 lines of diff.
- **Tests required** for new behavior and every bug fix. Integration tests for
  anything touching DB, MCP handlers, or the eval harness.
- **Migrations via `sqlx`.** Use `sqlx migrate add <name>` — never hand-craft
  migration filenames with appended timestamps.
- **No secrets in commits.** `.env` is gitignored; `.env.example` is the
  source of truth for config. The built-in scrubber catches common tokens
  but don't rely on it.

## Dev loop

```bash
cp .env.example .env     # configure DB URL + ports; keep .env untracked
./start.sh               # requires Docker running; boots Postgres + builds + runs daemon
cargo fmt                # before every push
cargo clippy --all-features --all-targets -- -D warnings
cargo test --all-features
```

CI runs `fmt`, `clippy -D warnings`, `check`, and the full test suite (with a
pgvector testcontainer) on every PR. Lint failures block merge.

## Commits

Conventional-style prefixes are preferred:

- `feat(scope): …`  new user-facing behavior
- `fix(scope): …`  bug fix
- `refactor(scope): …`  no behavior change
- `chore(scope): …`  tooling, deps, docs-only

Keep the subject under 72 chars. Describe the *why* in the body when it's
non-obvious.

## Filing issues

Include:

- Lore version / commit SHA
- Postgres + pgvector versions
- Minimal repro (MCP tool call sequence, or a failing `cargo test`)
- Relevant snippet of `run/lore.log` (redact DB URL / API keys first)

## Code of conduct

Be kind, be direct, assume good faith. Harassment, personal attacks, or
demeaning language are not tolerated.
