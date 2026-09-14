-- OTDEL block 1, R05.2 — what each purpose-specific pass of a run covered.
--
-- Migrations 0001–0009 are applied and frozen; everything here is additive.
--
-- Why this exists. R05 made a run say how much of the material it read. It could not say
-- what it read it *for*, because there was only one pass: a single request asking for
-- products, facts, terms, tasks, questions and gaps at once. A live run over a technical
-- catalogue came back with 51 products, 6 facts and **0 terms, 0 applications** — not
-- because the material lacked them, but because the request was over before it got there,
-- and the only thing the record could say afterwards was "44 of 44 pages processed".
--
-- That sentence was true and useless. The pages had been read *for products*. Whether
-- anything had ever looked for a term was not a question the schema could answer, so
-- "0 terms" stayed ambiguous in exactly the way R05 exists to prevent — and the only way
-- out was a declaration, which is how a sentence came to clear a topic nobody had
-- examined.
--
-- One row per (run, purpose) closes it. The requirement check can now distinguish:
--
--   * the glossary pass read every page and found nothing  → an observation about the
--     material, and a declaration beside it is worth something;
--   * the glossary pass never ran, or ran out of budget    → nothing was established, and
--     the topic is reported as unresolved coverage. No sentence clears it.

CREATE TABLE otdel.knowledge_run_passes (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    partner_id    uuid        NOT NULL,
    material_id   uuid        NOT NULL,
    run_id        uuid        NOT NULL,

    -- Which kind of thing this pass was asking for. The five are a fixed vocabulary and
    -- match `otdel_knowledge::DraftPurpose`; a value outside it is a bug, not a new case.
    purpose       text        NOT NULL
                              CHECK (purpose IN ('inventory', 'facts', 'glossary',
                                                 'applications', 'inquiry')),

    -- The share of the run's request budget this pass was given, and what it spent. Both,
    -- because "made 2 requests" and "was allowed 2 requests" are different findings: the
    -- first can mean the material was small, the second always means the budget bound it.
    requests_allowed integer  NOT NULL DEFAULT 0 CHECK (requests_allowed >= 0),
    requests_made    integer  NOT NULL DEFAULT 0 CHECK (requests_made >= 0),

    -- The page account of this pass alone. `pages_total` is the material's offerable
    -- pages, so `pages_processed < pages_total` is readable without a join.
    pages_total      integer  NOT NULL DEFAULT 0 CHECK (pages_total >= 0),
    pages_processed  integer  NOT NULL DEFAULT 0 CHECK (pages_processed >= 0),
    pages_deferred   integer  NOT NULL DEFAULT 0 CHECK (pages_deferred >= 0),

    -- Did this pass see the whole material? Stored rather than derived, because it is the
    -- input to the requirement check and a reader must be able to see the same value the
    -- check saw, not a recomputation of it.
    covered_everything boolean NOT NULL DEFAULT false,

    input_chars      integer  NOT NULL DEFAULT 0 CHECK (input_chars >= 0),
    created_at       timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    -- One row per purpose per run: a pass happens once.
    UNIQUE (run_id, purpose),

    -- A pass that made no request covered nothing. Stops the combination that would make
    -- "0 terms" clearable again: a glossary pass that never ran claiming full coverage.
    CONSTRAINT knowledge_run_passes_coverage_needs_a_request
        CHECK (NOT covered_everything OR requests_made > 0),
    -- …and a pass that left pages behind did not cover everything either.
    CONSTRAINT knowledge_run_passes_coverage_defers_nothing
        CHECK (NOT covered_everything OR pages_deferred = 0),

    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, run_id)
        REFERENCES otdel.knowledge_runs (bureau_id, id) ON DELETE CASCADE
);

CREATE INDEX knowledge_run_passes_run_idx
    ON otdel.knowledge_run_passes (bureau_id, run_id, purpose);

ALTER TABLE otdel.knowledge_run_passes ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.knowledge_run_passes FORCE ROW LEVEL SECURITY;
CREATE POLICY knowledge_run_passes_bureau_isolation ON otdel.knowledge_run_passes
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.knowledge_run_passes TO otdel_app;
