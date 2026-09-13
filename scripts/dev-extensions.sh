#!/usr/bin/env bash
# Install the PostgreSQL extensions phase 1E can use, as the container superuser.
#
# Why this is a separate script and not part of a migration:
#
#   `CREATE EXTENSION vector` requires a superuser — pgvector does not mark itself
#   `trusted` — and `otdel_migrator` is deliberately NOSUPERUSER (`infra/postgres/initdb/
#   10-roles.sh`). Migration `0006_publication.sql` therefore *tries* and carries on
#   without the vector column when it may not, so the exact-value and full-text halves of
#   search keep working on a database nobody has run this against.
#
# Why the extension goes into the `otdel` schema:
#
#   `0001_schema.sql` revokes everything on `public` and grants `USAGE` there to the
#   migration role only. A `vector` type created in `public` is therefore invisible to
#   `otdel_app`: the column exists and every statement naming its type fails with
#   `type "vector" does not exist`. Installing it into `otdel`, which the runtime role
#   already uses, is what makes semantic search actually reachable. A database that
#   already has it in `public` keeps working too — the application resolves the schema
#   from the catalogue and reports the honest state when it cannot reach it.
#
# Usage:
#   scripts/dev-extensions.sh          both the pilot and the test database
#   make db-extensions
#
# Afterwards run `make migrate` (and `scripts/dev-test-db.sh`) so migration 0006 adds the
# vector column. Re-applying migrations is idempotent.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ ! -f .local/db.env ]]; then
    echo "dev-extensions: .local/db.env is missing; run scripts/dev-init.sh first." >&2
    exit 1
fi

set -a
# shellcheck disable=SC1091
. ./.local/db.env
set +a

compose="${COMPOSE:-docker compose}"

for database in otdel otdel_test; do
    exists="$(
        $compose exec -T postgres \
            psql --quiet --no-psqlrc --tuples-only --no-align \
                 --username "$POSTGRES_USER" --dbname postgres \
                 -c "SELECT 1 FROM pg_database WHERE datname = '${database}'" 2>/dev/null || true
    )"
    if [[ "$(echo "$exists" | tr -d '[:space:]')" != "1" ]]; then
        echo "dev-extensions: ${database} does not exist yet; skipping."
        continue
    fi

    # The `otdel` schema is created by migration 0001. Installing into it before that has
    # run is not possible, and is not a failure worth stopping for: run this again after
    # `make migrate`.
    has_schema="$(
        $compose exec -T postgres \
            psql --quiet --no-psqlrc --tuples-only --no-align \
                 --username "$POSTGRES_USER" --dbname "$database" \
                 -c "SELECT 1 FROM pg_namespace WHERE nspname = 'otdel'" 2>/dev/null || true
    )"
    if [[ "$(echo "$has_schema" | tr -d '[:space:]')" != "1" ]]; then
        echo "dev-extensions: ${database} has no otdel schema yet; run make migrate first, then this."
        continue
    fi

    $compose exec -T postgres \
        psql --quiet --no-psqlrc --set ON_ERROR_STOP=1 \
             --username "$POSTGRES_USER" --dbname "$database" <<'SQL'
-- Idempotent. When the extension is already installed anywhere, this is a no-op that
-- does not move it: PostgreSQL ignores SCHEMA for an existing extension.
CREATE EXTENSION IF NOT EXISTS vector SCHEMA otdel;

-- Add the column migration 0006 could not.
--
-- 0006 adds `version_chunks.embedding` only if pgvector was already installed when it
-- ran, and a migration runs once. An operator who enables pgvector *afterwards* would
-- otherwise have the extension and no column for ever — the extension would be present
-- and semantic search still unavailable, which is the most confusing of the possible
-- states. Enabling pgvector is one operation, so this script performs all of it.
--
-- Idempotent and additive: it does nothing when the column is already there, and it
-- never touches a stored row.
DO $$
DECLARE
    v_schema text;
BEGIN
    SELECT n.nspname INTO v_schema
      FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace
     WHERE e.extname = 'vector';

    IF v_schema IS NULL THEN
        RETURN;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.tables
         WHERE table_schema = 'otdel' AND table_name = 'version_chunks'
    ) THEN
        RAISE NOTICE 'otdel.version_chunks does not exist yet; run make migrate, then this script again';
        RETURN;
    END IF;
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_schema = 'otdel' AND table_name = 'version_chunks'
           AND column_name = 'embedding'
    ) THEN
        RETURN;
    END IF;

    EXECUTE format('ALTER TABLE otdel.version_chunks ADD COLUMN embedding %I.vector', v_schema);
    EXECUTE format(
        'ALTER TABLE otdel.version_chunks ADD CONSTRAINT version_chunks_embedding_matches_dims '
        'CHECK (embedding IS NULL OR (embedding_dims IS NOT NULL '
        'AND %I.vector_dims(embedding) = embedding_dims))', v_schema);
    RAISE NOTICE 'added otdel.version_chunks.embedding';
END
$$;
SQL

    schema="$(
        $compose exec -T postgres \
            psql --quiet --no-psqlrc --tuples-only --no-align \
                 --username "$POSTGRES_USER" --dbname "$database" \
                 -c "SELECT n.nspname FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace WHERE e.extname = 'vector'"
    )"
    echo "dev-extensions: ${database}: pgvector installed in schema ${schema}."

    if [[ "$(echo "$schema" | tr -d '[:space:]')" == "public" ]]; then
        cat >&2 <<'WARN'
dev-extensions: WARNING — pgvector is in `public`, where the runtime role has no USAGE.
                Semantic search will report `extension_missing` and search will run
                keyword-only. To fix, as a superuser on that database:
                    DROP EXTENSION vector;   -- only if nothing uses it yet
                    CREATE EXTENSION vector SCHEMA otdel;
                or grant the runtime role access:
                    GRANT USAGE ON SCHEMA public TO otdel_app;
WARN
    fi
done

echo "dev-extensions: done. Run 'make migrate' so migration 0006 adds the vector column."
