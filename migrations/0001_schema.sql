-- OTDEL block 1, phase 1A — intake schema.
--
-- Applied by the *migration* role (owner of these objects), never by the runtime role.
-- The runtime role `otdel_app` gets only the privileges listed at the bottom of this
-- file and is subject to forced row-level security, so a bug in the application cannot
-- read another bureau's rows even with a hand-written query.

-- The runtime role must exist before the schema is created: the grants below are the
-- only thing that makes the application able to work at all, and silently skipping them
-- would produce a database that looks migrated but is not usable/isolated as intended.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'otdel_app') THEN
        RAISE EXCEPTION
            'runtime role "otdel_app" does not exist; create it before migrating '
            '(scripts/dev-init.sh + compose bootstrap, or docs/backend-1a.md for a manual database)';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'otdel_app' AND (rolsuper OR rolbypassrls)) THEN
        RAISE EXCEPTION
            'runtime role "otdel_app" must not be SUPERUSER or BYPASSRLS: tenant isolation '
            'relies on row-level security applying to it';
    END IF;
END
$$;

CREATE SCHEMA IF NOT EXISTS otdel;

-- Access context for row-level security. The server sets `otdel.bureau_id` with
-- `set_config(..., is_local => true)` inside each transaction; an unset context yields
-- NULL and therefore matches no row (fail closed).
CREATE FUNCTION otdel.current_bureau_id() RETURNS uuid
    LANGUAGE sql
    STABLE
    SET search_path = pg_catalog, pg_temp
AS $$
    SELECT nullif(current_setting('otdel.bureau_id', true), '')::uuid
$$;

-- --------------------------------------------------------------------------
-- Bureaus (workspaces). The local pilot has exactly one, seeded by 0002.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.bureaus (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    slug        text        NOT NULL UNIQUE CHECK (slug ~ '^[a-z0-9-]{1,64}$'),
    name        text        NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- The runtime role never touches this table directly (no grants below): it learns its
-- bureau only through the session functions. RLS is enabled anyway as a second wall.
ALTER TABLE otdel.bureaus ENABLE ROW LEVEL SECURITY;
CREATE POLICY bureaus_self_only ON otdel.bureaus
    USING (id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Partners
-- --------------------------------------------------------------------------
CREATE TABLE otdel.partners (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id   uuid        NOT NULL REFERENCES otdel.bureaus (id) ON DELETE RESTRICT,
    name        text        NOT NULL CHECK (char_length(btrim(name)) BETWEEN 1 AND 200),
    note        text        CHECK (note IS NULL OR char_length(note) <= 10000),
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    -- Referenced by the composite foreign keys below, which is what forces a material
    -- and its partner to belong to the same bureau at the database level.
    UNIQUE (bureau_id, id)
);

CREATE INDEX partners_bureau_created_idx ON otdel.partners (bureau_id, created_at DESC, id);

ALTER TABLE otdel.partners ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.partners FORCE ROW LEVEL SECURITY;
CREATE POLICY partners_bureau_isolation ON otdel.partners
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Materials (original uploads)
-- --------------------------------------------------------------------------
CREATE TABLE otdel.materials (
    id           uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id    uuid        NOT NULL,
    partner_id   uuid        NOT NULL,
    filename     text        NOT NULL CHECK (char_length(filename) BETWEEN 1 AND 200),
    media_type   text        NOT NULL CHECK (media_type IN ('application/pdf', 'image/png', 'image/jpeg')),
    size_bytes   bigint      NOT NULL CHECK (size_bytes > 0),
    sha256       text        NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    storage_key  text        NOT NULL CHECK (char_length(storage_key) BETWEEN 1 AND 512),
    status       text        NOT NULL DEFAULT 'queued'
                             CHECK (status IN ('queued', 'processing', 'completed', 'partial', 'failed', 'quarantined')),
    page_count   integer     CHECK (page_count IS NULL OR page_count >= 0),
    error        text        CHECK (error IS NULL OR char_length(error) <= 2000),
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    -- Deduplication is per partner: the same file uploaded to another partner is a
    -- separate original, a changed file is a new row (originals are never overwritten).
    UNIQUE (partner_id, sha256),
    UNIQUE (bureau_id, id),
    -- Referenced by the jobs foreign key: a job must point at a material that really
    -- belongs to both the bureau *and* the partner recorded on the job.
    UNIQUE (bureau_id, partner_id, id),
    FOREIGN KEY (bureau_id, partner_id) REFERENCES otdel.partners (bureau_id, id) ON DELETE RESTRICT
);

CREATE INDEX materials_partner_created_idx ON otdel.materials (partner_id, created_at DESC, id);

ALTER TABLE otdel.materials ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.materials FORCE ROW LEVEL SECURITY;
CREATE POLICY materials_bureau_isolation ON otdel.materials
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Jobs (durable queue). Phase 1A only enqueues; the 1B worker will lease and run.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.jobs (
    id                uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id         uuid        NOT NULL,
    partner_id        uuid        NOT NULL,
    material_id       uuid        NOT NULL,
    kind              text        NOT NULL CHECK (kind IN ('extract_document')),
    status            text        NOT NULL DEFAULT 'queued'
                                  CHECK (status IN ('queued', 'running', 'completed', 'failed', 'cancelled')),
    stage             text        CHECK (stage IS NULL OR char_length(stage) <= 100),
    attempts          integer     NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts      integer     NOT NULL DEFAULT 5 CHECK (max_attempts > 0),
    -- Stable per material+kind, so re-uploads and repeated retries cannot enqueue twice.
    idempotency_key   text        NOT NULL,
    run_after         timestamptz NOT NULL DEFAULT now(),
    lease_owner       text        CHECK (lease_owner IS NULL OR char_length(lease_owner) <= 200),
    lease_expires_at  timestamptz,
    error             text        CHECK (error IS NULL OR char_length(error) <= 2000),
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    UNIQUE (bureau_id, idempotency_key),
    -- One composite key, not separate partner/material keys: two independent foreign
    -- keys would still allow a job that names partner B while pointing at a material of
    -- partner A inside the same bureau. This shape makes that state unrepresentable.
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE RESTRICT,
    CHECK ((lease_owner IS NULL) = (lease_expires_at IS NULL))
);

CREATE INDEX jobs_partner_created_idx ON otdel.jobs (partner_id, created_at DESC, id);
-- Child-side index for the composite foreign key above (referential integrity checks
-- and the material→jobs lookups used by retry).
CREATE INDEX jobs_material_fk_idx ON otdel.jobs (bureau_id, partner_id, material_id);
CREATE INDEX jobs_runnable_idx ON otdel.jobs (run_after, id) WHERE status = 'queued';
CREATE INDEX jobs_lease_idx ON otdel.jobs (lease_expires_at) WHERE status = 'running';

ALTER TABLE otdel.jobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.jobs FORCE ROW LEVEL SECURITY;
CREATE POLICY jobs_bureau_isolation ON otdel.jobs
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Sessions
--
-- Only the SHA-256 fingerprint of the opaque session token is stored, so a database
-- dump cannot be replayed as a live session. The runtime role has **no** privileges on
-- this table: it can only call the SECURITY DEFINER functions below, which is also what
-- lets authentication happen before any bureau context exists.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.sessions (
    id                 uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id          uuid        NOT NULL REFERENCES otdel.bureaus (id) ON DELETE CASCADE,
    token_fingerprint  text        NOT NULL UNIQUE CHECK (token_fingerprint ~ '^[0-9a-f]{64}$'),
    csrf_token         text        NOT NULL CHECK (char_length(csrf_token) BETWEEN 20 AND 200),
    created_at         timestamptz NOT NULL DEFAULT now(),
    last_seen_at       timestamptz NOT NULL DEFAULT now(),
    expires_at         timestamptz NOT NULL
);

CREATE INDEX sessions_expires_idx ON otdel.sessions (expires_at);
-- Child-side index for the bureau foreign key (also makes “end every session of this
-- bureau” cheap once more than one bureau exists).
CREATE INDEX sessions_bureau_idx ON otdel.sessions (bureau_id);

ALTER TABLE otdel.sessions ENABLE ROW LEVEL SECURITY;

CREATE FUNCTION otdel.session_open(
    p_bureau_slug      text,
    p_token_fingerprint text,
    p_csrf_token       text,
    p_ttl_seconds      integer
) RETURNS TABLE (session_id uuid, bureau_id uuid, csrf_token text, expires_at timestamptz)
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = otdel, pg_catalog, pg_temp
AS $$
#variable_conflict use_column
DECLARE
    v_bureau_id uuid;
BEGIN
    IF p_ttl_seconds IS NULL OR p_ttl_seconds < 60 OR p_ttl_seconds > 604800 THEN
        RAISE EXCEPTION 'session ttl out of range';
    END IF;

    SELECT b.id INTO v_bureau_id FROM otdel.bureaus b WHERE b.slug = p_bureau_slug;
    IF v_bureau_id IS NULL THEN
        RAISE EXCEPTION 'bureau % is not provisioned', p_bureau_slug;
    END IF;

    RETURN QUERY
    WITH created AS (
        INSERT INTO otdel.sessions (bureau_id, token_fingerprint, csrf_token, expires_at)
        VALUES (v_bureau_id, p_token_fingerprint, p_csrf_token,
                now() + make_interval(secs => p_ttl_seconds))
        RETURNING id, bureau_id, csrf_token, expires_at
    )
    SELECT c.id, c.bureau_id, c.csrf_token, c.expires_at FROM created c;
END
$$;

-- Validates absolute expiry *and* idle timeout, and refreshes `last_seen_at`.
-- Returns no row when the session is unknown, expired or idle for too long.
CREATE FUNCTION otdel.session_touch(
    p_token_fingerprint text,
    p_idle_seconds      integer
) RETURNS TABLE (session_id uuid, bureau_id uuid, csrf_token text, expires_at timestamptz)
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = otdel, pg_catalog, pg_temp
AS $$
#variable_conflict use_column
BEGIN
    IF p_idle_seconds IS NULL OR p_idle_seconds < 60 THEN
        RAISE EXCEPTION 'idle timeout out of range';
    END IF;

    RETURN QUERY
    WITH touched AS (
        UPDATE otdel.sessions s
           SET last_seen_at = now()
         WHERE s.token_fingerprint = p_token_fingerprint
           AND s.expires_at > now()
           AND s.last_seen_at > now() - make_interval(secs => p_idle_seconds)
        RETURNING s.id, s.bureau_id, s.csrf_token, s.expires_at
    )
    SELECT t.id, t.bureau_id, t.csrf_token, t.expires_at FROM touched t;
END
$$;

-- Resolve the bureau of the local pilot without granting the runtime role access to the
-- bureaus table. Used by the maintenance worker (which has no session) and by tests.
CREATE FUNCTION otdel.bureau_id_by_slug(p_slug text) RETURNS uuid
    LANGUAGE sql
    STABLE
    SECURITY DEFINER
    SET search_path = otdel, pg_catalog, pg_temp
AS $$
    SELECT b.id FROM otdel.bureaus b WHERE b.slug = p_slug
$$;

CREATE FUNCTION otdel.session_close(p_token_fingerprint text) RETURNS integer
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = otdel, pg_catalog, pg_temp
AS $$
DECLARE
    v_removed integer;
BEGIN
    DELETE FROM otdel.sessions WHERE token_fingerprint = p_token_fingerprint;
    GET DIAGNOSTICS v_removed = ROW_COUNT;
    RETURN v_removed;
END
$$;

CREATE FUNCTION otdel.session_purge_expired(p_idle_seconds integer) RETURNS integer
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = otdel, pg_catalog, pg_temp
AS $$
DECLARE
    v_removed integer;
BEGIN
    IF p_idle_seconds IS NULL OR p_idle_seconds < 60 THEN
        RAISE EXCEPTION 'idle timeout out of range';
    END IF;

    DELETE FROM otdel.sessions
     WHERE expires_at <= now()
        OR last_seen_at <= now() - make_interval(secs => p_idle_seconds);
    GET DIAGNOSTICS v_removed = ROW_COUNT;
    RETURN v_removed;
END
$$;

-- --------------------------------------------------------------------------
-- Privileges for the runtime role: exactly what the API needs, nothing else.
-- No DELETE anywhere (1A never deletes tenant rows), no access to sessions or
-- bureaus tables, no CREATE on the schema.
-- --------------------------------------------------------------------------
REVOKE ALL ON SCHEMA otdel FROM PUBLIC;
GRANT USAGE ON SCHEMA otdel TO otdel_app;

GRANT SELECT, INSERT, UPDATE ON otdel.partners  TO otdel_app;
GRANT SELECT, INSERT, UPDATE ON otdel.materials TO otdel_app;
GRANT SELECT, INSERT, UPDATE ON otdel.jobs      TO otdel_app;

REVOKE ALL ON FUNCTION otdel.bureau_id_by_slug(text) FROM PUBLIC;
REVOKE ALL ON FUNCTION otdel.session_open(text, text, text, integer) FROM PUBLIC;
REVOKE ALL ON FUNCTION otdel.session_touch(text, integer) FROM PUBLIC;
REVOKE ALL ON FUNCTION otdel.session_close(text) FROM PUBLIC;
REVOKE ALL ON FUNCTION otdel.session_purge_expired(integer) FROM PUBLIC;

GRANT EXECUTE ON FUNCTION otdel.current_bureau_id() TO otdel_app;
GRANT EXECUTE ON FUNCTION otdel.bureau_id_by_slug(text) TO otdel_app;
GRANT EXECUTE ON FUNCTION otdel.session_open(text, text, text, integer) TO otdel_app;
GRANT EXECUTE ON FUNCTION otdel.session_touch(text, integer) TO otdel_app;
GRANT EXECUTE ON FUNCTION otdel.session_close(text) TO otdel_app;
GRANT EXECUTE ON FUNCTION otdel.session_purge_expired(integer) TO otdel_app;
