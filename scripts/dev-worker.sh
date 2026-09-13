#!/usr/bin/env bash
# Run the background worker with the local environment from .local/otdel.env.
#
# Usage: scripts/dev-worker.sh [run|once|probe]
#
#   run   — extraction + maintenance until stopped
#   once  — one extraction pass and one maintenance pass, then exit
#   probe — report whether the OCR engine and the page rasteriser are installed
#
# Since phase 1B the worker reads documents: queued materials become per-page records.
# Maintenance (expired sessions, job lease recovery, staging sweep, orphan reporting)
# continues to run alongside it.

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
