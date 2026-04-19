#!/usr/bin/env bash
# Lore — one-command boot.
# Usage: ./start.sh [start|stop|restart|status|logs|foreground] [--verbose]
#
# Ensures Postgres (docker), builds release binary once, runs Lore as an
# SSE HTTP daemon on http://localhost:3101/sse. PID → run/lore.pid, logs → run/lore.log.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

RUN_DIR="$SCRIPT_DIR/run"
PID_FILE="$RUN_DIR/lore.pid"
LOG_FILE="$RUN_DIR/lore.log"
BIN_PATH="$SCRIPT_DIR/target/release/lore"
DB_CONTAINER="${LORE_DB_CONTAINER:-lore-db}"
DB_IMAGE="pgvector/pgvector:pg16"
DB_PORT="${LORE_DB_PORT:-5432}"

mkdir -p "$RUN_DIR"

[[ -f .env ]] && { set -a; source .env; set +a; }

# Daemon always runs SSE — it's pointless otherwise. Override anything .env set.
# Users who want stdio should invoke the binary directly.
export MCP_TRANSPORT=sse
export MCP_SSE_PORT="${LORE_SSE_PORT:-3101}"
: "${DATABASE_URL:=postgres://lore:password@localhost:${DB_PORT}/ai_memory}"
export DATABASE_URL

log()  { printf '[lore] %s\n' "$*" >&2; }
warn() { printf '[lore] WARN: %s\n' "$*" >&2; }
die()  { printf '[lore] ERROR: %s\n' "$*" >&2; exit 1; }

is_running() {
    [[ -f "$PID_FILE" ]] || return 1
    local pid; pid="$(cat "$PID_FILE" 2>/dev/null || true)"
    [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

ensure_postgres() {
    # Skip if user points DATABASE_URL at a non-localhost host.
    if [[ "$DATABASE_URL" != *"localhost"* && "$DATABASE_URL" != *"127.0.0.1"* ]]; then
        log "DATABASE_URL is remote; skipping local Postgres check"
        return 0
    fi
    if ! command -v docker >/dev/null 2>&1; then
        warn "docker not found — assuming Postgres is already running at $DATABASE_URL"
        return 0
    fi
    if docker ps --format '{{.Names}}' | grep -qx "$DB_CONTAINER"; then
        log "Postgres already running ($DB_CONTAINER)"
        return 0
    fi
    if docker ps -a --format '{{.Names}}' | grep -qx "$DB_CONTAINER"; then
        log "Starting existing Postgres container ($DB_CONTAINER)"
        docker start "$DB_CONTAINER" >/dev/null
    else
        log "Creating Postgres container ($DB_CONTAINER) on port $DB_PORT"
        docker run -d --name "$DB_CONTAINER" \
            -e POSTGRES_USER=lore \
            -e POSTGRES_PASSWORD=password \
            -e POSTGRES_DB=ai_memory \
            -p "${DB_PORT}:5432" \
            "$DB_IMAGE" >/dev/null
    fi
    log "Waiting for Postgres to accept connections..."
    for _ in $(seq 1 30); do
        if docker exec "$DB_CONTAINER" pg_isready -U lore -d ai_memory >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    die "Postgres did not become ready within 30s"
}

build_if_needed() {
    if [[ -x "$BIN_PATH" ]]; then
        log "Using existing release binary ($BIN_PATH)"
        return 0
    fi
    log "Building release binary (first run only — can take a few minutes)"
    cargo build --release
}

preflight() {
    if [[ "${DASHBOARD_ENABLED:-false}" == "true" && "${DASHBOARD_PORT:-3102}" == "$MCP_SSE_PORT" ]]; then
        die "DASHBOARD_PORT (${DASHBOARD_PORT}) conflicts with MCP_SSE_PORT (${MCP_SSE_PORT}) — change one in .env"
    fi
    # Port already bound? Use lsof if available; fall back to a probe.
    if command -v lsof >/dev/null 2>&1 && lsof -iTCP:"$MCP_SSE_PORT" -sTCP:LISTEN >/dev/null 2>&1; then
        local holder; holder="$(lsof -iTCP:"$MCP_SSE_PORT" -sTCP:LISTEN -n -P | awk 'NR==2 {print $1"(pid "$2")"}')"
        die "Port $MCP_SSE_PORT already in use by $holder. Stop it, or set LORE_SSE_PORT to another port."
    fi
}

cmd_start() {
    if is_running; then
        log "Already running (pid $(cat "$PID_FILE")) — http://localhost:${MCP_SSE_PORT}/sse"
        return 0
    fi
    ensure_postgres
    build_if_needed
    preflight
    log "Starting Lore daemon on http://localhost:${MCP_SSE_PORT}/sse"
    nohup "$BIN_PATH" >>"$LOG_FILE" 2>&1 &
    echo $! >"$PID_FILE"
    sleep 2
    if ! is_running; then
        rm -f "$PID_FILE"
        die "Lore failed to start — tail $LOG_FILE for details"
    fi
    log "Started (pid $(cat "$PID_FILE")). MCP client config: {\"mcpServers\":{\"lore\":{\"url\":\"http://localhost:${MCP_SSE_PORT}/sse\"}}}"
}

cmd_stop() {
    if ! is_running; then
        log "Not running"
        rm -f "$PID_FILE"
        return 0
    fi
    local pid; pid="$(cat "$PID_FILE")"
    log "Stopping (pid $pid)"
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 10); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 1
    done
    kill -0 "$pid" 2>/dev/null && kill -9 "$pid" 2>/dev/null || true
    rm -f "$PID_FILE"
    log "Stopped"
}

cmd_status() {
    if is_running; then
        log "Running (pid $(cat "$PID_FILE")) on http://localhost:${MCP_SSE_PORT}/sse"
    else
        log "Not running"
        exit 1
    fi
}

cmd_logs() { tail -f "$LOG_FILE"; }

cmd_foreground() {
    ensure_postgres
    build_if_needed
    log "Running in foreground on http://localhost:${MCP_SSE_PORT}/sse"
    exec "$BIN_PATH"
}

ACTION="${1:-start}"
[[ "${2:-}" == "--verbose" ]] && export LOG_LEVEL=debug

case "$ACTION" in
    start)      cmd_start ;;
    stop)       cmd_stop ;;
    restart)    cmd_stop; cmd_start ;;
    status)     cmd_status ;;
    logs)       cmd_logs ;;
    foreground|fg) cmd_foreground ;;
    *)          die "Unknown action: $ACTION (use: start|stop|restart|status|logs|foreground)" ;;
esac
