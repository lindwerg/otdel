-- OTDEL block 1, phase 1F — updates, the history of what happened, and retention.
--
-- Applied by the *migration* role. Migrations 0001–0006 are frozen (they have been
-- applied to the pilot database), so everything here is additive.
--
-- Phase 1E made a version immutable. This phase makes the *cycle* around it honest:
-- a new document has to be able to start a new cycle without touching what is already
-- published, the owner has to be able to see why the published version is no longer the
-- whole truth, and the operational history of all of it has to survive the rows it
-- describes.
--
-- Four things are added, and each exists because something in 1A–1E could not answer a
-- question that `block-01-plan.md` §1F asks:
--
--  1. **An append-only event log.** Every "run" table in this system is one row per
--     scope, rewritten in place: `validation_runs` has `UNIQUE (partner_id)` and
--     re-arming it zeroes the previous counters, a job's `error` is overwritten by the
--     next failure, and `attempts` is a counter rather than a history. That is the right
--     shape for *current state* and the wrong shape for "что и почему изменилось". The
--     event log is the second shape: it is written once, never updated, and removed only
--     by a retention pass that says in the log itself that it ran.
--
--  2. **A candidate fingerprint on a version.** 1E's `input_fingerprint` includes each
--     claim's *verdict*, which is only known after the checker has re-read every source.
--     That makes it exactly right for "do not publish the same thing twice" and useless
--     for "does the published version still match what the partner's documents say?",
--     which has to be answerable without running a check. The candidate fingerprint
--     covers the candidates alone, so the comparison is one string against one string.
--
--  3. **A content revision on a material, and the revision each draft was made from.**
--     Re-reading a document rewrites `material_pages.text_content` in place. Before this
--     column there was no way to say "this draft was made from an older reading of this
--     file" — only "the file's timestamp moved", which is also true when nothing changed.
--
--  4. **Retention as a database function, not as an application loop.** The runtime role
--     has no DELETE privilege on the event log or on the queue, and it does not get one
--     here. Removal happens inside a `SECURITY DEFINER` function with a floor built into
--     it, which is what makes "retention cannot erase this week's history" a property of
--     the database rather than of whichever caller is currently correct.

-- --------------------------------------------------------------------------
-- 1. The event log
--
-- Append-only, bureau-scoped, and deliberately **not** a set of foreign keys.
--
-- `material_id`, `version_id`, `job_id` and `run_id` are stored as data. An event that
-- says "этот материал был перечитан" has to survive the material being removed, in the
-- same way and for the same reason that `version_evidence` copies its quotation instead
-- of pointing at a page row (`0006_publication.sql`). A cascade that silently erased the
-- record of a deletion would be the worst possible audit log: one that is complete only
-- while nothing has happened.
--
-- The two identifiers that *are* foreign keys are the ones that define the scope the log
-- belongs to. Erasing a partner erases their history with them — that is erasure, and it
-- is the one deletion this table accepts besides retention.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.events (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id   uuid        NOT NULL REFERENCES otdel.bureaus (id) ON DELETE CASCADE,
    -- NULL for an event about the whole bureau (a retention pass). Every other event
    -- names the partner whose cycle it belongs to.
    partner_id  uuid,
    -- A closed vocabulary. Widening it is a migration, on purpose: an event kind that
    -- can be invented at the call site is a log nobody can query.
    kind        text        NOT NULL CHECK (kind IN (
                                'material_uploaded',
                                'material_duplicate',
                                'material_reprocess_requested',
                                'material_extraction_finished',
                                'understanding_queued',
                                'understanding_finished',
                                'validation_queued',
                                'validation_finished',
                                'version_published',
                                'version_blocked',
                                'version_superseded',
                                'version_retracted',
                                'refresh_requested',
                                'export_read',
                                'job_failed',
                                'retention_applied')),
    -- Who caused it. `owner` is a request the signed-in owner made, `worker` is work the
    -- background process did, `system` is maintenance nobody asked for individually.
    -- There is exactly one human account in the local pilot, so `owner` is as precise as
    -- this can honestly be; a multi-user deployment would need a user id here and that is
    -- not something to invent before the accounts exist.
    actor       text        NOT NULL CHECK (actor IN ('owner', 'worker', 'system')),
    material_id uuid,
    version_id  uuid,
    job_id      uuid,
    run_id      uuid,
    -- The sentence the interface shows, verbatim. Written by the code that knows what
    -- happened, not assembled from an enum by the reader.
    summary     text        NOT NULL CHECK (char_length(btrim(summary)) BETWEEN 1 AND 1000),
    -- A small structured payload for the things a sentence cannot carry (counters, a
    -- version number, a fingerprint). Bounded, because an unbounded jsonb column in a
    -- table nobody prunes by row size is a disk-space incident waiting to happen.
    -- Secrets never reach it: nothing that writes here has access to one.
    detail      jsonb       NOT NULL DEFAULT '{}'::jsonb
                            CHECK (jsonb_typeof(detail) = 'object'
                                   AND pg_column_size(detail) <= 4000),
    occurred_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (bureau_id, partner_id)
        REFERENCES otdel.partners (bureau_id, id) ON DELETE CASCADE
);

-- The reading order of the log, and the only order the API offers: newest first within
-- one partner.
CREATE INDEX events_partner_idx
    ON otdel.events (bureau_id, partner_id, occurred_at DESC, id DESC);
-- Bureau-wide reads (the retention sweep's own record) and the retention pass itself.
CREATE INDEX events_bureau_time_idx ON otdel.events (bureau_id, occurred_at);

ALTER TABLE otdel.events ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.events FORCE ROW LEVEL SECURITY;
CREATE POLICY events_bureau_isolation ON otdel.events
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- Append-only, enforced for every writer including the schema owner.
--
-- UPDATE is refused outright: an audit record that can be edited records nothing.
--
-- DELETE is refused as well, with exactly two exceptions, and both are visible:
--
--   * a retention pass, which announces itself with a transaction-local setting that
--     only `otdel.apply_retention` sets, and which writes its own `retention_applied`
--     event saying how many rows it removed and up to which moment;
--   * a cascade from the partner or the bureau being deleted — by that point the parent
--     is already gone, so there is nothing to keep the history *of*.
-- --------------------------------------------------------------------------
CREATE FUNCTION otdel.refuse_event_change() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = otdel, pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP = 'UPDATE' THEN
        RAISE EXCEPTION
            'otdel.events is append-only: an event that can be edited records nothing'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;

    -- A retention pass. The setting is transaction-local and is set only by
    -- `otdel.apply_retention`, which is SECURITY DEFINER and carries its own floor.
    IF coalesce(current_setting('otdel.retention_pass', true), '') = 'on' THEN
        RETURN OLD;
    END IF;

    -- A cascade: the scope this event belonged to is being erased.
    IF OLD.partner_id IS NOT NULL
       AND NOT EXISTS (SELECT 1 FROM otdel.partners p WHERE p.id = OLD.partner_id) THEN
        RETURN OLD;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM otdel.bureaus b WHERE b.id = OLD.bureau_id) THEN
        RETURN OLD;
    END IF;

    RAISE EXCEPTION
        'otdel.events is append-only: an event is removed by the retention pass, which '
        'records that it ran, and never by a statement that leaves no trace'
        USING ERRCODE = 'integrity_constraint_violation';
END
$$;

CREATE TRIGGER events_append_only
    BEFORE UPDATE OR DELETE ON otdel.events
    FOR EACH ROW EXECUTE FUNCTION otdel.refuse_event_change();

-- --------------------------------------------------------------------------
-- 2. What a version was built from, without its verdicts
--
-- `input_fingerprint` (0006) answers "is this the same knowledge, checked the same way".
-- It cannot answer "have the partner's documents moved on since we published", because
-- computing it requires re-reading every cited source — which is the check itself.
--
-- `candidate_fingerprint` covers the candidate rows alone: their origin, product,
-- property, value, unit, conditions and quotations. Comparing today's candidates with it
-- is a cheap read, so `GET /api/partners/{id}/refresh` can tell the owner that a new
-- check would produce something different **before** anybody runs one.
--
-- NULL is a real state and is reported as one: a version published by phase 1E has no
-- candidate fingerprint, and inventing one now by hashing today's candidates would say
-- "unchanged" about a version built from something nobody recorded.
-- --------------------------------------------------------------------------
ALTER TABLE otdel.knowledge_versions
    ADD COLUMN candidate_fingerprint text
        CHECK (candidate_fingerprint IS NULL OR candidate_fingerprint ~ '^[0-9a-f]{64}$');

-- The identity trigger of 0006, widened to freeze the new column too. Replacing the
-- function keeps the existing trigger; the body is 0006's with one more comparison, and
-- the comments there still apply in full.
CREATE OR REPLACE FUNCTION otdel.refuse_version_identity_change() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = otdel, pg_catalog, pg_temp
AS $$
BEGIN
    IF NEW.partner_id IS DISTINCT FROM OLD.partner_id
       OR NEW.bureau_id IS DISTINCT FROM OLD.bureau_id
       OR NEW.number IS DISTINCT FROM OLD.number
       OR NEW.input_fingerprint IS DISTINCT FROM OLD.input_fingerprint
       OR NEW.candidate_fingerprint IS DISTINCT FROM OLD.candidate_fingerprint
       OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION
            'knowledge version % is immutable: partner, number, fingerprints and creation '
            'time cannot be changed',
            OLD.id
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;

    IF OLD.published_at IS NOT NULL AND NEW.status IN ('draft', 'validating') THEN
        RAISE EXCEPTION
            'knowledge version % has been published and cannot return to `%`',
            OLD.id, NEW.status
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;

    -- `revoked` and `superseded` stay terminal (0006: "откат не воскрешает отозванные
    -- источники").
    IF OLD.status IN ('revoked', 'superseded') AND NEW.status IS DISTINCT FROM OLD.status THEN
        RAISE EXCEPTION
            'knowledge version % is `%` and that is final; run a new check to publish again',
            OLD.id, OLD.status
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;

    IF OLD.published_at IS NOT NULL AND NEW.published_at IS DISTINCT FROM OLD.published_at THEN
        RAISE EXCEPTION
            'knowledge version % already has a publication time', OLD.id
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;

    RETURN NEW;
END
$$;

-- --------------------------------------------------------------------------
-- 3. How many times a document has been read, and which reading a draft used
--
-- `content_revision` counts completed *readings*, not uploads: a changed file is always a
-- new material row (dedup is by `partner_id` + SHA-256), so the only way the text under a
-- fixed material id changes is a re-read. It is incremented when a reading starts, so a
-- draft made during a re-read cannot claim the revision that re-read will produce.
--
-- Existing pilot rows that have already been read are set to 1 rather than left at 0:
-- "read zero times" about a document with 32 extracted pages would be a lie told by a
-- default value.
-- --------------------------------------------------------------------------
ALTER TABLE otdel.materials
    ADD COLUMN content_revision integer NOT NULL DEFAULT 0
        CHECK (content_revision >= 0);

UPDATE otdel.materials
   SET content_revision = 1
 WHERE extraction_finished_at IS NOT NULL;

-- Which reading the product role drafted from. NULL for runs that happened before this
-- column existed — reported as "неизвестно", never as "актуально".
ALTER TABLE otdel.knowledge_runs
    ADD COLUMN source_revision integer CHECK (source_revision IS NULL OR source_revision >= 0);

-- --------------------------------------------------------------------------
-- 4. Retention
--
-- What may be removed, and what may not, is the whole design:
--
--   * **A published version is never removed by retention.** Not when it is superseded,
--     not when it is retracted. `0006_publication.sql` refuses to delete a version that
--     was ever published, and this phase does not add an exception — a retention policy
--     that quietly erased the snapshot somebody's answer cited would make every citation
--     in this system conditional. Old versions are history and history is the point.
--
--   * **The event log and the queue are operational records and are prunable**, but only
--     past a floor. `p_keep_days` below 1 is refused, so "clean up" can never mean
--     "delete what just happened", and the pass writes its own event describing what it
--     did — the one record it is forbidden from removing, because it is newer than the
--     horizon it just applied.
--
--   * **A job row is removed only when it is finished.** A `queued` or `running` job is
--     work, not history. The last `p_keep_per_kind` finished jobs of each kind are kept
--     regardless of age, so a partner whose documents were all processed months ago does
--     not end up with an empty processing history.
--
-- Both functions are SECURITY DEFINER because the runtime role deliberately has no
-- DELETE privilege on either table: removal has to go through a function that carries the
-- floor with it. They are still bureau-scoped — these tables FORCE row-level security, so
-- the policy applies to the owner as well — and the explicit bureau argument is checked
-- against the transaction's context so a caller cannot prune somebody else's history by
-- passing a different id.
-- --------------------------------------------------------------------------
CREATE FUNCTION otdel.apply_retention(
    p_bureau_id      uuid,
    p_event_days     integer,
    p_job_days       integer,
    p_keep_per_kind  integer
) RETURNS TABLE (events_removed bigint, jobs_removed bigint)
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = otdel, pg_catalog, pg_temp
AS $$
DECLARE
    v_events bigint := 0;
    v_jobs   bigint := 0;
BEGIN
    IF p_bureau_id IS NULL OR p_bureau_id IS DISTINCT FROM otdel.current_bureau_id() THEN
        RAISE EXCEPTION
            'retention runs inside the caller''s own bureau context and nowhere else';
    END IF;
    IF p_event_days IS NULL OR p_event_days < 1
       OR p_job_days IS NULL OR p_job_days < 1 THEN
        RAISE EXCEPTION
            'retention horizons are in whole days and must be at least 1: a policy that '
            'removes what happened today is not a retention policy';
    END IF;
    -- The queue may not outlive the log that records what its jobs did. Reversing this
    -- would leave finished jobs with no surviving explanation of why they failed.
    IF p_job_days > p_event_days THEN
        RAISE EXCEPTION
            'the job horizon (% days) must not exceed the event horizon (% days): the '
            'log is what remains after a job row is gone',
            p_job_days, p_event_days;
    END IF;
    IF p_keep_per_kind IS NULL OR p_keep_per_kind < 0 THEN
        RAISE EXCEPTION 'p_keep_per_kind must not be negative';
    END IF;

    WITH prunable AS (
        SELECT j.id
          FROM otdel.jobs j
         WHERE j.bureau_id = p_bureau_id
           AND j.status IN ('completed', 'failed', 'cancelled')
           AND j.updated_at < now() - make_interval(days => p_job_days)
           AND j.id NOT IN (
               -- The most recent finished jobs of each kind, per partner, stay whatever
               -- their age: an empty processing history is indistinguishable from a
               -- partner nothing ever happened to.
               SELECT k.id FROM (
                   SELECT r.id,
                          row_number() OVER (PARTITION BY r.partner_id, r.kind
                                             ORDER BY r.updated_at DESC, r.id DESC) AS rank
                     FROM otdel.jobs r
                    WHERE r.bureau_id = p_bureau_id
                      AND r.status IN ('completed', 'failed', 'cancelled')
               ) k
                WHERE k.rank <= p_keep_per_kind
           )
    ), removed AS (
        DELETE FROM otdel.jobs j USING prunable p WHERE j.id = p.id RETURNING j.id
    )
    SELECT count(*) INTO v_jobs FROM removed;

    -- The event log last, and behind its own flag. Everything above this line is still
    -- described by events that are about to be considered for removal, which is why the
    -- horizons are ordered rather than independent.
    PERFORM set_config('otdel.retention_pass', 'on', true);
    WITH removed AS (
        DELETE FROM otdel.events e
         WHERE e.bureau_id = p_bureau_id
           AND e.occurred_at < now() - make_interval(days => p_event_days)
        RETURNING e.id
    )
    SELECT count(*) INTO v_events FROM removed;
    PERFORM set_config('otdel.retention_pass', 'off', true);

    RETURN QUERY SELECT v_events, v_jobs;
END
$$;

-- What a retention pass *would* remove, without removing it. The interface shows this
-- next to the policy so "включить хранение 90 дней" is not a button whose effect is only
-- visible afterwards.
CREATE FUNCTION otdel.retention_preview(
    p_bureau_id      uuid,
    p_event_days     integer,
    p_job_days       integer,
    p_keep_per_kind  integer
) RETURNS TABLE (events_prunable bigint, jobs_prunable bigint,
                 events_total bigint, jobs_total bigint,
                 oldest_event timestamptz)
    LANGUAGE plpgsql
    STABLE
    SECURITY DEFINER
    SET search_path = otdel, pg_catalog, pg_temp
AS $$
BEGIN
    IF p_bureau_id IS NULL OR p_bureau_id IS DISTINCT FROM otdel.current_bureau_id() THEN
        RAISE EXCEPTION
            'retention runs inside the caller''s own bureau context and nowhere else';
    END IF;

    RETURN QUERY
    SELECT
        (SELECT count(*) FROM otdel.events e
          WHERE e.bureau_id = p_bureau_id
            AND p_event_days IS NOT NULL AND p_event_days >= 1
            AND e.occurred_at < now() - make_interval(days => p_event_days)),
        (SELECT count(*) FROM otdel.jobs j
          WHERE j.bureau_id = p_bureau_id
            AND p_job_days IS NOT NULL AND p_job_days >= 1
            AND j.status IN ('completed', 'failed', 'cancelled')
            AND j.updated_at < now() - make_interval(days => p_job_days)
            AND j.id NOT IN (
                SELECT k.id FROM (
                    SELECT r.id,
                           row_number() OVER (PARTITION BY r.partner_id, r.kind
                                              ORDER BY r.updated_at DESC, r.id DESC) AS rank
                      FROM otdel.jobs r
                     WHERE r.bureau_id = p_bureau_id
                       AND r.status IN ('completed', 'failed', 'cancelled')
                ) k
                 WHERE k.rank <= coalesce(p_keep_per_kind, 0)
            )),
        (SELECT count(*) FROM otdel.events e WHERE e.bureau_id = p_bureau_id),
        (SELECT count(*) FROM otdel.jobs j WHERE j.bureau_id = p_bureau_id),
        (SELECT min(e.occurred_at) FROM otdel.events e WHERE e.bureau_id = p_bureau_id);
END
$$;

-- --------------------------------------------------------------------------
-- Privileges for the runtime role
--
-- SELECT and INSERT on the log, and nothing else. The application appends and reads; it
-- cannot rewrite and it cannot delete. Pruning is the two functions above, whose floor
-- the application cannot argue with.
-- --------------------------------------------------------------------------
GRANT SELECT, INSERT ON otdel.events TO otdel_app;

REVOKE ALL ON FUNCTION otdel.refuse_event_change() FROM PUBLIC;
REVOKE ALL ON FUNCTION otdel.apply_retention(uuid, integer, integer, integer) FROM PUBLIC;
REVOKE ALL ON FUNCTION otdel.retention_preview(uuid, integer, integer, integer) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION otdel.refuse_event_change() TO otdel_app;
GRANT EXECUTE ON FUNCTION otdel.apply_retention(uuid, integer, integer, integer) TO otdel_app;
GRANT EXECUTE ON FUNCTION otdel.retention_preview(uuid, integer, integer, integer) TO otdel_app;
