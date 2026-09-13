#!/bin/bash
# Create the separate test database, on first initialisation of the data directory.
#
# `otdel`      — the pilot database. Real partner material lives here.
# `otdel_test` — integration tests only. The suite truncates and rewrites rows, so it
#                must never point at the pilot database.
#
# Both are owned by otdel_migrator and reachable by otdel_app with the same restricted
# privileges, so the tests exercise exactly the production role setup.
#
# `scripts/dev-test-db.sh` creates the same database idempotently for a volume that was
# initialised before this script existed.

set -euo pipefail

psql --quiet --no-psqlrc --set ON_ERROR_STOP=1 \
     --username "$POSTGRES_USER" --dbname otdel <<'SQL'
CREATE DATABASE otdel_test OWNER otdel_migrator;
REVOKE ALL ON DATABASE otdel_test FROM PUBLIC;
GRANT CONNECT ON DATABASE otdel_test TO otdel_migrator, otdel_app;
SQL

psql --quiet --no-psqlrc --set ON_ERROR_STOP=1 \
     --username "$POSTGRES_USER" --dbname otdel_test <<'SQL'
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO otdel_migrator;
SQL

echo "otdel: created the separate test database otdel_test"
