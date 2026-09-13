#!/usr/bin/env bash
# Tests for scripts/dev-test-db.sh — name validation, URL derivation, and the guards that
# keep the pilot database and other worktrees safe.
#
# These tests never touch a real database and never read .local/. They point the script
# at throw-away fixture env files created in a temporary directory with obviously fake
# credentials, and run it with --dry-run, which resolves and validates everything but
# executes no docker, psql or cargo command. That is deliberate: a test suite for a
# script whose whole job is to *not* destroy databases must not need a database to run.
#
# Run: scripts/tests/dev-test-db.test.sh   (or: make test-scripts)

set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="${repo_root}/scripts/dev-test-db.sh"

fixtures="$(mktemp -d "${TMPDIR:-/tmp}/otdel-test-db-fixtures.XXXXXX")"
trap 'rm -rf "$fixtures"' EXIT

# Fake, non-secret values, spelled out so nobody mistakes them for real credentials if
# they ever show up in a log.
readonly FAKE_PASSWORD="not-a-real-password-fixture"

cat > "${fixtures}/otdel.env" <<EOF
OTDEL_DATABASE_URL='postgres://otdel_app:${FAKE_PASSWORD}@127.0.0.1:58432/otdel'
OTDEL_DATABASE_ADMIN_URL='postgres://otdel_migrator:${FAKE_PASSWORD}@127.0.0.1:58432/otdel'
EOF

cat > "${fixtures}/db.env" <<EOF
POSTGRES_USER=otdel_superuser
POSTGRES_PASSWORD=${FAKE_PASSWORD}
EOF

# A pilot env that points at a non-default database, used to prove the pilot is refused
# by value and not only by the hard-coded literal `otdel`.
cat > "${fixtures}/otdel-alt-pilot.env" <<EOF
OTDEL_DATABASE_URL='postgres://otdel_app:${FAKE_PASSWORD}@127.0.0.1:58432/otdel_pilot_alt'
OTDEL_DATABASE_ADMIN_URL='postgres://otdel_migrator:${FAKE_PASSWORD}@127.0.0.1:58432/otdel_pilot_alt'
EOF

passed=0
failed=0
stdout_file="${fixtures}/stdout"
stderr_file="${fixtures}/stderr"
status=0

ok() {
    passed=$((passed + 1))
    printf '  ok   %s\n' "$1"
}

bad() {
    failed=$((failed + 1))
    printf '  FAIL %s\n' "$1"
    printf '       status=%s\n       stdout: %s\n       stderr: %s\n' \
        "$status" "$(cat "$stdout_file")" "$(head -c 300 "$stderr_file")"
}

# check <description> <exit-status>
check() {
    if [[ "$2" -eq 0 ]]; then ok "$1"; else bad "$1"; fi
}

# run_script [ENVFILE=<fixture>] [VAR=value ...] -- <args...>
# Always runs with --dry-run and the fixture env files; captures stdout/stderr/status.
# ENVFILE picks a different pilot-env fixture (relative to the fixtures directory).
# Written for bash 3.2 (the system bash on macOS): an empty array must never be expanded
# under `set -u`, hence the `${arr[@]+...}` guard.
run_script() {
    local -a assignments=()
    local env_file="${fixtures}/otdel.env"
    while [[ $# -gt 0 && "$1" != "--" ]]; do
        case "$1" in
            ENVFILE=*) env_file="${fixtures}/${1#ENVFILE=}" ;;
            *) assignments[${#assignments[@]}]="$1" ;;
        esac
        shift
    done
    shift || true
    env -u OTDEL_TEST_DB_NAME -u OTDEL_TEST_DB_HOST_PORT \
        "OTDEL_ENV_FILE=${env_file}" \
        "OTDEL_DB_ENV_FILE=${fixtures}/db.env" \
        ${assignments[@]+"${assignments[@]}"} \
        bash "$script" --dry-run "$@" \
        > "$stdout_file" 2> "$stderr_file"
    status=$?
    return 0
}

expect_success() {
    [[ "$status" -eq 0 ]]
    check "$1" $?
}

expect_rejected() {
    # Non-zero exit, a deliberate `dev-test-db:` explanation on stderr (not an incidental
    # bash error, which would make any broken invocation look like a working guard), and —
    # critically — nothing on stdout, so a caller doing eval "$(...)" can never evaluate a
    # partial result.
    [[ "$status" -ne 0 && ! -s "$stdout_file" ]] && grep -q '^dev-test-db: ' "$stderr_file"
    check "$1" $?
}

stdout_is_pure_exports() {
    # Every stdout line must be an `export OTDEL_TEST_...=` assignment: no status text, no
    # blank lines, nothing a shell would run as a command.
    local line
    [[ -s "$stdout_file" ]] || return 1
    while IFS= read -r line; do
        [[ "$line" =~ ^export\ OTDEL_TEST_[A-Z_]+= ]] || return 1
    done < "$stdout_file"
}

echo "--- dev-test-db.sh ---"

# --- 1. the default stays backward compatible ----------------------------------------
run_script -- --export
expect_success "default run succeeds with no OTDEL_TEST_DB_NAME"
grep -q 'export OTDEL_TEST_DATABASE_URL=.*/otdel_test$' "$stdout_file"
check "the default database name is still otdel_test" $?

# --- 2. --export stdout is shell-only, status goes to stderr --------------------------
stdout_is_pure_exports
check "--export writes only export lines to stdout" $?
[[ -s "$stderr_file" ]]
check "--export writes its status to stderr, not stdout" $?
[[ "$(wc -l < "$stdout_file" | tr -d ' ')" == "3" ]]
check "--export emits exactly three variables" $?

# The acceptance criterion in full: the output must survive `eval` unchanged.
(
    # shellcheck disable=SC1090
    . "$stdout_file"
    [[ "$OTDEL_TEST_DATABASE_URL" == *"otdel_app"*"/otdel_test" ]] &&
        [[ "$OTDEL_TEST_ADMIN_DATABASE_URL" == *"otdel_migrator"*"/otdel_test" ]] &&
        [[ "$OTDEL_TEST_SUPERUSER_URL" == *"otdel_superuser"*"/otdel_test" ]]
)
check "--export output is eval-able and sets all three role URLs" $?

# --- 3. explicit per-worktree names ---------------------------------------------------
run_script OTDEL_TEST_DB_NAME=otdel_r01_isolation -- --export
expect_success "accepts an explicit per-worktree name"
[[ "$(grep -c '/otdel_r01_isolation$' "$stdout_file")" == "3" ]]
check "the explicit name reaches all three URLs" $?

first_url="$(grep '^export OTDEL_TEST_DATABASE_URL=' "$stdout_file")"
run_script OTDEL_TEST_DB_NAME=otdel_r08_uat -- --export
second_url="$(grep '^export OTDEL_TEST_DATABASE_URL=' "$stdout_file")"
[[ -n "$first_url" && -n "$second_url" && "$first_url" != "$second_url" ]]
check "two worktrees get two independent database URLs" $?

for good_name in otdel_test otdel_r01 otdel_r01_isolation o o1 otdel_9_a_b; do
    run_script "OTDEL_TEST_DB_NAME=${good_name}" -- --export
    expect_success "accepts safe name '${good_name}'"
done

# 63 characters is PostgreSQL's identifier limit and must still be accepted.
run_script "OTDEL_TEST_DB_NAME=$(printf 'o%.0s' $(seq 1 63))" -- --export
expect_success "accepts a 63-character name"

# --- 4. rejected names ----------------------------------------------------------------
# SQL injection, shell metacharacters, quoting, case, punctuation, over-length.
rejected_names=(
    'otdel'                               # the pilot database
    'otdel_test; DROP DATABASE otdel; --' # statement injection
    "otdel_test'"                         # quote break-out
    'otdel_test"'                         # identifier quote break-out
    'otdel_test--x'                       # SQL comment
    'otdel_test`id`'                      # command substitution (backticks)
    'otdel_test$(id)'                     # command substitution
    'otdel_test;id'                       # command separator
    'otdel_test|id'                       # pipe
    'otdel_test&'                         # background
    'otdel test'                          # whitespace
    $'otdel_test\nDROP DATABASE otdel'    # embedded newline
    'otdel_test/../otdel'                 # path-traversal shape
    'otdel-test'                          # hyphen: legal when quoted in SQL, refused here
    'Otdel_Test'                          # uppercase
    '9otdel_test'                         # leading digit
    '_otdel_test'                         # leading underscore
    ''                                    # explicitly empty
)
for bad_name in "${rejected_names[@]}"; do
    run_script "OTDEL_TEST_DB_NAME=${bad_name}" -- --export
    expect_rejected "rejects unsafe name '${bad_name:-<empty>}'"
done

run_script "OTDEL_TEST_DB_NAME=$(printf 'o%.0s' $(seq 1 64))" -- --export
expect_rejected "rejects a 64-character name (over PostgreSQL's limit)"

# The pilot is refused by value, not just by the literal string 'otdel'.
run_script ENVFILE=otdel-alt-pilot.env OTDEL_TEST_DB_NAME=otdel_pilot_alt -- --export
expect_rejected "rejects a test name equal to the configured pilot database"

run_script ENVFILE=otdel-alt-pilot.env OTDEL_TEST_DB_NAME=otdel_r01_isolation -- --export
expect_success "a non-default pilot database is otherwise supported"

# --- 5. the optional host port --------------------------------------------------------
run_script OTDEL_TEST_DB_NAME=otdel_r01_ports OTDEL_TEST_DB_HOST_PORT=58433 -- --export
expect_success "accepts an explicit host port"
[[ "$(grep -c '@127.0.0.1:58433/' "$stdout_file")" == "3" ]]
check "the explicit port is applied to all three URLs" $?
! grep -q ':58432/' "$stdout_file"
check "the configured port is not left behind in any URL" $?

run_script OTDEL_TEST_DB_NAME=otdel_r01_ports -- --export
[[ "$(grep -c '@127.0.0.1:58432/' "$stdout_file")" == "3" ]]
check "without the override the URLs keep the configured port 58432" $?

for bad_port in 0 65536 99999 12a '58432;id' '-1' ' '; do
    run_script "OTDEL_TEST_DB_HOST_PORT=${bad_port}" -- --export
    expect_rejected "rejects host port '${bad_port}'"
done

run_script 'OTDEL_TEST_DB_HOST_PORT=' -- --export
expect_success "an empty host port means 'no override'"

# --- 6. the drop guards ---------------------------------------------------------------
run_script OTDEL_TEST_DB_NAME=otdel_r01_isolation -- --drop
expect_rejected "--drop without --confirm is refused"

run_script OTDEL_TEST_DB_NAME=otdel_r01_isolation -- --drop --confirm otdel_r01_other
expect_rejected "--drop with a mismatched --confirm is refused"

run_script -- --drop --confirm otdel_test
expect_rejected "--drop refuses to guess a target when no name is set"

run_script OTDEL_TEST_DB_NAME=otdel_test -- --drop --confirm otdel_test
expect_rejected "--drop refuses the shared default otdel_test even when named explicitly"

run_script OTDEL_TEST_DB_NAME=otdel -- --drop --confirm otdel
expect_rejected "--drop refuses the pilot database"

run_script OTDEL_TEST_DB_NAME=scratch_r01 -- --drop --confirm scratch_r01
expect_rejected "--drop refuses a name outside the otdel_ namespace"

run_script OTDEL_TEST_DB_NAME=otdel_r01_isolation -- --drop --confirm otdel_r01_isolation
expect_success "--drop accepts a uniquely named disposable database"
grep -q 'would drop' "$stderr_file"
check "--drop --dry-run reports the target and executes nothing" $?

run_script OTDEL_TEST_DB_NAME=otdel_r01_isolation -- --drop --confirm otdel_r01_isolation --export
expect_rejected "--drop and --export cannot be combined"

# --- 7. provisioning never drops -------------------------------------------------------
# One executable DROP DATABASE in the whole script, and it sits after the --confirm guard.
drop_lines="$(grep -n 'DROP DATABASE' "$script" | grep -v '^[0-9]*: *#')"
[[ "$(printf '%s\n' "$drop_lines" | grep -c .)" == "1" ]]
check "exactly one executable DROP DATABASE exists in the script" $?

guard_line="$(grep -n 'needs --confirm' "$script" | head -1 | cut -d: -f1)"
drop_line="$(printf '%s' "$drop_lines" | head -1 | cut -d: -f1)"
[[ -n "$guard_line" && -n "$drop_line" ]] && ((drop_line > guard_line))
check "the only DROP DATABASE is reached after the --confirm guard" $?

grep -q 'already exists; reusing it' "$script"
check "provisioning reuses an existing database instead of recreating it" $?

# --- 8. bad invocations -----------------------------------------------------------------
run_script -- --not-a-flag
expect_rejected "an unknown argument is refused"

run_script ENVFILE=missing.env -- --export
expect_rejected "a missing env file is reported without writing to stdout"

# --- 9. the real provisioning path, with docker and cargo stubbed out -------------------
# --dry-run proves the configuration is resolved correctly but never exercises the branch
# that actually talks to a database. Here the compose command and cargo are replaced by
# recording stubs, so the full provisioning path runs for real and the exact SQL and
# environment it would have sent can be asserted — still without a database anywhere.
stub_bin="${fixtures}/bin"
mkdir -p "$stub_bin"
psql_log="${fixtures}/psql.log"
cargo_log="${fixtures}/cargo.log"
exists_answer="${fixtures}/exists-answer"

cat > "${stub_bin}/fake-compose" <<'STUB'
#!/usr/bin/env bash
# Stands in for `docker compose`. Records every argument and every SQL statement it is
# given, and answers the "does this database exist" probe from a control file.
set -u
printf 'ARGS: %s\n' "$*" >> "$OTDEL_STUB_PSQL_LOG"
is_probe=0
has_command=0
for arg in "$@"; do
    [[ "$arg" == "-c" ]] && has_command=1
    [[ "$arg" == *pg_database* ]] && is_probe=1
    [[ "$arg" == -* || "$arg" == *pg_database* ]] || true
done
for arg in "$@"; do
    case "$arg" in SELECT*|CREATE*|DROP*|GRANT*|REVOKE*) printf 'SQL: %s\n' "$arg" >> "$OTDEL_STUB_PSQL_LOG" ;; esac
done
if [[ "$has_command" -eq 0 ]]; then
    # Statements arrive on stdin as a heredoc.
    while IFS= read -r line; do printf 'SQL: %s\n' "$line" >> "$OTDEL_STUB_PSQL_LOG"; done
fi
if [[ "$is_probe" -eq 1 ]]; then
    cat "$OTDEL_STUB_EXISTS_ANSWER"
fi
exit 0
STUB

cat > "${stub_bin}/cargo" <<'STUB'
#!/usr/bin/env bash
# Stands in for cargo. Records the subcommand and the database URLs it was handed, so the
# test can prove migrations are pointed at the test database and never at the pilot one.
set -u
printf 'ARGS: %s\n' "$*" >> "$OTDEL_STUB_CARGO_LOG"
printf 'RUNTIME: %s\n' "${OTDEL_DATABASE_URL:-unset}" >> "$OTDEL_STUB_CARGO_LOG"
printf 'ADMIN: %s\n' "${OTDEL_DATABASE_ADMIN_URL:-unset}" >> "$OTDEL_STUB_CARGO_LOG"
exit 0
STUB

chmod +x "${stub_bin}/fake-compose" "${stub_bin}/cargo"

# run_provisioning <exists: 1|empty> [VAR=value ...] -- <args...>
run_provisioning() {
    printf '%s' "$1" > "$exists_answer"
    shift
    : > "$psql_log"
    : > "$cargo_log"
    local -a assignments=()
    while [[ $# -gt 0 && "$1" != "--" ]]; do
        assignments[${#assignments[@]}]="$1"
        shift
    done
    shift || true
    env -u OTDEL_TEST_DB_NAME -u OTDEL_TEST_DB_HOST_PORT \
        "PATH=${stub_bin}:${PATH}" \
        "COMPOSE=${stub_bin}/fake-compose" \
        "OTDEL_STUB_PSQL_LOG=${psql_log}" \
        "OTDEL_STUB_CARGO_LOG=${cargo_log}" \
        "OTDEL_STUB_EXISTS_ANSWER=${exists_answer}" \
        "OTDEL_ENV_FILE=${fixtures}/otdel.env" \
        "OTDEL_DB_ENV_FILE=${fixtures}/db.env" \
        ${assignments[@]+"${assignments[@]}"} \
        bash "$script" "$@" \
        > "$stdout_file" 2> "$stderr_file"
    status=$?
    return 0
}

# 9a. the database is missing: it is created, then migrated.
run_provisioning "" OTDEL_TEST_DB_NAME=otdel_r01_fresh -- --export
expect_success "provisions a missing database"
grep -q 'SQL: CREATE DATABASE otdel_r01_fresh OWNER otdel_migrator;' "$psql_log"
check "a missing database is created with the migration role as owner" $?
! grep -q 'DROP' "$psql_log"
check "creating a missing database issues no DROP of any kind" $?
stdout_is_pure_exports
check "stdout stays pure shell while the database is being created" $?
grep -q 'creating the' "$stderr_file"
check "the 'creating the database' status line goes to stderr" $?

# 9b. the database already exists: it is reused, never recreated.
run_provisioning "1" OTDEL_TEST_DB_NAME=otdel_r01_fresh -- --export
expect_success "reuses an existing database"
! grep -qE 'SQL: (CREATE|DROP) DATABASE' "$psql_log"
check "an existing database is neither recreated nor dropped" $?
grep -q 'already exists; reusing it' "$stderr_file"
check "reuse is reported on stderr" $?

# 9c. migrations are pointed at the test database, never at the pilot one.
grep -q 'ARGS: run --quiet --bin otdel-api -- migrate' "$cargo_log"
check "migrate runs against the provisioned database" $?
grep -q 'ARGS: run --quiet --bin otdel-api -- bootstrap' "$cargo_log"
check "bootstrap runs against the provisioned database" $?
[[ "$(grep -c '/otdel_r01_fresh$' "$cargo_log")" == "4" ]]
check "both cargo invocations get both URLs pointed at the test database" $?
! grep -qE '(RUNTIME|ADMIN): .*/otdel$' "$cargo_log"
check "no cargo invocation is ever handed the pilot database URL" $?

# 9d. a rejected name never reaches docker, psql or cargo at all.
run_provisioning "" 'OTDEL_TEST_DB_NAME=otdel_test; DROP DATABASE otdel; --' -- --export
expect_rejected "an injected name is rejected before any command runs"
[[ ! -s "$psql_log" && ! -s "$cargo_log" ]]
check "a rejected name reaches neither psql nor cargo" $?

# 9e. the pilot database is never the write target. Run against the create path, where
# there really are CREATE/GRANT/REVOKE statements to inspect — asserting this on the
# reuse path would pass vacuously.
run_provisioning "" OTDEL_TEST_DB_NAME=otdel_r01_fresh -- --export
[[ "$(grep -cE '^SQL: (CREATE|GRANT|REVOKE)' "$psql_log")" -ge 5 ]]
check "the create path emits the expected ownership and privilege statements" $?
! grep -qE '^SQL: (CREATE|DROP|GRANT|REVOKE) .*[ ;]otdel([ ;]|$)' "$psql_log"
check "no statement in the provisioning path names the pilot database" $?

echo ""
printf '%d passed, %d failed\n' "$passed" "$failed"
[[ "$failed" -eq 0 ]]
