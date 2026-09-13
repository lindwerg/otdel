-- OTDEL block 1, phase 1D — bounded industry research: its money, its sources and its
-- conclusions.
--
-- Applied by the *migration* role. Migrations 0001–0004 are frozen (they have been
-- applied to the pilot database), so everything here is additive and lives in its own
-- file.
--
-- Five properties are enforced by the schema itself rather than left to the application:
--
--  1. **A conclusion cannot exist without an external source.** A deferred constraint
--     trigger checks at commit time that every row in otdel.research_findings has at
--     least one row in otdel.research_evidence — the same shape as 0004's rule for
--     facts, and for the same reason.
--
--  2. **Evidence cannot point outside its own plan.** Each evidence row carries the
--     bureau *and the plan*, and both composite foreign keys — to the finding and to the
--     source — include them. A conclusion of plan A citing a page fetched by plan B (let
--     alone by another bureau) is not representable, independently of any application
--     check.
--
--  3. **A finding is about the industry and nothing else.** `scope` accepts exactly one
--     value, 'industry', and the table has no product, material or partner-attribute
--     column at all. Copying a competitor's characteristic into a partner's product card
--     (`docs/block-01-plan.md`, 1D §4) has no representation to be written into.
--
--  4. **Everything here is a candidate.** `status` accepts exactly one value,
--     'candidate'. The checker's vocabulary (source_supported, conflicted, …) belongs to
--     phase 1E and cannot be written by this phase.
--
--  5. **Money is never counted twice and never quietly forgotten.** Reservation, spend
--     and the "unknown outcome" bucket are separate non-negative columns updated in the
--     same transaction as the ledger row that explains them.
--
-- One deliberate asymmetry: the *ceilings* are configuration, the *balances* are data.
-- `otdel.research_budgets` records what has actually happened (reserved, spent,
-- unknown); the bureau's limit is passed into each statement from
-- OTDEL_RESEARCH_BUDGET_MICROS, so raising it takes effect at once and no stale copy can
-- disagree with the configuration. A *plan's* budget is stored, because it was decided
-- when the owner approved that plan and a later configuration change must not
-- retroactively rewrite what that plan was allowed to do.

-- --------------------------------------------------------------------------
-- Budgets: one row per bureau, holding only what really happened.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.research_budgets (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id       uuid        NOT NULL UNIQUE REFERENCES otdel.bureaus (id) ON DELETE CASCADE,
    -- Money held for calls that are in flight right now, across every worker.
    reserved_micros bigint      NOT NULL DEFAULT 0 CHECK (reserved_micros >= 0),
    -- Money accounted for, including the part whose outcome nobody could confirm.
    spent_micros    bigint      NOT NULL DEFAULT 0 CHECK (spent_micros >= 0),
    -- The part of spent_micros awaiting reconciliation against the provider's own record
    -- (`docs/block-01-spec.md` §10: "либо неопределённый исход, требующий сверки").
    unknown_micros  bigint      NOT NULL DEFAULT 0 CHECK (unknown_micros >= 0),
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),

    CHECK (unknown_micros <= spent_micros)
);

ALTER TABLE otdel.research_budgets ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.research_budgets FORCE ROW LEVEL SECURITY;
CREATE POLICY research_budgets_bureau_isolation ON otdel.research_budgets
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Plans
--
-- One approved 1C question, turned into bounded research. The question is *copied*, not
-- only referenced: re-running the understanding of a material replaces its questions
-- (0004's replace_draft), and the research that was really done must survive that. The
-- foreign key therefore nulls itself out instead of cascading the plan away.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.research_plans (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id       uuid        NOT NULL,
    partner_id      uuid        NOT NULL,
    -- The material whose gap raised the question, so a plan stays traceable to the
    -- document that caused it.
    material_id     uuid        NOT NULL,
    question_id     uuid,
    -- The question as it stood when the owner approved it. This is what was researched.
    question_text   text        NOT NULL CHECK (char_length(btrim(question_text)) BETWEEN 1 AND 1000),
    topic           text        CHECK (topic IS NULL OR char_length(topic) <= 200),

    status          text        NOT NULL DEFAULT 'queued'
                                CHECK (status IN ('queued', 'running', 'completed', 'partial',
                                                  'failed', 'needs_provider', 'budget_exhausted',
                                                  'cancelled')),
    provider        text        CHECK (provider IS NULL OR char_length(provider) <= 100),
    model           text        CHECK (model IS NULL OR char_length(model) <= 200),
    prompt_profile  text        NOT NULL CHECK (char_length(prompt_profile) BETWEEN 1 AND 100),

    -- Passes already run, bounded by the configuration at claim time.
    passes          integer     NOT NULL DEFAULT 0 CHECK (passes >= 0),
    max_passes      integer     NOT NULL DEFAULT 2 CHECK (max_passes > 0),

    -- This plan's own ceiling, fixed when it was approved.
    budget_micros   bigint      NOT NULL DEFAULT 0 CHECK (budget_micros >= 0),
    reserved_micros bigint      NOT NULL DEFAULT 0 CHECK (reserved_micros >= 0),
    spent_micros    bigint      NOT NULL DEFAULT 0 CHECK (spent_micros >= 0),

    queries_made      integer   NOT NULL DEFAULT 0 CHECK (queries_made >= 0),
    results_seen      integer   NOT NULL DEFAULT 0 CHECK (results_seen >= 0),
    sources_fetched   integer   NOT NULL DEFAULT 0 CHECK (sources_fetched >= 0),
    sources_skipped   integer   NOT NULL DEFAULT 0 CHECK (sources_skipped >= 0),
    bytes_fetched     bigint    NOT NULL DEFAULT 0 CHECK (bytes_fetched >= 0),
    findings_rejected integer   NOT NULL DEFAULT 0 CHECK (findings_rejected >= 0),
    duration_ms       bigint    CHECK (duration_ms IS NULL OR duration_ms >= 0),

    -- Why candidates or sources were refused, in words. Bounded by the application to a
    -- few dozen deduplicated lines; the column keeps that honest.
    rejections      text[]      NOT NULL DEFAULT '{}'
                                CHECK (array_length(rejections, 1) IS NULL
                                       OR array_length(rejections, 1) <= 100),
    diagnostic      text        CHECK (diagnostic IS NULL OR char_length(diagnostic) <= 2000),
    -- The owner asked the run to stop; the worker settles at its next checkpoint, which
    -- is always *before* a chargeable call.
    cancel_requested boolean    NOT NULL DEFAULT false,

    started_at      timestamptz,
    finished_at     timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),

    -- One plan per approved question: pressing "исследовать" twice is one plan.
    -- (Several plans may carry NULL here once 1C has re-drafted their material.)
    UNIQUE (question_id),
    UNIQUE (bureau_id, id),
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    -- SET NULL with an explicit column list: without it PostgreSQL would null *every*
    -- referencing column, including the NOT NULL bureau_id, and the delete would fail.
    FOREIGN KEY (bureau_id, question_id)
        REFERENCES otdel.knowledge_questions (bureau_id, id) ON DELETE SET NULL (question_id)
);

CREATE INDEX research_plans_partner_idx ON otdel.research_plans (bureau_id, partner_id, updated_at DESC);
CREATE INDEX research_plans_question_idx ON otdel.research_plans (bureau_id, question_id);
CREATE INDEX research_plans_material_idx ON otdel.research_plans (bureau_id, partner_id, material_id);

ALTER TABLE otdel.research_plans ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.research_plans FORCE ROW LEVEL SECURITY;
CREATE POLICY research_plans_bureau_isolation ON otdel.research_plans
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Queries: the journal of what was actually asked of a search engine.
--
-- Including the ones that were *refused before being sent* — a query naming the partner,
-- or one that arrived after a limit was reached. "Why did this plan only make one
-- search?" has to be answerable from this table.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.research_queries (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    plan_id       uuid        NOT NULL,
    ordinal       integer     NOT NULL CHECK (ordinal >= 1),
    -- Exactly the text that was sent. Not a reconstruction.
    query_text    text        NOT NULL CHECK (char_length(btrim(query_text)) BETWEEN 1 AND 500),
    provider      text        NOT NULL CHECK (char_length(provider) BETWEEN 1 AND 100),
    results_count integer     NOT NULL DEFAULT 0 CHECK (results_count >= 0),
    cost_micros   bigint      NOT NULL DEFAULT 0 CHECK (cost_micros >= 0),
    outcome       text        NOT NULL CHECK (outcome IN ('ok', 'failed', 'unknown', 'refused')),
    diagnostic    text        CHECK (diagnostic IS NULL OR char_length(diagnostic) <= 1000),
    created_at    timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    -- A refused query never left the machine and therefore never cost anything.
    CHECK (outcome <> 'refused' OR cost_micros = 0),
    FOREIGN KEY (bureau_id, plan_id)
        REFERENCES otdel.research_plans (bureau_id, id) ON DELETE CASCADE
);

CREATE INDEX research_queries_plan_idx ON otdel.research_queries (bureau_id, plan_id, ordinal);

ALTER TABLE otdel.research_queries ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.research_queries FORCE ROW LEVEL SECURITY;
CREATE POLICY research_queries_bureau_isolation ON otdel.research_queries
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Sources: every URL that was discovered, and what became of it.
--
-- A row exists for a result that was never opened, with the reason. That is the point of
-- a research journal: "эта ссылка не читалась, потому что её хост не разрешён" is
-- information, and silently dropping the result would leave the owner thinking the
-- search found nothing.
--
-- `text_content` is the snapshot a citation is matched against. The original bytes are
-- not archived (see docs/research-1d.md, "Known limitations"); `content_hash` identifies
-- them, `retrieved_at` dates them.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.research_sources (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    partner_id    uuid        NOT NULL,
    plan_id       uuid        NOT NULL,
    query_id      uuid,

    -- Normalised absolute https URL: no credentials, no fragment, no non-default port.
    -- The length is a separate condition on purpose: PostgreSQL's regex engine caps a
    -- repetition count at 255, so `{1,2000}` inside the pattern is not a bound — it is a
    -- syntax error at first use.
    url           text        NOT NULL
                              CHECK (url ~ '^https://[^[:space:]]+$'
                                     AND char_length(url) BETWEEN 9 AND 2000),
    -- SHA-256 of the normalised URL; what makes one document one source within a plan.
    url_hash      text        NOT NULL CHECK (url_hash ~ '^[0-9a-f]{64}$'),
    host          text        NOT NULL CHECK (char_length(host) BETWEEN 1 AND 253),
    title         text        CHECK (title IS NULL OR char_length(title) <= 300),
    -- The search engine's own summary. Discovery, never evidence: no finding may cite it,
    -- and no evidence row points at it.
    snippet       text        CHECK (snippet IS NULL OR char_length(snippet) <= 600),

    status        text        NOT NULL DEFAULT 'discovered'
                              CHECK (status IN ('discovered', 'skipped_host', 'skipped_robots',
                                                'skipped_limit', 'skipped_type', 'fetched',
                                                'failed')),
    http_status   integer     CHECK (http_status IS NULL OR http_status BETWEEN 100 AND 599),
    content_type  text        CHECK (content_type IS NULL OR char_length(content_type) <= 200),
    content_bytes bigint      CHECK (content_bytes IS NULL OR content_bytes >= 0),
    content_chars integer     CHECK (content_chars IS NULL OR content_chars >= 0),
    content_hash  text        CHECK (content_hash IS NULL OR content_hash ~ '^[0-9a-f]{64}$'),
    -- The stored snapshot. Present exactly when the page was really read.
    text_content  text,
    -- Filled only when the page declares a licence. NULL means "not stated", never
    -- "free to use" — and `license_note` says which.
    license       text        CHECK (license IS NULL OR char_length(license) <= 300),
    license_note  text        CHECK (license_note IS NULL OR char_length(license_note) <= 300),
    retrieved_at  timestamptz,
    -- Only when the source itself states a date. Never inferred.
    published_at  timestamptz,
    cost_micros   bigint      NOT NULL DEFAULT 0 CHECK (cost_micros >= 0),
    diagnostic    text        CHECK (diagnostic IS NULL OR char_length(diagnostic) <= 1000),
    created_at    timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    -- Referenced by the evidence foreign key below: this is what forces a citation to
    -- name a source of its own plan.
    UNIQUE (bureau_id, plan_id, id),
    -- One document, one row, per plan.
    UNIQUE (plan_id, url_hash),
    -- Text and a retrieval time exist exactly when the page was fetched. A source that
    -- was not read cannot acquire a snapshot, and a fetched one cannot lack its date.
    CHECK ((status = 'fetched') = (text_content IS NOT NULL AND retrieved_at IS NOT NULL)),
    CHECK (status <> 'fetched' OR char_length(btrim(text_content)) > 0),

    FOREIGN KEY (bureau_id, plan_id)
        REFERENCES otdel.research_plans (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, partner_id)
        REFERENCES otdel.partners (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, query_id)
        REFERENCES otdel.research_queries (bureau_id, id) ON DELETE SET NULL (query_id)
);

CREATE INDEX research_sources_plan_idx ON otdel.research_sources (bureau_id, plan_id, created_at);
CREATE INDEX research_sources_partner_idx ON otdel.research_sources (bureau_id, partner_id);
CREATE INDEX research_sources_query_idx ON otdel.research_sources (bureau_id, query_id);
CREATE INDEX research_sources_host_idx ON otdel.research_sources (bureau_id, host);

ALTER TABLE otdel.research_sources ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.research_sources FORCE ROW LEVEL SECURITY;
CREATE POLICY research_sources_bureau_isolation ON otdel.research_sources
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Findings: candidate statements about the industry.
--
-- No product_id. No material_id. No attribute that could name a partner's article. The
-- separation the specification asks for (§6.5, "отраслевой контекст отделяется от
-- возможностей партнёра") is a property of this table's *shape*, not of a rule somebody
-- has to remember.
--
-- `partner_id` is here for scope and for the interface — these conclusions were
-- researched for that partner's gap — and says nothing about the partner's products.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.research_findings (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    partner_id    uuid        NOT NULL,
    plan_id       uuid        NOT NULL,
    -- One value. See the header of this file.
    scope         text        NOT NULL DEFAULT 'industry' CHECK (scope = 'industry'),
    -- One value. Verification statuses belong to 1E.
    status        text        NOT NULL DEFAULT 'candidate' CHECK (status = 'candidate'),
    topic         text        NOT NULL CHECK (char_length(btrim(topic)) BETWEEN 1 AND 200),
    attribute     text        NOT NULL CHECK (char_length(btrim(attribute)) BETWEEN 1 AND 200),
    value_text    text        NOT NULL CHECK (char_length(btrim(value_text)) BETWEEN 1 AND 200),
    unit          text        CHECK (unit IS NULL OR char_length(btrim(unit)) BETWEEN 1 AND 40),
    conditions    text        CHECK (conditions IS NULL OR char_length(conditions) <= 1000),
    model_context text        CHECK (model_context IS NULL OR char_length(model_context) <= 1000),
    created_at    timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    UNIQUE (bureau_id, plan_id, id),
    FOREIGN KEY (bureau_id, plan_id)
        REFERENCES otdel.research_plans (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, partner_id)
        REFERENCES otdel.partners (bureau_id, id) ON DELETE CASCADE
);

CREATE INDEX research_findings_plan_idx ON otdel.research_findings (bureau_id, plan_id);
CREATE INDEX research_findings_partner_idx ON otdel.research_findings (bureau_id, partner_id, created_at DESC);

ALTER TABLE otdel.research_findings ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.research_findings FORCE ROW LEVEL SECURITY;
CREATE POLICY research_findings_bureau_isolation ON otdel.research_findings
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Evidence: the verbatim fragment of a fetched page, with the offsets that locate it in
-- the stored snapshot.
--
-- Both foreign keys carry (bureau_id, plan_id). A conclusion of one plan citing a page
-- fetched by another — or by another bureau — is therefore not representable.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.research_evidence (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    plan_id       uuid        NOT NULL,
    finding_id    uuid        NOT NULL,
    source_id     uuid        NOT NULL,
    quote         text        NOT NULL CHECK (char_length(btrim(quote)) BETWEEN 1 AND 600),
    -- Character offsets into otdel.research_sources.text_content.
    char_start    integer     NOT NULL CHECK (char_start >= 0),
    char_end      integer     NOT NULL CHECK (char_end > char_start),
    created_at    timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    FOREIGN KEY (bureau_id, plan_id, finding_id)
        REFERENCES otdel.research_findings (bureau_id, plan_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, plan_id, source_id)
        REFERENCES otdel.research_sources (bureau_id, plan_id, id) ON DELETE CASCADE
);

CREATE INDEX research_evidence_finding_idx ON otdel.research_evidence (bureau_id, finding_id);
CREATE INDEX research_evidence_source_idx ON otdel.research_evidence (bureau_id, source_id);

ALTER TABLE otdel.research_evidence ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.research_evidence FORCE ROW LEVEL SECURITY;
CREATE POLICY research_evidence_bureau_isolation ON otdel.research_evidence
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- The spend ledger: one row per chargeable operation, from reservation to settlement.
--
-- The balances in otdel.research_budgets and otdel.research_plans are the running totals;
-- this is the explanation of how they got there. A row that stays 'reserved' after its
-- worker died is what the maintenance pass looks for.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.research_spend (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    plan_id       uuid        NOT NULL,
    kind          text        NOT NULL CHECK (kind IN ('search', 'fetch', 'model')),
    state         text        NOT NULL DEFAULT 'reserved'
                              CHECK (state IN ('reserved', 'settled', 'released', 'unknown')),
    amount_micros bigint      NOT NULL CHECK (amount_micros >= 0),
    note          text        CHECK (note IS NULL OR char_length(note) <= 500),
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    FOREIGN KEY (bureau_id, plan_id)
        REFERENCES otdel.research_plans (bureau_id, id) ON DELETE CASCADE
);

CREATE INDEX research_spend_plan_idx ON otdel.research_spend (bureau_id, plan_id, created_at);
CREATE INDEX research_spend_open_idx ON otdel.research_spend (bureau_id, updated_at)
    WHERE state = 'reserved';

ALTER TABLE otdel.research_spend ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.research_spend FORCE ROW LEVEL SECURITY;
CREATE POLICY research_spend_bureau_isolation ON otdel.research_spend
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Jobs: the research plan is a fourth kind of queued work.
--
-- It is the only kind that reaches outside this machine and the only one that spends
-- money, which is why the worker halves claim disjoint kinds
-- (`otdel_core::model::JobKind::research_kinds`).
-- --------------------------------------------------------------------------
ALTER TABLE otdel.jobs DROP CONSTRAINT jobs_kind_check;
ALTER TABLE otdel.jobs
    ADD CONSTRAINT jobs_kind_check
    CHECK (kind IN ('extract_document', 'extract_page', 'understand_material', 'research_plan'));

-- A research job names its plan, and only a research job does.
ALTER TABLE otdel.jobs ADD COLUMN research_plan_id uuid;
ALTER TABLE otdel.jobs
    ADD CONSTRAINT jobs_research_plan_matches_kind
    CHECK ((kind = 'research_plan') = (research_plan_id IS NOT NULL));
ALTER TABLE otdel.jobs
    ADD CONSTRAINT jobs_research_plan_fk
    FOREIGN KEY (bureau_id, research_plan_id)
        REFERENCES otdel.research_plans (bureau_id, id) ON DELETE CASCADE;

CREATE INDEX jobs_research_plan_idx ON otdel.jobs (bureau_id, research_plan_id)
    WHERE research_plan_id IS NOT NULL;

-- --------------------------------------------------------------------------
-- "No conclusion without an external source", enforced at commit time.
--
-- A finding and its evidence are written in one transaction, finding first, so the check
-- has to be deferred. It is a CONSTRAINT TRIGGER rather than application code because the
-- rule must hold for every writer, including a future one.
--
-- It runs with the caller's own rights on purpose, exactly as 0004's does: row-level
-- security is forced on these tables, so the check sees the rows the writing transaction
-- may see, and a transaction with no bureau context finds no evidence and gets an error
-- — the fail-closed direction.
-- --------------------------------------------------------------------------
CREATE FUNCTION otdel.assert_finding_has_evidence() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = otdel, pg_catalog, pg_temp
AS $$
DECLARE
    v_id  uuid;
    v_has boolean;
BEGIN
    -- On the finding table the row being checked is the finding itself (INSERT or
    -- UPDATE → NEW). On the evidence table the finding to re-check is the one the
    -- evidence *used to* belong to: a DELETE removes it, and an UPDATE can re-point the
    -- row at a different finding, leaving the old one unsourced.
    IF TG_TABLE_NAME = 'research_evidence' THEN
        v_id := OLD.finding_id;
    ELSE
        v_id := NEW.id;
    END IF;

    IF v_id IS NULL THEN
        RETURN NULL;
    END IF;

    -- The finding may legitimately be gone (a re-run replaced the whole set).
    IF NOT EXISTS (SELECT 1 FROM otdel.research_findings f WHERE f.id = v_id) THEN
        RETURN NULL;
    END IF;

    SELECT EXISTS (
        SELECT 1 FROM otdel.research_evidence e WHERE e.finding_id = v_id
    ) INTO v_has;

    IF NOT v_has THEN
        RAISE EXCEPTION
            'research finding % has no external evidence; a conclusion without a source is not stored',
            v_id
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;

    RETURN NULL;
END
$$;

-- INSERT *and* UPDATE: rewriting a finding must not be a way to keep a row whose evidence
-- has meanwhile gone.
CREATE CONSTRAINT TRIGGER research_findings_require_evidence
    AFTER INSERT OR UPDATE ON otdel.research_findings
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION otdel.assert_finding_has_evidence();

-- Removing the last evidence row of a surviving finding — or re-pointing it at a
-- different one — is the same violation seen from the other side.
CREATE CONSTRAINT TRIGGER research_evidence_keeps_findings_sourced
    AFTER DELETE OR UPDATE ON otdel.research_evidence
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION otdel.assert_finding_has_evidence();

-- --------------------------------------------------------------------------
-- Privileges for the runtime role.
--
-- DELETE is granted on the tables a re-run replaces (sources, findings, evidence and the
-- query journal of the pass being repeated). The plan row itself is never deleted by the
-- application — it is the record that the question was researched — and neither are the
-- budget or the ledger: money that was spent does not stop having been spent.
-- --------------------------------------------------------------------------
GRANT SELECT, INSERT, UPDATE         ON otdel.research_budgets  TO otdel_app;
GRANT SELECT, INSERT, UPDATE         ON otdel.research_plans    TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.research_queries  TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.research_sources  TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.research_findings TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.research_evidence TO otdel_app;
GRANT SELECT, INSERT, UPDATE         ON otdel.research_spend    TO otdel_app;

-- The trigger function is not a general-purpose entry point; only the role whose writes
-- fire it needs to execute it.
REVOKE ALL ON FUNCTION otdel.assert_finding_has_evidence() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION otdel.assert_finding_has_evidence() TO otdel_app;
