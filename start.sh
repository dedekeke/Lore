#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Load .env if present
if [[ -f .env ]]; then
    set -a
    source .env
    set +a
fi

# Override LOG_LEVEL for verbose mode: ./start.sh --verbose
if [[ "${1:-}" == "--verbose" ]]; then
    export LOG_LEVEL=debug
    shift
fi

exec cargo run --release "$@"
