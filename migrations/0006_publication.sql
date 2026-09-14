-- OTDEL block 1, phase 1E — the checker, the immutable knowledge version, and the
-- searchable snapshot other agents may read.
--
-- Applied by the *migration* role. Migrations 0001–0005 are frozen (they have been
-- applied to the pilot database), so everything here is additive and lives in its own
-- file. The single exception is `otdel.jobs`, which every phase so far has widened in
-- its own file, and which is widened here again (see "the queue" at the bottom).
--
-- Properties enforced by the schema itself rather than left to the application:
--
--  1. **A published version is immutable.** Its claims, its citations, its gaps and its
--     readiness cannot be updated at all, and cannot be deleted while the version they
--     belong to exists. The runtime role is not granted UPDATE or DELETE on any of them,
--     and a trigger refuses both anyway — for every writer, including a later phase.
--  2. **A partner has at most one published version, always.** A partial unique index
--     says so, so the "atomic published pointer" of `block-01-spec.md` §7 is a property
--     of the database rather than of the order two statements happen to run in.
--  3. **A version's identity never changes.** Its partner, its number and the
--     fingerprint of the input it was built from are frozen by a trigger; only the
--     status and the lifecycle timestamps may move (`draft → validating → published →
--     superseded | revoked`).
--  4. **A claim without a citation is not stored** — the deferred trigger of 0004/0005
--     applied to the snapshot, on INSERT *and* UPDATE.
--  5. **The snapshot owns its quotations.** Evidence copies the quote, the filename and
--     the offsets instead of referencing the page row, and deliberately has **no foreign
--     key to `materials`**: a cascade from a deleted material must not be able to remove
--     a claim from a version that was already published.
--  6. **An industry claim can never name a partner's product**, in the snapshot exactly
--     as in `research_findings` (`block-01-plan.md`, 1D §4) — there is no column for it
--     when the scope is `industry`.
--  7. **A blocked version has to say why.** `status = 'blocked'` requires a non-empty
--     `blocked_reasons`, and a revoked one requires a reason and a timestamp.
--  8. **pgvector is optional and honestly so.** The extension is not `trusted`, so the
--     restricted migration role cannot create it; this file therefore *tries*, and
--     carries on without the vector column when it may not. Nothing in the search path
--     references that column unless it exists.

-- --------------------------------------------------------------------------
-- pgvector, if this role is allowed to have it.
--
-- `CREATE EXTENSION vector` requires a superuser (the extension does not mark itself
-- trusted), and `otdel_migrator` is deliberately NOSUPERUSER. Two things follow, and
-- both are handled here rather than by asking the operator to remember them:
--
--   * when a superuser has already installed the extension (the compose image ships it;
--     `scripts/dev-extensions.sh` installs it), `CREATE EXTENSION IF NOT EXISTS` is a
--     no-op that needs no privilege, and the vector column is added;
--   * when nobody has, this migration must still apply. Semantic search is then
--     genuinely unavailable, `GET /api/retrieval/provider` reports
--     `vector.state = "extension_missing"`, and search runs keyword-only and says so.
--
-- Failing the migration instead would take away the exact-value and full-text halves of
-- search, which need no extension at all, in order to punish the absence of the half
-- that does.
-- --------------------------------------------------------------------------
DO $$
BEGIN
    BEGIN
        CREATE EXTENSION IF NOT EXISTS vector;
    EXCEPTION
        WHEN insufficient_privilege THEN
            RAISE NOTICE 'otdel: pgvector is not installed and this role may not install '
                         'it; semantic search stays unavailable and the API reports that. '
                         'Run scripts/dev-extensions.sh (or CREATE EXTENSION vector as a '
                         'superuser) and re-apply to enable it.';
        WHEN undefined_file THEN
            RAISE NOTICE 'otdel: this PostgreSQL server does not provide pgvector; '
                         'semantic search stays unavailable and the API reports that.';
    END;
END
$$;

-- --------------------------------------------------------------------------
-- Validation runs
--
-- One row per partner: the current state of the checker over that partner's candidates.
-- The history of attempts lives on the job row (`attempts`, `error`, `error_kind`), the
-- same way 1C keeps it for understanding runs.
--
-- There is no `needs_provider` status here, and its absence is the point. Verification
-- is deterministic: it re-reads the stored page text and the stored snapshots, checks
-- the quotations by offset, and decides. A model may add a second opinion when one is
-- configured (`model_reviewed` counts how often it did), but it can only raise doubt,
-- never grant support — "совпадение ответов двух моделей не является доказательством"
-- (`block-01-spec.md` §6.7). So the checker runs with no key, and publication does too.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.validation_runs (
    id                uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id         uuid        NOT NULL,
    partner_id        uuid        NOT NULL,
    status            text        NOT NULL DEFAULT 'queued'
                                  CHECK (status IN ('queued', 'running', 'completed',
                                                    'partial', 'failed')),
    prompt_profile    text        NOT NULL
                                  CHECK (char_length(btrim(prompt_profile)) BETWEEN 1 AND 100),
    -- The version this run produced, published or blocked. NULL while the run is still
    -- working, or when it failed before building one.
    version_id        uuid,
    claims_considered integer     NOT NULL DEFAULT 0 CHECK (claims_considered >= 0),
    claims_rejected   integer     NOT NULL DEFAULT 0 CHECK (claims_rejected >= 0),
    gaps_carried      integer     NOT NULL DEFAULT 0 CHECK (gaps_carried >= 0),
    chunks_created    integer     NOT NULL DEFAULT 0 CHECK (chunks_created >= 0),
    chunks_embedded   integer     NOT NULL DEFAULT 0 CHECK (chunks_embedded >= 0),
    -- How many claims a model looked at. 0 is the normal state without a key and does
    -- not lower the run's status.
    model_reviewed    integer     NOT NULL DEFAULT 0 CHECK (model_reviewed >= 0),
    published         boolean     NOT NULL DEFAULT false,
    rejections        text[]      NOT NULL DEFAULT '{}'
                                  CHECK (array_length(rejections, 1) IS NULL
                                         OR array_length(rejections, 1) <= 100),
    blocked_reasons   text[]      NOT NULL DEFAULT '{}'
                                  CHECK (array_length(blocked_reasons, 1) IS NULL
                                         OR array_length(blocked_reasons, 1) <= 100),
    diagnostic        text        CHECK (diagnostic IS NULL OR char_length(diagnostic) <= 2000),
    started_at        timestamptz,
    finished_at       timestamptz,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    -- One run row per partner: pressing "проверить" twice reuses it.
    UNIQUE (partner_id),
    UNIQUE (bureau_id, id),
    FOREIGN KEY (bureau_id, partner_id)
        REFERENCES otdel.partners (bureau_id, id) ON DELETE CASCADE
);

CREATE INDEX validation_runs_partner_idx
    ON otdel.validation_runs (bureau_id, partner_id);

ALTER TABLE otdel.validation_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.validation_runs FORCE ROW LEVEL SECURITY;
CREATE POLICY validation_runs_bureau_isolation ON otdel.validation_runs
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Knowledge versions
--
-- The header of one immutable snapshot. `block-01-spec.md` §7:
-- `draft → validating → published | blocked`, and a published version may later become
-- `superseded` or `revoked`.
--
-- `input_fingerprint` is what makes "старый завершившийся запуск не может перезаписать
-- более новую входную ревизию" checkable rather than hoped for: it is a hash over the
-- candidates that went in, and publication refuses a fingerprint that is not newer than
-- the one already published.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.knowledge_versions (
    id                uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id         uuid        NOT NULL,
    partner_id        uuid        NOT NULL,
    -- Sequential per partner, from 1. Never reused: a number that came back would make
    -- two different snapshots share a name.
    number            integer     NOT NULL CHECK (number >= 1),
    status            text        NOT NULL DEFAULT 'draft'
                                  CHECK (status IN ('draft', 'validating', 'published',
                                                    'blocked', 'superseded', 'revoked')),
    validation_run_id uuid,
    -- SHA-256 over the candidate set this version was built from.
    input_fingerprint text        NOT NULL CHECK (input_fingerprint ~ '^[0-9a-f]{64}$'),
    -- Identifier of the embedding profile whose vectors this version carries, or NULL
    -- when it has none. Profiles are never mixed in one space (`block-01-spec.md` §9).
    embedding_profile text        CHECK (embedding_profile IS NULL
                                         OR char_length(btrim(embedding_profile)) BETWEEN 1 AND 200),
    blocked_reasons   text[]      NOT NULL DEFAULT '{}'
                                  CHECK (array_length(blocked_reasons, 1) IS NULL
                                         OR array_length(blocked_reasons, 1) <= 100),
    revoked_reason    text        CHECK (revoked_reason IS NULL
                                         OR char_length(btrim(revoked_reason)) BETWEEN 1 AND 1000),
    created_at        timestamptz NOT NULL DEFAULT now(),
    published_at      timestamptz,
    superseded_at     timestamptz,
    revoked_at        timestamptz,
    UNIQUE (partner_id, number),
    UNIQUE (bureau_id, id),
    -- Widened key: the snapshot tables below reference (bureau_id, partner_id, id), so a
    -- claim cannot belong to a version of a different partner even inside one bureau.
    UNIQUE (bureau_id, partner_id, id),
    FOREIGN KEY (bureau_id, partner_id)
        REFERENCES otdel.partners (bureau_id, id) ON DELETE CASCADE,
    -- A version that reached any published state has a publication time.
    CONSTRAINT knowledge_versions_published_has_time
        CHECK (status NOT IN ('published', 'superseded', 'revoked') OR published_at IS NOT NULL),
    CONSTRAINT knowledge_versions_superseded_has_time
        CHECK (status <> 'superseded' OR superseded_at IS NOT NULL),
    -- A retraction without a stated reason is indistinguishable from a malfunction.
    CONSTRAINT knowledge_versions_revoked_has_reason
        CHECK (status <> 'revoked' OR (revoked_at IS NOT NULL AND revoked_reason IS NOT NULL)),
    -- "Честно остаются неполными": a version that did not pass the rules must carry the
    -- rules it failed, in words.
    CONSTRAINT knowledge_versions_blocked_has_reasons
        CHECK (status <> 'blocked' OR array_length(blocked_reasons, 1) IS NOT NULL)
);

-- The atomic published pointer of `block-01-spec.md` §7. Two workers publishing at the
-- same moment do not race for it: the second one's INSERT/UPDATE fails on this index.
CREATE UNIQUE INDEX knowledge_versions_published_idx
    ON otdel.knowledge_versions (bureau_id, partner_id)
    WHERE status = 'published';

CREATE INDEX knowledge_versions_partner_idx
    ON otdel.knowledge_versions (bureau_id, partner_id, number DESC);

ALTER TABLE otdel.knowledge_versions ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.knowledge_versions FORCE ROW LEVEL SECURITY;
CREATE POLICY knowledge_versions_bureau_isolation ON otdel.knowledge_versions
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- The snapshot: claims
--
-- A copy, not a view. A claim keeps the wording, the value, the unit, the conditions and
-- the checker's verdict as they were when the version was built; re-running 1C over the
-- material afterwards changes the candidate and leaves this row exactly as published.
--
-- `origin_id` points back at the candidate for traceability only. It is deliberately not
-- a foreign key: deleting a candidate must not be able to alter a published version.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.version_claims (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    partner_id    uuid        NOT NULL,
    version_id    uuid        NOT NULL,
    origin        text        NOT NULL CHECK (origin IN ('partner_material', 'industry_research')),
    origin_id     uuid        NOT NULL,
    scope         text        NOT NULL CHECK (scope IN ('partner', 'industry')),
    product_name  text        CHECK (product_name IS NULL
                                     OR char_length(btrim(product_name)) BETWEEN 1 AND 200),
    kind          text        NOT NULL CHECK (kind IN ('characteristic', 'limitation',
                                                       'application', 'commercial')),
    -- The checker's verdict (`block-01-spec.md` §6.7). `source_supported` means the
    -- source supports it — not that anybody independently verified it.
    status        text        NOT NULL CHECK (status IN ('source_supported', 'hypothesis',
                                                         'unknown', 'conflicted', 'stale')),
    attribute     text        NOT NULL CHECK (char_length(btrim(attribute)) BETWEEN 1 AND 200),
    value_text    text        NOT NULL CHECK (char_length(btrim(value_text)) BETWEEN 1 AND 200),
    unit          text        CHECK (unit IS NULL OR char_length(btrim(unit)) BETWEEN 1 AND 40),
    conditions    text        CHECK (conditions IS NULL OR char_length(conditions) <= 1000),
    -- The model's own words, carried over from the candidate. Never a quotation.
    model_context text        CHECK (model_context IS NULL OR char_length(model_context) <= 1000),
    -- Why the checker gave this verdict, in words. Shown verbatim.
    check_note    text        CHECK (check_note IS NULL OR char_length(check_note) <= 1000),
    created_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (bureau_id, id),
    UNIQUE (bureau_id, version_id, id),
    -- An industry conclusion cannot become a characteristic of the partner's product,
    -- because there is nowhere to write the product (`block-01-plan.md`, 1D §4).
    CONSTRAINT version_claims_industry_has_no_product
        CHECK (scope <> 'industry' OR product_name IS NULL),
    CONSTRAINT version_claims_origin_matches_scope
        CHECK ((origin = 'industry_research') = (scope = 'industry')),
    FOREIGN KEY (bureau_id, partner_id, version_id)
        REFERENCES otdel.knowledge_versions (bureau_id, partner_id, id) ON DELETE CASCADE
);

CREATE INDEX version_claims_version_idx
    ON otdel.version_claims (bureau_id, version_id, status);
CREATE INDEX version_claims_product_idx
    ON otdel.version_claims (bureau_id, version_id, product_name);

ALTER TABLE otdel.version_claims ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.version_claims FORCE ROW LEVEL SECURITY;
CREATE POLICY version_claims_bureau_isolation ON otdel.version_claims
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- The snapshot: citations
--
-- The quotation is **copied** here, with the offsets it had. 1C's own known limitation
-- is that re-reading a page rewrites `material_pages.text_content` in place, so offsets
-- of an older draft can drift; a published version must not drift with them. What the
-- document said at publication time is stored here and stays stored.
--
-- There is no foreign key to `materials` or `material_pages`, on purpose: a cascade from
-- a deleted material would otherwise silently remove claims from a published version.
-- The identifiers are kept as data so the interface can still offer `.../original#page=N`,
-- and a link that no longer resolves is an honest 404 rather than a rewritten history.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.version_evidence (
    id                uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id         uuid        NOT NULL,
    version_id        uuid        NOT NULL,
    claim_id          uuid        NOT NULL,
    source_kind       text        NOT NULL CHECK (source_kind IN ('material', 'external')),
    -- Partner material (1C origin).
    material_id       uuid,
    material_filename text        CHECK (material_filename IS NULL
                                         OR char_length(btrim(material_filename)) BETWEEN 1 AND 400),
    page_number       integer     CHECK (page_number IS NULL OR page_number >= 1),
    region_id         uuid,
    -- External source (1D origin).
    url               text        CHECK (url IS NULL
                                         OR (url ~ '^https://[^[:space:]]+$'
                                             AND char_length(url) BETWEEN 9 AND 2000)),
    host              text        CHECK (host IS NULL OR char_length(btrim(host)) BETWEEN 1 AND 253),
    retrieved_at      timestamptz,
    content_hash      text        CHECK (content_hash IS NULL OR content_hash ~ '^[0-9a-f]{64}$'),
    quote             text        NOT NULL CHECK (char_length(btrim(quote)) BETWEEN 1 AND 600),
    char_start        integer     NOT NULL CHECK (char_start >= 0),
    char_end          integer     NOT NULL CHECK (char_end > char_start),
    created_at        timestamptz NOT NULL DEFAULT now(),
    UNIQUE (bureau_id, id),
    -- Each kind carries its own identifying columns and none of the other's, so a row
    -- cannot claim to be both, or neither.
    CONSTRAINT version_evidence_material_shape
        CHECK (source_kind <> 'material'
               OR (material_id IS NOT NULL AND material_filename IS NOT NULL
                   AND page_number IS NOT NULL
                   AND url IS NULL AND host IS NULL AND content_hash IS NULL)),
    CONSTRAINT version_evidence_external_shape
        CHECK (source_kind <> 'external'
               OR (url IS NOT NULL AND host IS NOT NULL AND retrieved_at IS NOT NULL
                   AND material_id IS NULL AND page_number IS NULL AND region_id IS NULL)),
    FOREIGN KEY (bureau_id, version_id, claim_id)
        REFERENCES otdel.version_claims (bureau_id, version_id, id) ON DELETE CASCADE
);

CREATE INDEX version_evidence_claim_idx ON otdel.version_evidence (bureau_id, claim_id);

ALTER TABLE otdel.version_evidence ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.version_evidence FORCE ROW LEVEL SECURITY;
CREATE POLICY version_evidence_bureau_isolation ON otdel.version_evidence
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- The snapshot: gaps
--
-- A gap travels with the version it limits, so "какие ответы блокирует этот пробел"
-- (`block-01-spec.md` §11) is answerable from the published version alone, without
-- consulting a draft that may have moved on.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.version_gaps (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    partner_id    uuid        NOT NULL,
    version_id    uuid        NOT NULL,
    origin_id     uuid        NOT NULL,
    product_name  text        CHECK (product_name IS NULL
                                     OR char_length(btrim(product_name)) BETWEEN 1 AND 200),
    topic         text        NOT NULL CHECK (char_length(btrim(topic)) BETWEEN 1 AND 200),
    missing       text        NOT NULL CHECK (char_length(btrim(missing)) BETWEEN 1 AND 1000),
    blocks        text        CHECK (blocks IS NULL OR char_length(blocks) <= 1000),
    -- Which of the four readiness topics this gap limits.
    blocks_topics text[]      NOT NULL DEFAULT '{}'
                              CHECK (array_length(blocks_topics, 1) IS NULL
                                     OR array_length(blocks_topics, 1) <= 4),
    created_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (bureau_id, id),
    FOREIGN KEY (bureau_id, partner_id, version_id)
        REFERENCES otdel.knowledge_versions (bureau_id, partner_id, id) ON DELETE CASCADE
);

CREATE INDEX version_gaps_version_idx ON otdel.version_gaps (bureau_id, version_id);

ALTER TABLE otdel.version_gaps ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.version_gaps FORCE ROW LEVEL SECURITY;
CREATE POLICY version_gaps_bureau_isolation ON otdel.version_gaps
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- The snapshot: the readiness matrix
--
-- Four topics, decided separately (`block-01-spec.md` §7). This is the availability of
-- knowledge, not a permission to send anything, promise compatibility or take on an
-- obligation — the interface is required to say so next to it.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.version_readiness (
    id         uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id  uuid        NOT NULL,
    partner_id uuid        NOT NULL,
    version_id uuid        NOT NULL,
    topic      text        NOT NULL CHECK (topic IN ('product_description', 'audience_hypotheses',
                                                     'characteristic_answers', 'commercial_answers')),
    state      text        NOT NULL CHECK (state IN ('ready', 'limited', 'blocked')),
    reason     text        NOT NULL CHECK (char_length(btrim(reason)) BETWEEN 1 AND 1000),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (bureau_id, id),
    UNIQUE (version_id, topic),
    FOREIGN KEY (bureau_id, partner_id, version_id)
        REFERENCES otdel.knowledge_versions (bureau_id, partner_id, id) ON DELETE CASCADE
);

ALTER TABLE otdel.version_readiness ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.version_readiness FORCE ROW LEVEL SECURITY;
CREATE POLICY version_readiness_bureau_isolation ON otdel.version_readiness
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- The searchable half of a version
--
-- One chunk per claim: its product, attribute, value, unit, conditions and the words of
-- its citations, as one searchable string. Finer chunking of prose is a later phase;
-- what a claim needs in order to be *found* is exactly what a claim contains.
--
-- This table is derived and rebuildable, which is why — unlike the snapshot above — the
-- runtime role may rewrite it. Configuring an embedding provider after a version was
-- published must be able to add vectors to it without touching a single published claim.
--
-- `search_vector` is a generated column: it cannot drift from `chunk_text`, because
-- there is no second statement that could forget to update it.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.version_chunks (
    id                   uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id            uuid        NOT NULL,
    partner_id           uuid        NOT NULL,
    version_id           uuid        NOT NULL,
    claim_id             uuid        NOT NULL,
    chunk_text           text        NOT NULL
                                     CHECK (char_length(btrim(chunk_text)) BETWEEN 1 AND 4000),
    -- Folded forms for the exact half of the hybrid search: an article number or a
    -- parameter name is looked up as itself, not stemmed.
    normalised_value     text        NOT NULL CHECK (char_length(normalised_value) BETWEEN 1 AND 200),
    normalised_attribute text        NOT NULL CHECK (char_length(normalised_attribute) BETWEEN 1 AND 200),
    normalised_product   text        CHECK (normalised_product IS NULL
                                            OR char_length(normalised_product) BETWEEN 1 AND 200),
    search_vector        tsvector    GENERATED ALWAYS AS (to_tsvector('russian', chunk_text)) STORED,
    -- Which embedding space this row's vector lives in, and how wide it is. Both NULL
    -- when the row has no vector, which is the normal state without a provider.
    embedding_profile    text        CHECK (embedding_profile IS NULL
                                            OR char_length(btrim(embedding_profile)) BETWEEN 1 AND 200),
    embedding_dims       integer     CHECK (embedding_dims IS NULL
                                            OR embedding_dims BETWEEN 1 AND 4096),
    created_at           timestamptz NOT NULL DEFAULT now(),
    UNIQUE (bureau_id, id),
    UNIQUE (claim_id),
    CONSTRAINT version_chunks_embedding_is_whole
        CHECK ((embedding_profile IS NULL) = (embedding_dims IS NULL)),
    FOREIGN KEY (bureau_id, version_id, claim_id)
        REFERENCES otdel.version_claims (bureau_id, version_id, id) ON DELETE CASCADE
);

-- The full-text half.
CREATE INDEX version_chunks_search_idx ON otdel.version_chunks USING gin (search_vector);
-- The exact half: an article or a parameter written as itself.
CREATE INDEX version_chunks_value_idx
    ON otdel.version_chunks (bureau_id, version_id, normalised_value);
CREATE INDEX version_chunks_attribute_idx
    ON otdel.version_chunks (bureau_id, version_id, normalised_attribute);
CREATE INDEX version_chunks_version_idx ON otdel.version_chunks (bureau_id, version_id);

-- The vector column, when pgvector is available (see the top of this file).
--
-- It is `vector` without a declared width on purpose. The width belongs to the embedding
-- profile, which is configuration and may change; a column fixed at 1536 would make
-- changing the model a migration. Every query filters by `embedding_profile` first, so
-- two profiles never meet in one distance computation (`block-01-spec.md` §9), and
-- `embedding_dims` records what was actually stored.
--
-- **No ANN index is created here, deliberately.** `block-01-spec.md` §9 allows exact
-- vector search on a small corpus and requires that a move to ANN be justified by
-- measuring recall against it, with partner filters applied. Creating an HNSW index now
-- would silently make the planner answer `ORDER BY embedding <=> $1` approximately,
-- which is the very thing that has to be *measured* before it is adopted. The statement
-- to run once that measurement exists is in docs/publication-1e.md.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'vector') THEN
        ALTER TABLE otdel.version_chunks ADD COLUMN embedding vector;
        -- A stored vector must be as wide as the row says it is.
        ALTER TABLE otdel.version_chunks
            ADD CONSTRAINT version_chunks_embedding_matches_dims
            CHECK (embedding IS NULL
                   OR (embedding_dims IS NOT NULL AND vector_dims(embedding) = embedding_dims));
    END IF;
END
$$;

ALTER TABLE otdel.version_chunks ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.version_chunks FORCE ROW LEVEL SECURITY;
CREATE POLICY version_chunks_bureau_isolation ON otdel.version_chunks
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- "A claim without a source is not a claim", in the snapshot too.
--
-- The same deferred constraint trigger as 0004/0005: the claim and its citations are
-- written in one transaction, claim first, so the check has to run at commit. It runs
-- with the caller's own rights, so forced row-level security makes a transaction with no
-- bureau context find no evidence and fail — the fail-closed direction.
-- --------------------------------------------------------------------------
CREATE FUNCTION otdel.assert_version_claim_has_evidence() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = otdel, pg_catalog, pg_temp
AS $$
DECLARE
    v_id  uuid;
    v_has boolean;
BEGIN
    -- On the claim table the row being checked is the claim. On the evidence table it is
    -- the claim the evidence *used to* belong to: a DELETE removes it, and an UPDATE can
    -- re-point the row, leaving the old claim unsourced.
    IF TG_TABLE_NAME = 'version_evidence' THEN
        v_id := OLD.claim_id;
    ELSE
        v_id := NEW.id;
    END IF;

    IF v_id IS NULL THEN
        RETURN NULL;
    END IF;

    -- The claim may legitimately be gone (the whole version was removed).
    IF NOT EXISTS (SELECT 1 FROM otdel.version_claims c WHERE c.id = v_id) THEN
        RETURN NULL;
    END IF;

    SELECT EXISTS (SELECT 1 FROM otdel.version_evidence e WHERE e.claim_id = v_id) INTO v_has;

    IF NOT v_has THEN
        RAISE EXCEPTION
            'version claim % has no evidence; a published claim without a source is not stored',
            v_id
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;

    RETURN NULL;
END
$$;

CREATE CONSTRAINT TRIGGER version_claims_require_evidence
    AFTER INSERT OR UPDATE ON otdel.version_claims
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION otdel.assert_version_claim_has_evidence();

CREATE CONSTRAINT TRIGGER version_evidence_keeps_claims_sourced
    AFTER DELETE OR UPDATE ON otdel.version_evidence
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION otdel.assert_version_claim_has_evidence();

-- --------------------------------------------------------------------------
-- Immutability of a published snapshot
--
-- The runtime role is not granted UPDATE or DELETE on the four snapshot tables, so the
-- application cannot do either. This trigger is the second half of that: it holds for
-- every writer, including the schema owner and any later phase, and it is what makes
-- "неизменяемая версия" a property of the database rather than a convention.
--
-- What it still allows, and must:
--
--   * writing the snapshot in the first place (INSERT is untouched);
--   * removing a version that was never published — a `draft` left behind by a crashed
--     run is not history, it is litter;
--   * a cascade. Deleting a partner (erasure, or a test tearing down its bureau) deletes
--     the version row first, and PostgreSQL then removes the children; by that point the
--     parent is gone, `v_published` finds nothing, and the delete proceeds. A direct
--     DELETE against a published version's claim finds the parent and is refused.
-- --------------------------------------------------------------------------
CREATE FUNCTION otdel.refuse_published_snapshot_change() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = otdel, pg_catalog, pg_temp
AS $$
DECLARE
    v_version_id uuid := CASE TG_OP WHEN 'DELETE' THEN OLD.version_id ELSE NEW.version_id END;
    v_published  boolean;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        RAISE EXCEPTION
            'otdel.% is part of an immutable knowledge version and cannot be updated',
            TG_TABLE_NAME
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;

    SELECT v.published_at IS NOT NULL
      INTO v_published
      FROM otdel.knowledge_versions v
     WHERE v.id = v_version_id;

    -- No parent row: the version itself is being deleted and this is its cascade.
    IF v_published IS NULL THEN
        RETURN OLD;
    END IF;

    IF v_published THEN
        RAISE EXCEPTION
            'otdel.% belongs to knowledge version % which has been published; a published '
            'version is immutable',
            TG_TABLE_NAME, v_version_id
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;

    RETURN OLD;
END
$$;

CREATE TRIGGER version_claims_immutable
    BEFORE UPDATE OR DELETE ON otdel.version_claims
    FOR EACH ROW EXECUTE FUNCTION otdel.refuse_published_snapshot_change();

CREATE TRIGGER version_evidence_immutable
    BEFORE UPDATE OR DELETE ON otdel.version_evidence
    FOR EACH ROW EXECUTE FUNCTION otdel.refuse_published_snapshot_change();

CREATE TRIGGER version_gaps_immutable
    BEFORE UPDATE OR DELETE ON otdel.version_gaps
    FOR EACH ROW EXECUTE FUNCTION otdel.refuse_published_snapshot_change();

CREATE TRIGGER version_readiness_immutable
    BEFORE UPDATE OR DELETE ON otdel.version_readiness
    FOR EACH ROW EXECUTE FUNCTION otdel.refuse_published_snapshot_change();

-- --------------------------------------------------------------------------
-- A version's identity is frozen; only its lifecycle moves.
--
-- Publication, supersession and retraction are status changes, so the header row has to
-- be updatable. What must never change is *which* snapshot this is: the partner it
-- belongs to, its number, and the fingerprint of the input it was built from. Without
-- this, "неизменяемая версия" would be one UPDATE away from a different meaning.
--
-- A version that was published may never go back to `draft` or `validating` either: a
-- published snapshot that returned to drafting would be a published version that
-- changed.
-- --------------------------------------------------------------------------
CREATE FUNCTION otdel.refuse_version_identity_change() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = otdel, pg_catalog, pg_temp
AS $$
BEGIN
    IF NEW.partner_id IS DISTINCT FROM OLD.partner_id
       OR NEW.bureau_id IS DISTINCT FROM OLD.bureau_id
       OR NEW.number IS DISTINCT FROM OLD.number
       OR NEW.input_fingerprint IS DISTINCT FROM OLD.input_fingerprint
       OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION
            'knowledge version % is immutable: partner, number, fingerprint and creation '
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

    -- `revoked` and `superseded` are terminal.
    --
    -- Without this the lifecycle is not actually immutable: the runtime role holds
    -- UPDATE on this table (it has to, so a version can be published, superseded and
    -- retracted), and every CHECK on the row is satisfied by flipping a retracted
    -- version back to `published` — `knowledge_versions_revoked_has_reason` only
    -- constrains a row *whose status is* `revoked`, and the partial unique index is
    -- happy as long as nothing else is published. The result would be a live published
    -- version carrying `revoked_at` and a retraction reason, which is exactly what
    -- "откат не воскрешает отозванные источники" (`block-01-spec.md` §7) forbids.
    --
    -- A retracted version is re-published by checking again and publishing a new one,
    -- which is what leaves the retraction in the history where it belongs.
    IF OLD.status IN ('revoked', 'superseded') AND NEW.status IS DISTINCT FROM OLD.status THEN
        RAISE EXCEPTION
            'knowledge version % is `%` and that is final; run a new check to publish again',
            OLD.id, OLD.status
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;

    -- A published time, once set, is history.
    IF OLD.published_at IS NOT NULL AND NEW.published_at IS DISTINCT FROM OLD.published_at THEN
        RAISE EXCEPTION
            'knowledge version % already has a publication time', OLD.id
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;

    RETURN NEW;
END
$$;

CREATE TRIGGER knowledge_versions_identity_frozen
    BEFORE UPDATE ON otdel.knowledge_versions
    FOR EACH ROW EXECUTE FUNCTION otdel.refuse_version_identity_change();

-- A published version is not deletable by anybody short of removing its partner. The
-- runtime role has no DELETE grant at all; this covers the schema owner as well.
CREATE FUNCTION otdel.refuse_published_version_delete() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = otdel, pg_catalog, pg_temp
AS $$
DECLARE
    v_partner_exists boolean;
BEGIN
    IF OLD.published_at IS NULL THEN
        RETURN OLD;
    END IF;

    -- The partner is being deleted: this is that cascade, and erasing a partner has to
    -- erase their published knowledge too.
    SELECT EXISTS (SELECT 1 FROM otdel.partners p WHERE p.id = OLD.partner_id)
      INTO v_partner_exists;
    IF NOT v_partner_exists THEN
        RETURN OLD;
    END IF;

    RAISE EXCEPTION
        'knowledge version % has been published and cannot be deleted; retract it instead',
        OLD.id
        USING ERRCODE = 'integrity_constraint_violation';
END
$$;

CREATE TRIGGER knowledge_versions_published_not_deletable
    BEFORE DELETE ON otdel.knowledge_versions
    FOR EACH ROW EXECUTE FUNCTION otdel.refuse_published_version_delete();

-- --------------------------------------------------------------------------
-- The queue
--
-- `validate_partner` is the fifth job kind, and the first one that is not about a single
-- material: the checker looks at everything a partner has, because a contradiction
-- between two documents is only visible from there. `material_id` therefore becomes
-- nullable — exactly for this kind, and a CHECK ties the two together so no other kind
-- can lose its material.
--
-- The composite foreign key to `materials` is MATCH SIMPLE, so it is satisfied (not
-- checked) when `material_id` is NULL; a second, narrower key keeps the partner honest
-- for those rows.
-- --------------------------------------------------------------------------
ALTER TABLE otdel.jobs DROP CONSTRAINT jobs_kind_check;
ALTER TABLE otdel.jobs
    ADD CONSTRAINT jobs_kind_check
    CHECK (kind IN ('extract_document', 'extract_page', 'understand_material',
                    'research_plan', 'validate_partner'));

ALTER TABLE otdel.jobs ALTER COLUMN material_id DROP NOT NULL;
ALTER TABLE otdel.jobs
    ADD CONSTRAINT jobs_material_matches_kind
    CHECK ((kind = 'validate_partner') = (material_id IS NULL));
ALTER TABLE otdel.jobs
    ADD CONSTRAINT jobs_partner_fk
    FOREIGN KEY (bureau_id, partner_id)
        REFERENCES otdel.partners (bureau_id, id) ON DELETE RESTRICT;

ALTER TABLE otdel.jobs ADD COLUMN validation_run_id uuid;
ALTER TABLE otdel.jobs
    ADD CONSTRAINT jobs_validation_run_matches_kind
    CHECK ((kind = 'validate_partner') = (validation_run_id IS NOT NULL));
ALTER TABLE otdel.jobs
    ADD CONSTRAINT jobs_validation_run_fk
    FOREIGN KEY (bureau_id, validation_run_id)
        REFERENCES otdel.validation_runs (bureau_id, id) ON DELETE CASCADE;

CREATE INDEX jobs_validation_run_idx ON otdel.jobs (bureau_id, validation_run_id)
    WHERE validation_run_id IS NOT NULL;

-- --------------------------------------------------------------------------
-- Privileges for the runtime role.
--
-- The asymmetry here is the phase's central rule, written as grants.
--
-- `validation_runs` and `knowledge_versions` are mutable: a run reports progress, and a
-- version moves through its lifecycle (published → superseded, published → revoked). No
-- DELETE on versions: a published version is retracted, never removed, so that "откат не
-- воскрешает отозванные источники" has something to point at.
--
-- The four snapshot tables get **SELECT and INSERT only**. The application writes a
-- version once and then cannot change or remove any part of it. That is not a
-- convention the code has to remember; it is a privilege it does not have.
--
-- `version_chunks` is the exception, and it is derived data: it carries no claim of its
-- own, only a searchable rendering of one, and it must be rewritable so that configuring
-- an embedding provider later can add vectors to an already published version.
-- --------------------------------------------------------------------------
GRANT SELECT, INSERT, UPDATE         ON otdel.validation_runs    TO otdel_app;
GRANT SELECT, INSERT, UPDATE         ON otdel.knowledge_versions TO otdel_app;
GRANT SELECT, INSERT                 ON otdel.version_claims     TO otdel_app;
GRANT SELECT, INSERT                 ON otdel.version_evidence   TO otdel_app;
GRANT SELECT, INSERT                 ON otdel.version_gaps       TO otdel_app;
GRANT SELECT, INSERT                 ON otdel.version_readiness  TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.version_chunks     TO otdel_app;

REVOKE ALL ON FUNCTION otdel.assert_version_claim_has_evidence() FROM PUBLIC;
REVOKE ALL ON FUNCTION otdel.refuse_published_snapshot_change() FROM PUBLIC;
REVOKE ALL ON FUNCTION otdel.refuse_version_identity_change() FROM PUBLIC;
REVOKE ALL ON FUNCTION otdel.refuse_published_version_delete() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION otdel.assert_version_claim_has_evidence() TO otdel_app;
GRANT EXECUTE ON FUNCTION otdel.refuse_published_snapshot_change() TO otdel_app;
GRANT EXECUTE ON FUNCTION otdel.refuse_version_identity_change() TO otdel_app;
GRANT EXECUTE ON FUNCTION otdel.refuse_published_version_delete() TO otdel_app;
