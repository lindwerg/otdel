#!/bin/bash
# Runs once, on first initialisation of the R05 live data directory.
#
# It creates the same two roles the pilot uses — so the live run exercises exactly the
# production privilege setup, row-level security included — and the single database this
# stack owns:
#
#   otdel_r05_live — the ONLY database this container serves application traffic from.
#
# There is deliberately no `otdel` and no `otdel_test` here: a mistyped connection URL
# should fail to connect, not quietly reach a database that looks like the pilot's.
#
# Passwords come from .local/r05-live/db.env and are never echoed.

set -euo pipefail

: "${OTDEL_MIGRATOR_PASSWORD:?OTDEL_MIGRATOR_PASSWORD must be set (see scripts/r05-live.sh init)}"
: "${OTDEL_APP_PASSWORD:?OTDEL_APP_PASSWORD must be set (see scripts/r05-live.sh init)}"

psql --quiet --no-psqlrc --set ON_ERROR_STOP=1 \
     --username "$POSTGRES_USER" --dbname postgres \
     --set migrator_password="$OTDEL_MIGRATOR_PASSWORD" \
     --set app_password="$OTDEL_APP_PASSWORD" <<'SQL'
CREATE ROLE otdel_migrator LOGIN PASSWORD :'migrator_password'
    NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS NOINHERIT;

CREATE ROLE otdel_app LOGIN PASSWORD :'app_password'
    NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS NOINHERIT;

CREATE DATABASE otdel_r05_live OWNER otdel_migrator;

REVOKE ALL ON DATABASE otdel_r05_live FROM PUBLIC;
GRANT CONNECT ON DATABASE otdel_r05_live TO otdel_migrator, otdel_app;

ALTER ROLE otdel_app SET search_path = otdel, public;
ALTER ROLE otdel_migrator SET search_path = otdel, public;
SQL

psql --quiet --no-psqlrc --set ON_ERROR_STOP=1 \
     --username "$POSTGRES_USER" --dbname otdel_r05_live <<'SQL'
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO otdel_migrator;
SQL

echo "otdel-r05-live: created roles and database otdel_r05_live"
