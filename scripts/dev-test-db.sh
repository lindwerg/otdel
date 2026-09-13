#!/usr/bin/env bash
# Provision the **separate** integration-test database and print the variables the test
# suite needs.
#
# The tests create, mutate and delete rows, so they must not run against the pilot
# database `otdel`. Everything here targets `otdel_test`, a physically separate database
# in the same container, owned by the migration role and reachable by the same
# restricted runtime role — so the tests exercise the real privilege setup without ever
# touching pilot data.
#
# The variables the suite reads:
#   OTDEL_TEST_DATABASE_URL       — restricted runtime role on otdel_test
#   OTDEL_TEST_ADMIN_DATABASE_URL — migration role on otdel_test
#   OTDEL_TEST_SUPERUSER_URL      — container superuser; only the negative test that a
#                                   privileged role is refused at startup uses it
#
# Usage:
#   scripts/dev-test-db.sh              provision + print instructions
#   eval "$(scripts/dev-test-db.sh --export)"   set them in the current shell
#   make test-db                        provision and run the whole suite

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

TEST_DB_NAME="otdel_test"
PILOT_DB_NAME="otdel"

if [[ ! -f .local/otdel.env || ! -f .local/db.env ]]; then
    echo "dev-test-db: .local/otdel.env or .local/db.env is missing; run scripts/dev-init.sh first." >&2
    exit 1
fi

set -a
# shellcheck disable=SC1091
. ./.local/otdel.env
# shellcheck disable=SC1091
. ./.local/db.env
set +a

compose="${COMPOSE:-docker compose}"

# Refuse to continue if the pilot URL somehow points at the test database or the other
# way round: that mix-up is exactly what this script exists to prevent.
if [[ "$OTDEL_DATABASE_URL" == *"/${TEST_DB_NAME}" ]]; then
    echo "dev-test-db: .local/otdel.env points the pilot at ${TEST_DB_NAME}; refusing to continue." >&2
    exit 1
fi

# --- idempotent provisioning -------------------------------------------------------
# The initdb script creates otdel_test on a fresh volume. For a volume that already
# existed, create it here. `docker compose exec` is used so no psql client is needed on
# the host; credentials come from the container's own environment.
exists="$(
    $compose exec -T postgres \
        psql --quiet --no-psqlrc --tuples-only --no-align \
             --username "$POSTGRES_USER" --dbname "$PILOT_DB_NAME" \
             -c "SELECT 1 FROM pg_database WHERE datname = '${TEST_DB_NAME}'" 2>/dev/null || true
)"

if [[ "$(echo "$exists" | tr -d '[:space:]')" != "1" ]]; then
    echo "dev-test-db: creating the ${TEST_DB_NAME} database..."
    $compose exec -T postgres \
        psql --quiet --no-psqlrc --set ON_ERROR_STOP=1 \
             --username "$POSTGRES_USER" --dbname "$PILOT_DB_NAME" <<SQL
CREATE DATABASE ${TEST_DB_NAME} OWNER otdel_migrator;
REVOKE ALL ON DATABASE ${TEST_DB_NAME} FROM PUBLIC;
GRANT CONNECT ON DATABASE ${TEST_DB_NAME} TO otdel_migrator, otdel_app;
SQL
    $compose exec -T postgres \
        psql --quiet --no-psqlrc --set ON_ERROR_STOP=1 \
             --username "$POSTGRES_USER" --dbname "${TEST_DB_NAME}" <<SQL
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO otdel_migrator;
SQL
fi

# Swap the database name in the configured URLs; credentials and host stay as generated.
test_runtime_url="${OTDEL_DATABASE_URL%/${PILOT_DB_NAME}}/${TEST_DB_NAME}"
test_admin_url="${OTDEL_DATABASE_ADMIN_URL%/${PILOT_DB_NAME}}/${TEST_DB_NAME}"
test_superuser_url="postgres://${POSTGRES_USER}:${POSTGRES_PASSWORD}@127.0.0.1:58432/${TEST_DB_NAME}"

# --- migrate the test database (never the pilot one) --------------------------------
OTDEL_DATABASE_URL="$test_runtime_url" \
OTDEL_DATABASE_ADMIN_URL="$test_admin_url" \
    cargo run --quiet --bin otdel-api -- migrate >/dev/null
OTDEL_DATABASE_URL="$test_runtime_url" \
OTDEL_DATABASE_ADMIN_URL="$test_admin_url" \
    cargo run --quiet --bin otdel-api -- bootstrap >/dev/null

if [[ "${1:-}" == "--export" ]]; then
    printf 'export OTDEL_TEST_DATABASE_URL=%q\n' "$test_runtime_url"
    printf 'export OTDEL_TEST_ADMIN_DATABASE_URL=%q\n' "$test_admin_url"
    printf 'export OTDEL_TEST_SUPERUSER_URL=%q\n' "$test_superuser_url"
    exit 0
fi

echo "dev-test-db: ${TEST_DB_NAME} is migrated and provisioned (pilot database untouched)."
echo "dev-test-db: run the integration tests with:"
echo "  eval \"\$(scripts/dev-test-db.sh --export)\" && cargo test --workspace"
