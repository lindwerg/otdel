#!/usr/bin/env bash
# Provision a **separate** integration-test database and print the variables the test
# suite needs.
#
# The tests create, mutate and delete rows, so they must not run against the pilot
# database (`otdel`). This script targets a physically separate database in the same
# container, owned by the migration role and reachable by the same restricted runtime
# role — so the tests exercise the real privilege setup without ever touching pilot data.
#
# Per-worktree isolation
# ----------------------
# The default test database is `otdel_test`. That single name is fine for one checkout,
# but several worktrees running `make test-db` at the same time would share one database
# and corrupt each other's fixtures. Give each worktree its own name instead:
#
#   export OTDEL_TEST_DB_NAME=otdel_r01_isolation
#   scripts/dev-test-db.sh
#
# The name may also be written into `.local/otdel.env` so every invocation in that
# worktree picks it up automatically. `OTDEL_TEST_DB_HOST_PORT` additionally retargets
# the derived URLs at a different published PostgreSQL port, for the case where a
# worktree runs its own container; with the shared `compose.yaml` container (published on
# 127.0.0.1:58432) it is not needed — distinct database names are enough.
#
# The variables the suite reads:
#   OTDEL_TEST_DATABASE_URL       — restricted runtime role on the test database
#   OTDEL_TEST_ADMIN_DATABASE_URL — migration role on the test database
#   OTDEL_TEST_SUPERUSER_URL      — container superuser; only the negative test that a
#                                   privileged role is refused at startup uses it
#
# Usage:
#   scripts/dev-test-db.sh                      provision + print instructions
#   eval "$(scripts/dev-test-db.sh --export)"   set them in the current shell
#   scripts/dev-test-db.sh --export --dry-run   resolve + validate only, touch nothing
#   scripts/dev-test-db.sh --drop --confirm NAME  delete one disposable test database
#   make test-db                                provision and run the whole suite
#
# Safety properties this script is expected to keep (see scripts/tests/dev-test-db.test.sh):
#   * the normal provisioning path contains no DROP DATABASE and never recreates an
#     existing database — it only creates one that is missing, then migrates it;
#   * the pilot database is never a valid target, in either direction;
#   * every status line goes to stderr, so `--export` stdout is always pure shell;
#   * the database name is validated against a strict allowlist before it is ever
#     interpolated into SQL, so no name can inject a second statement.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

readonly DEFAULT_TEST_DB_NAME="otdel_test"
readonly PILOT_DB_NAME="otdel"
# PostgreSQL truncates identifiers at NAMEDATALEN-1.
readonly MAX_DB_NAME_LENGTH=63
readonly REQUIRED_DROP_PREFIX="otdel_"

# Every diagnostic goes to stderr, unconditionally. `--export` writes shell code to
# stdout and callers `eval` it; a single stray status line there would be executed.
say() { printf 'dev-test-db: %s\n' "$*" >&2; }
die() {
    printf 'dev-test-db: %s\n' "$*" >&2
    exit 1
}

usage() {
    # The file header is the documentation; print it without the comment markers.
    sed -n '2,45p' "${BASH_SOURCE[0]}" | sed 's/^#\{1\} \{0,1\}//' >&2
}

# --- arguments ----------------------------------------------------------------------
mode="provision"
export_env=0
dry_run=0
confirm_name=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --export) export_env=1 ;;
        --dry-run) dry_run=1 ;;
        --drop) mode="drop" ;;
        --confirm)
            [[ $# -ge 2 ]] || die "--confirm needs the database name as its argument."
            confirm_name="$2"
            shift
            ;;
        --confirm=*) confirm_name="${1#--confirm=}" ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) die "unknown argument '$1' (try --help)." ;;
    esac
    shift
done

if [[ "$mode" == "drop" && $export_env -eq 1 ]]; then
    die "--drop and --export are separate operations; run them one at a time."
fi

# --- environment --------------------------------------------------------------------
# The file paths are overridable so the script's own test suite can drive it with
# throw-away fixtures instead of the real .local secrets. Normal runs use the defaults.
env_file="${OTDEL_ENV_FILE:-.local/otdel.env}"
db_env_file="${OTDEL_DB_ENV_FILE:-.local/db.env}"

# Captured before sourcing, so a value passed by the caller can be told apart from one
# that a worktree wrote into its env file. `--drop` requires an explicit choice.
name_from_caller="${OTDEL_TEST_DB_NAME:-}"

if [[ ! -f "$env_file" || ! -f "$db_env_file" ]]; then
    die "$env_file or $db_env_file is missing; run scripts/dev-init.sh first."
fi

# `source` searches PATH for a bare relative name, so a path without a slash gets an
# explicit `./`. An absolute path must be left exactly as it is.
[[ "$env_file" == */* ]] || env_file="./$env_file"
[[ "$db_env_file" == */* ]] || db_env_file="./$db_env_file"

set -a
# shellcheck disable=SC1090
. "$env_file"
# shellcheck disable=SC1090
. "$db_env_file"
set +a

for required in OTDEL_DATABASE_URL OTDEL_DATABASE_ADMIN_URL POSTGRES_USER POSTGRES_PASSWORD; do
    [[ -n "${!required:-}" ]] || die "$required is not set by $env_file / $db_env_file."
done

# --- validation ---------------------------------------------------------------------
# A database name reaches SQL as a bare identifier, so it is checked against a strict
# allowlist rather than escaped: lowercase letter first, then lowercase letters, digits
# and underscores. Nothing that could terminate a statement, open a quote, or mean
# anything to a shell can survive this.
validate_db_name() {
    local name="$1"

    [[ -n "$name" ]] || die "OTDEL_TEST_DB_NAME is empty; unset it to use the default '${DEFAULT_TEST_DB_NAME}'."
    if ((${#name} > MAX_DB_NAME_LENGTH)); then
        die "test database name '${name}' is longer than ${MAX_DB_NAME_LENGTH} characters."
    fi
    if [[ ! "$name" =~ ^[a-z][a-z0-9_]*$ ]]; then
        die "test database name '${name}' is not allowed; use lowercase letters, digits and underscores, starting with a letter (e.g. otdel_r01_isolation)."
    fi
}

validate_host_port() {
    local port="$1"
    if [[ ! "$port" =~ ^[0-9]+$ ]] || ((port < 1 || port > 65535)); then
        die "OTDEL_TEST_DB_HOST_PORT '${port}' is not a TCP port between 1 and 65535."
    fi
}

# postgres://user:password@host:port/dbname — no query string, so the database name is
# unambiguously the last path segment and can be swapped without losing parameters.
validate_pg_url() {
    local label="$1" url="$2"
    if [[ ! "$url" =~ ^[a-z][a-z0-9+.-]*://[^@/]+@[^@/]+/[^/?#]+$ ]]; then
        die "${label} is not a plain postgres://user:password@host:port/dbname URL; this script cannot rewrite it safely."
    fi
}

url_field() {
    # $1 = url, $2 = one of scheme|credentials|hostport|dbname
    local url="$1"
    case "$2" in
        scheme) printf '%s' "${url%%://*}" ;;
        credentials)
            local rest="${url#*://}"
            printf '%s' "${rest%%@*}"
            ;;
        hostport)
            local rest="${url#*@}"
            printf '%s' "${rest%%/*}"
            ;;
        dbname) printf '%s' "${url##*/}" ;;
    esac
}

validate_pg_url "OTDEL_DATABASE_URL" "$OTDEL_DATABASE_URL"
validate_pg_url "OTDEL_DATABASE_ADMIN_URL" "$OTDEL_DATABASE_ADMIN_URL"

pilot_runtime_db="$(url_field "$OTDEL_DATABASE_URL" dbname)"
pilot_admin_db="$(url_field "$OTDEL_DATABASE_ADMIN_URL" dbname)"

# `+set` rather than `:-`: an explicitly empty OTDEL_TEST_DB_NAME is a mistake worth
# reporting, not a silent fall back to the shared default.
if [[ -n "${OTDEL_TEST_DB_NAME+set}" ]]; then
    test_db_name="$OTDEL_TEST_DB_NAME"
else
    test_db_name="$DEFAULT_TEST_DB_NAME"
fi
validate_db_name "$test_db_name"

# Refuse the pilot database in either direction: as the test target, or as a pilot URL
# that already points at the test database. That mix-up is what this script exists to
# prevent, and `otdel` itself is refused even if the pilot URL were repointed elsewhere.
for protected in "$PILOT_DB_NAME" "$pilot_runtime_db" "$pilot_admin_db"; do
    if [[ "$test_db_name" == "$protected" ]]; then
        die "refusing to use '${test_db_name}' as the test database: that is the pilot database."
    fi
done
if [[ "$pilot_runtime_db" != "$pilot_admin_db" ]]; then
    die "OTDEL_DATABASE_URL ('${pilot_runtime_db}') and OTDEL_DATABASE_ADMIN_URL ('${pilot_admin_db}') name different databases; fix $env_file first."
fi

# --- derive the test URLs -----------------------------------------------------------
# Host and credentials are taken from the configured pilot URLs; only the database name
# changes, plus the published port when OTDEL_TEST_DB_HOST_PORT asks for a different
# container.
pilot_hostport="$(url_field "$OTDEL_DATABASE_URL" hostport)"
[[ "$pilot_hostport" == *:* ]] || die "OTDEL_DATABASE_URL has no explicit port; expected host:port."
pilot_host="${pilot_hostport%:*}"
pilot_port="${pilot_hostport##*:}"

test_host_port="${OTDEL_TEST_DB_HOST_PORT:-$pilot_port}"
validate_host_port "$test_host_port"

build_url() {
    # $1 = template url (for scheme + credentials), $2 = database name
    printf '%s://%s@%s:%s/%s' \
        "$(url_field "$1" scheme)" "$(url_field "$1" credentials)" \
        "$pilot_host" "$test_host_port" "$2"
}

test_runtime_url="$(build_url "$OTDEL_DATABASE_URL" "$test_db_name")"
test_admin_url="$(build_url "$OTDEL_DATABASE_ADMIN_URL" "$test_db_name")"
test_superuser_url="postgres://${POSTGRES_USER}:${POSTGRES_PASSWORD}@${pilot_host}:${test_host_port}/${test_db_name}"

compose="${COMPOSE:-docker compose}"

psql_maintenance() {
    # A connection to the pilot database used only as a maintenance entry point:
    # CREATE DATABASE cannot run inside the database it creates. Nothing here writes to
    # the pilot database itself.
    $compose exec -T postgres \
        psql --quiet --no-psqlrc --set ON_ERROR_STOP=1 \
        --username "$POSTGRES_USER" --dbname "$pilot_runtime_db" "$@"
}

database_exists() {
    local found
    found="$(
        $compose exec -T postgres \
            psql --quiet --no-psqlrc --tuples-only --no-align \
            --username "$POSTGRES_USER" --dbname "$pilot_runtime_db" \
            -c "SELECT 1 FROM pg_database WHERE datname = '${1}'" 2>/dev/null || true
    )"
    [[ "$(printf '%s' "$found" | tr -d '[:space:]')" == "1" ]]
}

# --- drop: an explicit, separate, heavily guarded operation --------------------------
# Never reachable from the provisioning path. It exists so a finished worktree can clean
# up the database it created, and it refuses anything that is not unambiguously a
# disposable per-worktree database.
if [[ "$mode" == "drop" ]]; then
    explicit_name="${name_from_caller:-${OTDEL_TEST_DB_NAME:-}}"
    [[ -n "$explicit_name" ]] ||
        die "--drop needs OTDEL_TEST_DB_NAME set explicitly; it will not guess a target."
    [[ "$test_db_name" != "$DEFAULT_TEST_DB_NAME" ]] ||
        die "--drop refuses the shared default '${DEFAULT_TEST_DB_NAME}': other worktrees may be using it. Drop only a uniquely named database."
    [[ "$test_db_name" == "$REQUIRED_DROP_PREFIX"* ]] ||
        die "--drop only accepts names starting with '${REQUIRED_DROP_PREFIX}' (e.g. ${REQUIRED_DROP_PREFIX}r01_isolation); got '${test_db_name}'."
    [[ -n "$confirm_name" ]] ||
        die "--drop needs --confirm ${test_db_name} to proceed."
    [[ "$confirm_name" == "$test_db_name" ]] ||
        die "--confirm '${confirm_name}' does not match OTDEL_TEST_DB_NAME '${test_db_name}'; refusing."

    if ((dry_run)); then
        say "dry run: would drop '${test_db_name}' (nothing was executed)."
        exit 0
    fi

    if ! database_exists "$test_db_name"; then
        say "'${test_db_name}' does not exist; nothing to drop."
        exit 0
    fi
    say "dropping '${test_db_name}' (pilot database '${pilot_runtime_db}' untouched)..."
    psql_maintenance -c "DROP DATABASE ${test_db_name}"
    say "'${test_db_name}' dropped."
    exit 0
fi

# --- idempotent provisioning ---------------------------------------------------------
# The initdb script creates the default test database on a fresh volume. Any other name,
# or a volume that already existed, is handled here. An existing database is left exactly
# as it is — it is migrated, never recreated — so a concurrent worktree that happens to
# share a name loses no data to this script.
if ((dry_run)); then
    say "dry run: resolved test database '${test_db_name}' on ${pilot_host}:${test_host_port}; nothing was created or migrated."
else
    if database_exists "$test_db_name"; then
        say "'${test_db_name}' already exists; reusing it (never dropped, never recreated)."
    else
        say "creating the '${test_db_name}' database..."
        # The name is safe to interpolate: validate_db_name accepted only
        # ^[a-z][a-z0-9_]*$, which cannot close a statement or a quoted string.
        psql_maintenance <<SQL
CREATE DATABASE ${test_db_name} OWNER otdel_migrator;
REVOKE ALL ON DATABASE ${test_db_name} FROM PUBLIC;
GRANT CONNECT ON DATABASE ${test_db_name} TO otdel_migrator, otdel_app;
SQL
        $compose exec -T postgres \
            psql --quiet --no-psqlrc --set ON_ERROR_STOP=1 \
            --username "$POSTGRES_USER" --dbname "${test_db_name}" <<SQL
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO otdel_migrator;
SQL
    fi

    # --- migrate the test database (never the pilot one) ------------------------------
    OTDEL_DATABASE_URL="$test_runtime_url" \
        OTDEL_DATABASE_ADMIN_URL="$test_admin_url" \
        cargo run --quiet --bin otdel-api -- migrate >/dev/null
    OTDEL_DATABASE_URL="$test_runtime_url" \
        OTDEL_DATABASE_ADMIN_URL="$test_admin_url" \
        cargo run --quiet --bin otdel-api -- bootstrap >/dev/null
fi

if ((export_env)); then
    printf 'export OTDEL_TEST_DATABASE_URL=%q\n' "$test_runtime_url"
    printf 'export OTDEL_TEST_ADMIN_DATABASE_URL=%q\n' "$test_admin_url"
    printf 'export OTDEL_TEST_SUPERUSER_URL=%q\n' "$test_superuser_url"
    exit 0
fi

say "'${test_db_name}' is migrated and provisioned (pilot database '${pilot_runtime_db}' untouched)."
say "run the integration tests with:"
say "  eval \"\$(scripts/dev-test-db.sh --export)\" && cargo test --workspace"
if [[ "$test_db_name" == "$DEFAULT_TEST_DB_NAME" ]]; then
    say "this is the shared default name. For a parallel worktree, set OTDEL_TEST_DB_NAME=otdel_<task>_<suffix> first."
fi
