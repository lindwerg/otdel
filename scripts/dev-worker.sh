#!/usr/bin/env bash
# Run the background worker with the local environment from .local/otdel.env.
#
# Usage: scripts/dev-worker.sh [run|once|probe]
#
#   run   — extraction + product understanding + maintenance until stopped
#   once  — one pass of each, then exit
#   probe — report OCR engine, page rasteriser and model adapter availability
#
# Since phase 1B the worker reads documents: queued materials become per-page records.
# Since phase 1C it also drafts product knowledge from the pages that were read; with
# no model key configured it calls nothing and records each run as `needs_provider`.
# Maintenance (expired sessions, job lease recovery, staging sweep, orphan reporting)
# continues to run alongside both.

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
