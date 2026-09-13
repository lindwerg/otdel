-- OTDEL block 1, phase 1C — the structured product draft built from read pages.
--
-- Applied by the *migration* role. Migrations 0001–0003 are frozen (they have been
-- applied to the pilot database), so everything here is additive and lives in its own
-- file.
--
-- Three properties are enforced by the schema itself rather than left to the
-- application:
--
--  1. **A fact cannot exist without evidence.** A deferred constraint trigger checks at
--     commit time that every fact, glossary term and Q&A row has at least one row in
--     otdel.knowledge_evidence. Deferred, because a fact and its evidence are inserted
--     in that order inside one transaction.
--  2. **Evidence cannot point outside its own partner.** Each evidence row carries the
--     bureau, the partner, the material and the page, and *two* composite foreign keys
--     tie them together: (bureau, material, page) to the page and (bureau, partner,
--     material) to the material. A row citing another partner's page is therefore not
--     representable, independently of any application check.
--  3. **Everything here is a candidate.** `status` on a fact accepts exactly one value,
--     'candidate'. The checker's vocabulary (source_supported, conflicted, …) belongs
--     to phase 1E and cannot be written by this phase.
--
-- These rows are *derived*: they can be produced again from the original. That is why
-- they cascade on delete and why the runtime role may DELETE them — re-running the
-- understanding of a material replaces its candidates instead of accumulating copies.
-- The originals and the page records remain undeletable by the application.

-- --------------------------------------------------------------------------
-- Jobs: the understanding run is a third kind of queued work.
-- --------------------------------------------------------------------------
ALTER TABLE otdel.jobs DROP CONSTRAINT jobs_kind_check;
ALTER TABLE otdel.jobs
    ADD CONSTRAINT jobs_kind_check
    CHECK (kind IN ('extract_document', 'extract_page', 'understand_material'));

-- Only a page job names a page; the 0003 constraint said so in terms of the two kinds
-- that existed then. Restated here so the new kind is covered explicitly.
ALTER TABLE otdel.jobs DROP CONSTRAINT jobs_page_number_matches_kind;
ALTER TABLE otdel.jobs
    ADD CONSTRAINT jobs_page_number_matches_kind
    CHECK ((kind = 'extract_page') = (page_number IS NOT NULL));

-- --------------------------------------------------------------------------
-- Runs
--
-- One row per material: the current state of its understanding, updated in place.
-- History of *attempts* already lives on the job row (attempts, error, error_kind);
-- duplicating it here would create two answers to "how did the last run go".
-- --------------------------------------------------------------------------
CREATE TABLE otdel.knowledge_runs (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id       uuid        NOT NULL,
    partner_id      uuid        NOT NULL,
    material_id     uuid        NOT NULL,
    status          text        NOT NULL DEFAULT 'queued'
                                CHECK (status IN ('queued', 'running', 'completed', 'partial',
                                                  'failed', 'needs_provider')),
    -- Which adapter and model produced the draft, and under which prompt profile: a
    -- later re-run with another model must be distinguishable (spec §6.1).
    provider        text        CHECK (provider IS NULL OR char_length(provider) <= 100),
    model           text        CHECK (model IS NULL OR char_length(model) <= 200),
    prompt_profile  text        NOT NULL CHECK (char_length(prompt_profile) BETWEEN 1 AND 100),

    pages_considered   integer  NOT NULL DEFAULT 0 CHECK (pages_considered >= 0),
    pages_skipped      integer  NOT NULL DEFAULT 0 CHECK (pages_skipped >= 0),
    requests_made      integer  NOT NULL DEFAULT 0 CHECK (requests_made >= 0),
    input_chars        integer  NOT NULL DEFAULT 0 CHECK (input_chars >= 0),
    facts_accepted     integer  NOT NULL DEFAULT 0 CHECK (facts_accepted >= 0),
    facts_rejected     integer  NOT NULL DEFAULT 0 CHECK (facts_rejected >= 0),

    -- Why candidates were refused, in words. Bounded by the application to a few dozen
    -- deduplicated lines; the column keeps that honest.
    rejections      text[]      NOT NULL DEFAULT '{}'
                                CHECK (array_length(rejections, 1) IS NULL
                                       OR array_length(rejections, 1) <= 100),
    diagnostic      text        CHECK (diagnostic IS NULL OR char_length(diagnostic) <= 2000),
    started_at      timestamptz,
    finished_at     timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),

    UNIQUE (material_id),
    UNIQUE (bureau_id, id),
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE
);

CREATE INDEX knowledge_runs_partner_idx ON otdel.knowledge_runs (bureau_id, partner_id, updated_at DESC);

ALTER TABLE otdel.knowledge_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.knowledge_runs FORCE ROW LEVEL SECURITY;
CREATE POLICY knowledge_runs_bureau_isolation ON otdel.knowledge_runs
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Product categories (directions and families) and products
--
-- Both are scoped to the material whose run produced them. Two materials naming the
-- same profile therefore produce two candidates: merging them is a judgement about
-- identity that needs evidence, and spec §6.6 forbids collapsing synonyms without it.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.product_categories (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id       uuid        NOT NULL,
    partner_id      uuid        NOT NULL,
    material_id     uuid        NOT NULL,
    run_id          uuid        NOT NULL,
    kind            text        NOT NULL CHECK (kind IN ('direction', 'family')),
    name            text        NOT NULL CHECK (char_length(btrim(name)) BETWEEN 1 AND 200),
    -- Case/whitespace-folded name, used only to keep one run from storing the same
    -- category twice.
    normalised_name text        NOT NULL CHECK (char_length(normalised_name) BETWEEN 1 AND 200),
    summary         text        CHECK (summary IS NULL OR char_length(summary) <= 1000),
    created_at      timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    UNIQUE (material_id, kind, normalised_name),
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, run_id)
        REFERENCES otdel.knowledge_runs (bureau_id, id) ON DELETE CASCADE
);

CREATE INDEX product_categories_partner_idx ON otdel.product_categories (bureau_id, partner_id, name);
CREATE INDEX product_categories_run_idx ON otdel.product_categories (bureau_id, run_id);

ALTER TABLE otdel.product_categories ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.product_categories FORCE ROW LEVEL SECURITY;
CREATE POLICY product_categories_bureau_isolation ON otdel.product_categories
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

CREATE TABLE otdel.products (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id       uuid        NOT NULL,
    partner_id      uuid        NOT NULL,
    material_id     uuid        NOT NULL,
    run_id          uuid        NOT NULL,
    category_id     uuid,
    kind            text        NOT NULL CHECK (kind IN ('product', 'service')),
    name            text        NOT NULL CHECK (char_length(btrim(name)) BETWEEN 1 AND 200),
    normalised_name text        NOT NULL CHECK (char_length(normalised_name) BETWEEN 1 AND 200),
    summary         text        CHECK (summary IS NULL OR char_length(summary) <= 1000),
    created_at      timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    UNIQUE (material_id, kind, normalised_name),
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, run_id)
        REFERENCES otdel.knowledge_runs (bureau_id, id) ON DELETE CASCADE,
    -- A category of the same bureau, or none. Losing the link is acceptable; pointing
    -- at another bureau's category is not representable.
    --
    -- The column list after SET NULL matters: without it PostgreSQL would null *every*
    -- referencing column, including the NOT NULL bureau_id, and the delete would fail.
    FOREIGN KEY (bureau_id, category_id)
        REFERENCES otdel.product_categories (bureau_id, id) ON DELETE SET NULL (category_id)
);

CREATE INDEX products_partner_idx ON otdel.products (bureau_id, partner_id, name);
CREATE INDEX products_category_idx ON otdel.products (bureau_id, category_id);
CREATE INDEX products_run_idx ON otdel.products (bureau_id, run_id);

ALTER TABLE otdel.products ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.products FORCE ROW LEVEL SECURITY;
CREATE POLICY products_bureau_isolation ON otdel.products
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Facts
--
-- `value_text` is the source's own wording. There is deliberately no numeric column,
-- for the same reason table cells have none (0003): parsing "40…60" or "– / 2074" into
-- a number is a judgement this phase is not entitled to make. `unit` and `conditions`
-- are separate and are only stored when they are literally present in a cited
-- fragment; anything the model added in its own words lives in `model_context`.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.knowledge_facts (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    partner_id    uuid        NOT NULL,
    material_id   uuid        NOT NULL,
    run_id        uuid        NOT NULL,
    product_id    uuid,
    kind          text        NOT NULL
                              CHECK (kind IN ('characteristic', 'limitation', 'application', 'commercial')),
    -- Phase 1C produces candidates only. Verification statuses belong to 1E.
    status        text        NOT NULL DEFAULT 'candidate' CHECK (status = 'candidate'),
    attribute     text        NOT NULL CHECK (char_length(btrim(attribute)) BETWEEN 1 AND 200),
    value_text    text        NOT NULL CHECK (char_length(btrim(value_text)) BETWEEN 1 AND 200),
    unit          text        CHECK (unit IS NULL OR char_length(btrim(unit)) BETWEEN 1 AND 40),
    conditions    text        CHECK (conditions IS NULL OR char_length(conditions) <= 1000),
    model_context text        CHECK (model_context IS NULL OR char_length(model_context) <= 1000),
    created_at    timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, run_id)
        REFERENCES otdel.knowledge_runs (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, product_id)
        REFERENCES otdel.products (bureau_id, id) ON DELETE CASCADE
);

CREATE INDEX knowledge_facts_partner_idx ON otdel.knowledge_facts (bureau_id, partner_id, created_at DESC);
CREATE INDEX knowledge_facts_product_idx ON otdel.knowledge_facts (bureau_id, product_id);
CREATE INDEX knowledge_facts_material_idx ON otdel.knowledge_facts (bureau_id, material_id);
CREATE INDEX knowledge_facts_run_idx ON otdel.knowledge_facts (bureau_id, run_id);

ALTER TABLE otdel.knowledge_facts ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.knowledge_facts FORCE ROW LEVEL SECURITY;
CREATE POLICY knowledge_facts_bureau_isolation ON otdel.knowledge_facts
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Glossary, Q&A and gaps
-- --------------------------------------------------------------------------
CREATE TABLE otdel.glossary_terms (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id       uuid        NOT NULL,
    partner_id      uuid        NOT NULL,
    material_id     uuid        NOT NULL,
    run_id          uuid        NOT NULL,
    term            text        NOT NULL CHECK (char_length(btrim(term)) BETWEEN 1 AND 200),
    normalised_term text        NOT NULL CHECK (char_length(normalised_term) BETWEEN 1 AND 200),
    definition      text        NOT NULL CHECK (char_length(btrim(definition)) BETWEEN 1 AND 1000),
    -- true when the definition is the model's wording rather than the source's. The
    -- interface labels it accordingly instead of presenting it as a quotation.
    definition_is_model_context boolean NOT NULL DEFAULT true,
    created_at      timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    UNIQUE (material_id, normalised_term),
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, run_id)
        REFERENCES otdel.knowledge_runs (bureau_id, id) ON DELETE CASCADE
);

CREATE INDEX glossary_terms_partner_idx ON otdel.glossary_terms (bureau_id, partner_id, term);
CREATE INDEX glossary_terms_run_idx ON otdel.glossary_terms (bureau_id, run_id);

ALTER TABLE otdel.glossary_terms ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.glossary_terms FORCE ROW LEVEL SECURITY;
CREATE POLICY glossary_terms_bureau_isolation ON otdel.glossary_terms
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

CREATE TABLE otdel.knowledge_qa (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    partner_id    uuid        NOT NULL,
    material_id   uuid        NOT NULL,
    run_id        uuid        NOT NULL,
    question      text        NOT NULL CHECK (char_length(btrim(question)) BETWEEN 1 AND 1000),
    answer        text        NOT NULL CHECK (char_length(btrim(answer)) BETWEEN 1 AND 1000),
    -- An answer is the model's sentence unless it is literally in the source. Default
    -- true: the honest assumption, so a writer that forgets the column cannot present
    -- a synthesis as a quotation.
    answer_is_model_context boolean NOT NULL DEFAULT true,
    created_at    timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, run_id)
        REFERENCES otdel.knowledge_runs (bureau_id, id) ON DELETE CASCADE
);

CREATE INDEX knowledge_qa_partner_idx ON otdel.knowledge_qa (bureau_id, partner_id, created_at DESC);
CREATE INDEX knowledge_qa_run_idx ON otdel.knowledge_qa (bureau_id, run_id);

ALTER TABLE otdel.knowledge_qa ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.knowledge_qa FORCE ROW LEVEL SECURITY;
CREATE POLICY knowledge_qa_bureau_isolation ON otdel.knowledge_qa
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- A gap records what the material does *not* say, so it carries no evidence: there is
-- nothing to quote. It is the honest alternative to inventing a price or a lead time.
CREATE TABLE otdel.knowledge_gaps (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    partner_id    uuid        NOT NULL,
    material_id   uuid        NOT NULL,
    run_id        uuid        NOT NULL,
    product_id    uuid,
    topic         text        NOT NULL CHECK (char_length(btrim(topic)) BETWEEN 1 AND 200),
    missing       text        NOT NULL CHECK (char_length(btrim(missing)) BETWEEN 1 AND 1000),
    blocks        text        CHECK (blocks IS NULL OR char_length(blocks) <= 1000),
    status        text        NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'closed')),
    created_at    timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, run_id)
        REFERENCES otdel.knowledge_runs (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, product_id)
        REFERENCES otdel.products (bureau_id, id) ON DELETE SET NULL (product_id)
);

CREATE INDEX knowledge_gaps_partner_idx ON otdel.knowledge_gaps (bureau_id, partner_id, created_at DESC);
CREATE INDEX knowledge_gaps_product_idx ON otdel.knowledge_gaps (bureau_id, product_id);
CREATE INDEX knowledge_gaps_run_idx ON otdel.knowledge_gaps (bureau_id, run_id);

ALTER TABLE otdel.knowledge_gaps ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.knowledge_gaps FORCE ROW LEVEL SECURITY;
CREATE POLICY knowledge_gaps_bureau_isolation ON otdel.knowledge_gaps
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- Questions prepared from a gap. Nothing is sent in this phase: `prepared` is the only
-- status this phase writes, and the delivery states exist so the communication channel
-- (spec §2) can later record what really happened instead of overwriting history.
CREATE TABLE otdel.knowledge_questions (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    partner_id    uuid        NOT NULL,
    material_id   uuid        NOT NULL,
    run_id        uuid        NOT NULL,
    gap_id        uuid        NOT NULL,
    audience      text        NOT NULL CHECK (audience IN ('partner', 'industry')),
    text_content  text        NOT NULL CHECK (char_length(btrim(text_content)) BETWEEN 1 AND 1000),
    status        text        NOT NULL DEFAULT 'prepared'
                              CHECK (status IN ('prepared', 'answered', 'withdrawn')),
    created_at    timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    UNIQUE (gap_id, audience),
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, run_id)
        REFERENCES otdel.knowledge_runs (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, gap_id)
        REFERENCES otdel.knowledge_gaps (bureau_id, id) ON DELETE CASCADE
);

CREATE INDEX knowledge_questions_partner_idx ON otdel.knowledge_questions (bureau_id, partner_id, created_at DESC);
CREATE INDEX knowledge_questions_gap_idx ON otdel.knowledge_questions (bureau_id, gap_id);
CREATE INDEX knowledge_questions_run_idx ON otdel.knowledge_questions (bureau_id, run_id);

ALTER TABLE otdel.knowledge_questions ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.knowledge_questions FORCE ROW LEVEL SECURITY;
CREATE POLICY knowledge_questions_bureau_isolation ON otdel.knowledge_questions
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Evidence
--
-- The verbatim fragment of a page, with the character offsets that locate it there.
-- `quote` is the *page's* wording (the server extracts it by offset after matching the
-- model's quote), which is what makes "открыть факт и увидеть цитату" trustworthy.
--
-- Exactly one parent: a fact, a glossary term or a Q&A entry.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.knowledge_evidence (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    partner_id    uuid        NOT NULL,
    material_id   uuid        NOT NULL,
    page_id       uuid        NOT NULL,
    page_number   integer     NOT NULL CHECK (page_number >= 1),
    region_id     uuid,
    fact_id       uuid,
    term_id       uuid,
    qa_id         uuid,
    quote         text        NOT NULL CHECK (char_length(btrim(quote)) BETWEEN 1 AND 600),
    -- Character offsets into otdel.material_pages.text_content.
    char_start    integer     NOT NULL CHECK (char_start >= 0),
    char_end      integer     NOT NULL CHECK (char_end > char_start),
    created_at    timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    CONSTRAINT knowledge_evidence_has_exactly_one_parent
        CHECK (num_nonnulls(fact_id, term_id, qa_id) = 1),

    -- The page really exists, in this bureau, in this material…
    FOREIGN KEY (bureau_id, material_id, page_id)
        REFERENCES otdel.material_pages (bureau_id, material_id, id) ON DELETE CASCADE,
    -- …and that material really belongs to this partner. Together these two keys make
    -- "evidence pointing at another partner's page" unrepresentable.
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, region_id)
        REFERENCES otdel.page_regions (bureau_id, id) ON DELETE SET NULL (region_id),
    FOREIGN KEY (bureau_id, fact_id)
        REFERENCES otdel.knowledge_facts (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, term_id)
        REFERENCES otdel.glossary_terms (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, qa_id)
        REFERENCES otdel.knowledge_qa (bureau_id, id) ON DELETE CASCADE
);

CREATE INDEX knowledge_evidence_fact_idx ON otdel.knowledge_evidence (bureau_id, fact_id);
CREATE INDEX knowledge_evidence_term_idx ON otdel.knowledge_evidence (bureau_id, term_id);
CREATE INDEX knowledge_evidence_qa_idx ON otdel.knowledge_evidence (bureau_id, qa_id);
CREATE INDEX knowledge_evidence_page_idx ON otdel.knowledge_evidence (bureau_id, page_id);

ALTER TABLE otdel.knowledge_evidence ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.knowledge_evidence FORCE ROW LEVEL SECURITY;
CREATE POLICY knowledge_evidence_bureau_isolation ON otdel.knowledge_evidence
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- "No statement without a source", enforced at commit time.
--
-- A fact and its evidence are written in one transaction, fact first, so the check has
-- to be deferred. It is a CONSTRAINT TRIGGER rather than application code because the
-- rule must hold for every writer, including a future one.
--
-- It runs with the caller's own rights on purpose. Row-level security is forced on
-- these tables, so the check sees exactly the rows the writing transaction may see: a
-- transaction with no bureau context finds no evidence and gets an error, which is the
-- fail-closed direction. A SECURITY DEFINER function would add privilege without
-- changing that (RLS is forced for the owner too) and is therefore not used.
-- --------------------------------------------------------------------------
CREATE FUNCTION otdel.assert_statement_has_evidence() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = otdel, pg_catalog, pg_temp
AS $$
DECLARE
    v_kind text := TG_ARGV[0];
    v_id   uuid;
    v_has  boolean;
BEGIN
    -- On a statement table the row being checked is the statement itself (INSERT or
    -- UPDATE → NEW). On the evidence table the statement to re-check is the one the
    -- evidence *used to* belong to: a DELETE removes it, and an UPDATE can re-point
    -- the row at a different parent, leaving the old one unsourced.
    IF TG_TABLE_NAME = 'knowledge_evidence' THEN
        v_id := CASE v_kind
                    WHEN 'fact' THEN OLD.fact_id
                    WHEN 'term' THEN OLD.term_id
                    ELSE OLD.qa_id
                END;
    ELSE
        v_id := NEW.id;
    END IF;

    IF v_id IS NULL THEN
        RETURN NULL;
    END IF;

    -- The parent may legitimately be gone (a re-run deleted the whole set).
    IF v_kind = 'fact' THEN
        IF NOT EXISTS (SELECT 1 FROM otdel.knowledge_facts f WHERE f.id = v_id) THEN
            RETURN NULL;
        END IF;
        SELECT EXISTS (SELECT 1 FROM otdel.knowledge_evidence e WHERE e.fact_id = v_id) INTO v_has;
    ELSIF v_kind = 'term' THEN
        IF NOT EXISTS (SELECT 1 FROM otdel.glossary_terms t WHERE t.id = v_id) THEN
            RETURN NULL;
        END IF;
        SELECT EXISTS (SELECT 1 FROM otdel.knowledge_evidence e WHERE e.term_id = v_id) INTO v_has;
    ELSE
        IF NOT EXISTS (SELECT 1 FROM otdel.knowledge_qa q WHERE q.id = v_id) THEN
            RETURN NULL;
        END IF;
        SELECT EXISTS (SELECT 1 FROM otdel.knowledge_evidence e WHERE e.qa_id = v_id) INTO v_has;
    END IF;

    IF NOT v_has THEN
        RAISE EXCEPTION
            'knowledge statement %  (%) has no evidence; a fact without a source is not stored',
            v_id, v_kind
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;

    RETURN NULL;
END
$$;

-- INSERT *and* UPDATE: rewriting a statement must not be a way to keep a row whose
-- evidence has meanwhile gone. The rule has to hold for every writer, including a
-- later phase that edits rather than replaces.
CREATE CONSTRAINT TRIGGER knowledge_facts_require_evidence
    AFTER INSERT OR UPDATE ON otdel.knowledge_facts
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION otdel.assert_statement_has_evidence('fact');

CREATE CONSTRAINT TRIGGER glossary_terms_require_evidence
    AFTER INSERT OR UPDATE ON otdel.glossary_terms
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION otdel.assert_statement_has_evidence('term');

CREATE CONSTRAINT TRIGGER knowledge_qa_require_evidence
    AFTER INSERT OR UPDATE ON otdel.knowledge_qa
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION otdel.assert_statement_has_evidence('qa');

-- Removing the last evidence row of a surviving statement — or re-pointing it at a
-- different statement — is the same violation seen from the other side.
CREATE CONSTRAINT TRIGGER knowledge_evidence_keeps_facts_sourced
    AFTER DELETE OR UPDATE ON otdel.knowledge_evidence
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION otdel.assert_statement_has_evidence('fact');

CREATE CONSTRAINT TRIGGER knowledge_evidence_keeps_terms_sourced
    AFTER DELETE OR UPDATE ON otdel.knowledge_evidence
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION otdel.assert_statement_has_evidence('term');

CREATE CONSTRAINT TRIGGER knowledge_evidence_keeps_qa_sourced
    AFTER DELETE OR UPDATE ON otdel.knowledge_evidence
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION otdel.assert_statement_has_evidence('qa');

-- --------------------------------------------------------------------------
-- Privileges for the runtime role.
--
-- DELETE is granted on the candidate tables: re-running the understanding of a
-- material replaces its draft. The run row itself is never deleted by the application
-- (it is the record that the material was processed), and nothing here grants access
-- to materials, pages or partners beyond what 0001/0003 already allow.
-- --------------------------------------------------------------------------
GRANT SELECT, INSERT, UPDATE         ON otdel.knowledge_runs      TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.product_categories  TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.products            TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.knowledge_facts     TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.glossary_terms      TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.knowledge_qa        TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.knowledge_gaps      TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.knowledge_questions TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.knowledge_evidence  TO otdel_app;

-- The trigger function is not a general-purpose entry point; only the role whose
-- writes fire it needs to execute it.
REVOKE ALL ON FUNCTION otdel.assert_statement_has_evidence() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION otdel.assert_statement_has_evidence() TO otdel_app;
