#!/usr/bin/env bash
# Run the maintenance worker with the local environment from .local/otdel.env.
#
# Usage: scripts/dev-worker.sh [run|once]
#
# Phase 1A maintenance only (expired sessions, job lease recovery, staging sweep,
# orphan reporting). It does not extract documents — that is phase 1B.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ ! -f .local/otdel.env ]]; then
    echo "dev-worker: .local/otdel.env is missing; run scripts/dev-init.sh first." >&2
    exit 1
fi

set -a
# shellcheck disable=SC1091
. ./.local/otdel.env
set +a

exec cargo run --quiet --bin otdel-worker -- "${1:-run}"
