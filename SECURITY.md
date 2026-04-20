# Security Policy

## Scope

Lore is a **local, single-user daemon** designed to run on `127.0.0.1`. It has
no authentication layer. If you expose it to a network (`MCP_SSE_BIND=0.0.0.0`)
you are responsible for putting it behind a trusted boundary — a reverse proxy
with auth, a WireGuard tunnel, or equivalent.

The MCP tool surface is unauthenticated by design. Anything that can reach the
port can read/write the memory ledger, execute codebase indexing, and
trigger webhooks.

## Reporting a vulnerability

Please **do not** open a public issue for security bugs. Instead, open a
[GitHub security advisory](https://docs.github.com/en/code-security/security-advisories/working-with-repository-security-advisories/creating-a-repository-security-advisory)
on this repo — that channel is private until the advisory is published.

Expected response time: within a week for acknowledgement, longer for a fix
depending on severity. Lore is volunteer-maintained.

Please include:

- Affected version / commit SHA
- Reproduction steps
- Impact assessment (what an attacker can achieve)
- Suggested mitigation, if you have one

## Data you send to Lore

Lore runs a regex-based **secret scrubber** on every store path (AWS keys,
API tokens, PEM blocks, connection strings, JWTs, Bearer tokens, GitHub PATs)
— enabled by default via `LORE_SCRUB_SECRETS=true`. The scrubber is defense
in depth, not a guarantee. **Don't paste secrets into rules, attempts, or
task descriptions.** If you find a pattern the scrubber misses, a PR against
`src/scrubber.rs` with a new regex + test is very welcome.

## Dependencies

We track advisories on the Rust toolchain and our direct dependencies via
`cargo audit` in CI (when present). If you notice a CVE affecting a crate we
pin, please open an issue — we'll bump.
