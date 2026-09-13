#!/usr/bin/env bash
# Run the API server with the local environment from .local/otdel.env.
#
# Usage: scripts/dev-api.sh [serve|migrate|bootstrap|check-config]
# Default: serve (127.0.0.1:18480).

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ ! -f .local/otdel.env ]]; then
    echo "dev-api: .local/otdel.env is missing; run scripts/dev-init.sh first." >&2
    exit 1
fi

set -a
# shellcheck disable=SC1091
. ./.local/otdel.env
set +a

exec cargo run --quiet --bin otdel-api -- "${1:-serve}"
