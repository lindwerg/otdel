-- OTDEL block 1, R05 — the productologist's product base: coverage, passports, senses,
-- application map, and the uncertainties a table leaves behind.
--
-- Migrations 0001–0008 are applied and frozen; everything here is additive and lives in
-- its own file. No column is dropped and no existing column changes meaning.
--
-- Why this exists. A real run over a 44-page catalogue produced 44 products, 13 facts,
-- 0 terms, 0 Q&A, 0 gaps — and left 8 pages out of the account entirely. Every one of
-- those numbers was *reported as success*, because nothing in the schema distinguished
-- "the material says nothing about prices" from "nobody looked", and nothing recorded
-- which pages had been offered to the model at all. A product base cannot be built on a
-- record that cannot tell those two apart.
--
-- Four properties are therefore enforced by the schema itself rather than left to the
-- application:
--
--  1. **Every page is accounted for.** `knowledge_page_coverage` has one row per page of
--     the material per run, carrying what happened to it and why. A page that is not in
--     that table is not "skipped": it makes the run's own counters disagree with the
--     rows, which the coverage state cannot then call complete.
--  2. **Silence is stated, never inferred from an empty array.** A run that produced no
--     glossary terms is indistinguishable from a run that never tried — unless it says
--     so. `knowledge_declarations` is where it says so, one row per topic, and the
--     readiness rule asks for rows *or* a declaration. An empty array alone satisfies
--     nothing.
--  3. **Nothing new is storable without its source.** Every table added here that makes
--     a claim carries `page_id`/`quote`/offsets as NOT NULL columns tied to the page by
--     a composite foreign key. There is no trigger to remember and no code path that can
--     write an unsourced alias, sense, parameter or identity link — it is not
--     representable. (0004's deferred trigger stays as it is; nothing here changes the
--     evidence table.)
--  4. **A run that ran out of budget is never "completed".** Pages left for a later pass
--     are counted in `pages_deferred`, and a CHECK forbids the pair
--     (`status = 'completed'`, `pages_deferred > 0`). Partial is the honest word and the
--     only one available.
--
-- Like 0004's rows, everything here is *derived* and cascades on delete: re-running the
-- understanding of a material replaces its candidates rather than accumulating copies.

-- --------------------------------------------------------------------------
-- Runs: what the pass actually covered, and what it cost.
-- --------------------------------------------------------------------------
ALTER TABLE otdel.knowledge_runs
    -- Pages the material has at all — the denominator the old record never had.
    ADD COLUMN pages_total     integer NOT NULL DEFAULT 0 CHECK (pages_total >= 0),
    -- Pages that carried quotable text and could therefore be offered to the model.
    ADD COLUMN pages_offered   integer NOT NULL DEFAULT 0 CHECK (pages_offered >= 0),
    -- Pages actually sent in a request that came back.
    ADD COLUMN pages_processed integer NOT NULL DEFAULT 0 CHECK (pages_processed >= 0),
    -- Pages left for a later pass because the request or cost budget ran out. These are
    -- queued, not dropped.
    ADD COLUMN pages_deferred  integer NOT NULL DEFAULT 0 CHECK (pages_deferred >= 0),
    -- Pages nobody could read: awaiting recognition, blank, or failed.
    ADD COLUMN pages_unreadable integer NOT NULL DEFAULT 0 CHECK (pages_unreadable >= 0),

    -- The verdict over the account above.
    --
    --   unknown           — not computed (a run from before this package, or one that
    --                       never got as far as planning).
    --   complete          — every page of the material was processed.
    --   partial_accounted — some pages were not processed, each with a stated reason,
    --                       and none of them was deferred for budget.
    --   incomplete        — pages remain deferred, or the account does not add up.
    --
    -- There is deliberately no value meaning "good enough".
    ADD COLUMN coverage_state text NOT NULL DEFAULT 'unknown'
        CHECK (coverage_state IN ('unknown', 'complete', 'partial_accounted', 'incomplete')),
    -- Why the state is not `complete`, in words, for the interface to show as-is.
    ADD COLUMN coverage_notes text[] NOT NULL DEFAULT '{}'
        CHECK (array_length(coverage_notes, 1) IS NULL OR array_length(coverage_notes, 1) <= 100),

    -- Whether the run produced the things a product passport has to have before anything
    -- may be published from it automatically, and which of them are missing.
    --
    -- Three values, not a boolean: a run from before this package, or one that failed
    -- before the check could be made, has *not* been judged, and `unknown` says so. A
    -- boolean would have to call that `false` — indistinguishable from "checked and
    -- found wanting" — which is the same conflation this package exists to remove.
    ADD COLUMN requirements_state text NOT NULL DEFAULT 'unknown'
        CHECK (requirements_state IN ('unknown', 'met', 'unmet')),
    ADD COLUMN requirements_missing text[] NOT NULL DEFAULT '{}'
        CHECK (array_length(requirements_missing, 1) IS NULL
               OR array_length(requirements_missing, 1) <= 50),

    -- What the pass cost, as the provider reported it. Integer micro-dollars: a float
    -- price is a rounding argument nobody needs, and NULL means "the provider did not
    -- say" rather than "free".
    ADD COLUMN prompt_tokens     integer CHECK (prompt_tokens IS NULL OR prompt_tokens >= 0),
    ADD COLUMN completion_tokens integer CHECK (completion_tokens IS NULL OR completion_tokens >= 0),
    ADD COLUMN cost_micro_usd    bigint  CHECK (cost_micro_usd IS NULL OR cost_micro_usd >= 0);

-- A run that left pages for later has not finished the material. This is the rule the
-- audited run broke: eight pages were never offered and the run still said `completed`.
ALTER TABLE otdel.knowledge_runs
    ADD CONSTRAINT knowledge_runs_deferred_pages_are_not_complete
    CHECK (status <> 'completed' OR pages_deferred = 0);

-- `complete` means what it says, in the counters as well as in the word.
ALTER TABLE otdel.knowledge_runs
    ADD CONSTRAINT knowledge_runs_complete_coverage_processed_everything
    CHECK (coverage_state <> 'complete'
           OR (pages_deferred = 0 AND pages_unreadable = 0 AND pages_processed = pages_total));

-- …and `partial_accounted` is only available while nothing is waiting on budget.
ALTER TABLE otdel.knowledge_runs
    ADD CONSTRAINT knowledge_runs_accounted_coverage_defers_nothing
    CHECK (coverage_state <> 'partial_accounted' OR pages_deferred = 0);

-- A state that is not `complete` has to say why. An empty explanation is how the
-- original defect presented itself.
ALTER TABLE otdel.knowledge_runs
    ADD CONSTRAINT knowledge_runs_unfinished_coverage_is_explained
    CHECK (coverage_state IN ('unknown', 'complete')
           OR coalesce(array_length(coverage_notes, 1), 0) >= 1);

-- Same rule for the content requirements, from both sides: `met` cannot carry a list of
-- things it is missing, and `unmet` cannot refuse to name them. `unknown` carries nothing
-- because nothing was decided.
ALTER TABLE otdel.knowledge_runs
    ADD CONSTRAINT knowledge_runs_requirements_are_named
    CHECK ((requirements_state = 'unmet')
           = (coalesce(array_length(requirements_missing, 1), 0) >= 1));

-- --------------------------------------------------------------------------
-- Page coverage: one row per page of the material, per run.
--
-- This is the table the audit asked for. It answers "what happened to page 37" without
-- an inference, and it is the only place a page can be *absent* from — which is itself
-- detectable, because `pages_total` is written from the page inventory, not from here.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.knowledge_page_coverage (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    partner_id    uuid        NOT NULL,
    material_id   uuid        NOT NULL,
    run_id        uuid        NOT NULL,
    page_id       uuid        NOT NULL,
    page_number   integer     NOT NULL CHECK (page_number >= 1),

    -- What became of this page. Every value except `processed` is a reason the page is
    -- not represented in the draft, and each is distinguishable from the others: the
    -- audit could not tell "awaiting OCR" from "ran out of requests" from "nobody
    -- looked", because all three looked the same — absent.
    disposition   text        NOT NULL
                              CHECK (disposition IN (
                                  'processed',           -- sent to the model, answer came back
                                  'deferred_budget',     -- request/cost cap reached; queued for the next pass
                                  'unreadable_needs_ocr',
                                  'unreadable_failed',
                                  'unreadable_empty',
                                  'not_read_yet',       -- inventoried, the reader has not reached it
                                  'not_offered_no_text', -- read, but the stored text is blank
                                  'excluded_by_request'  -- the caller restricted the pass to other pages
                              )),
    -- Whether the page was put in front of the model at all.
    offered       boolean     NOT NULL DEFAULT false,
    -- Characters of this page's text actually sent (0 when it was not).
    chars_sent    integer     NOT NULL DEFAULT 0 CHECK (chars_sent >= 0),
    -- Which request of the run carried it, 1-based. NULL when it was never sent.
    batch_index   integer     CHECK (batch_index IS NULL OR batch_index >= 1),
    -- One sentence the interface shows as-is. Required whenever the page was not
    -- processed: "не хватило бюджета запросов" is an account, silence is not.
    reason        text        CHECK (reason IS NULL OR char_length(reason) <= 500),
    created_at    timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    UNIQUE (run_id, page_id),

    -- A processed page was offered and carries a batch; an unprocessed one explains
    -- itself. Both halves matter: the first stops a page being counted as read without
    -- having been sent, the second stops a page vanishing without a word.
    CONSTRAINT knowledge_page_coverage_processed_was_offered
        CHECK (disposition <> 'processed' OR (offered AND batch_index IS NOT NULL)),
    CONSTRAINT knowledge_page_coverage_unprocessed_states_a_reason
        CHECK (disposition = 'processed' OR btrim(coalesce(reason, '')) <> ''),

    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, run_id)
        REFERENCES otdel.knowledge_runs (bureau_id, id) ON DELETE CASCADE,
    -- The page really belongs to this material of this bureau.
    FOREIGN KEY (bureau_id, material_id, page_id)
        REFERENCES otdel.material_pages (bureau_id, material_id, id) ON DELETE CASCADE
);

CREATE INDEX knowledge_page_coverage_run_idx
    ON otdel.knowledge_page_coverage (bureau_id, run_id, page_number);
CREATE INDEX knowledge_page_coverage_pending_idx
    ON otdel.knowledge_page_coverage (bureau_id, material_id, disposition);

ALTER TABLE otdel.knowledge_page_coverage ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.knowledge_page_coverage FORCE ROW LEVEL SECURITY;
CREATE POLICY knowledge_page_coverage_bureau_isolation ON otdel.knowledge_page_coverage
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Declarations: the explicit "there is none", per topic.
--
-- Without this table an empty `glossary` array means either "this catalogue introduces
-- no terms" or "the pass never produced any", and the difference is exactly what decides
-- whether a draft may be published without a person reading it. A declaration is a
-- statement the run makes on the record; the readiness rule accepts rows **or** a
-- declaration, and an empty array on its own satisfies nothing.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.knowledge_declarations (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    partner_id    uuid        NOT NULL,
    material_id   uuid        NOT NULL,
    run_id        uuid        NOT NULL,
    topic         text        NOT NULL
                              CHECK (topic IN ('glossary', 'questions', 'applications',
                                               'commercial_unknowns', 'technical_unknowns')),
    -- The reason, in the run's own words, shown to the owner unchanged.
    stated        text        NOT NULL CHECK (char_length(btrim(stated)) BETWEEN 1 AND 1000),
    -- Who said it: the model in its answer, or the server because the material itself
    -- made the answer unnecessary. Never blank — "somebody decided this" is the point.
    origin        text        NOT NULL DEFAULT 'model'
                              CHECK (origin IN ('model', 'server')),
    created_at    timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    UNIQUE (run_id, topic),
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, run_id)
        REFERENCES otdel.knowledge_runs (bureau_id, id) ON DELETE CASCADE
);

CREATE INDEX knowledge_declarations_partner_idx
    ON otdel.knowledge_declarations (bureau_id, partner_id, topic);

ALTER TABLE otdel.knowledge_declarations ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.knowledge_declarations FORCE ROW LEVEL SECURITY;
CREATE POLICY knowledge_declarations_bureau_isolation ON otdel.knowledge_declarations
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Product aliases and senses.
--
-- Spec §6.6 forbids collapsing synonyms without proof, and 0004 implements that by
-- scoping products to their material. What was missing is the other half: a catalogue
-- that writes «BP21» in the table and «Профиль BP 21 монтажный» in the prose has two
-- surface forms of something, and refusing to record the second one loses information
-- just as surely as merging them would invent it.
--
-- An alias is therefore a *recorded surface form*, never a merge. `relation` says how
-- far the claim goes, and `unclear` is a first-class answer. The provenance columns are
-- NOT NULL: an alias nobody can point at in the document is not representable.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.product_aliases (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id       uuid        NOT NULL,
    partner_id      uuid        NOT NULL,
    material_id     uuid        NOT NULL,
    run_id          uuid        NOT NULL,
    product_id      uuid        NOT NULL,
    surface         text        NOT NULL CHECK (char_length(btrim(surface)) BETWEEN 1 AND 200),
    normalised_surface text     NOT NULL CHECK (char_length(normalised_surface) BETWEEN 1 AND 200),
    -- alias        — the same thing written differently, and the page shows both;
    -- sense        — a narrower reading used in one context of this material;
    -- unclear      — it looks related and the document does not settle it. Kept apart on
    --                purpose: an unclear alias must never become a silent merge.
    relation        text        NOT NULL CHECK (relation IN ('alias', 'sense', 'unclear')),
    -- Free-text qualifier: which context this sense belongs to, or what is unclear.
    note            text        CHECK (note IS NULL OR char_length(note) <= 1000),

    -- Provenance, mandatory. The composite key below ties the page to this material.
    page_id         uuid        NOT NULL,
    page_number     integer     NOT NULL CHECK (page_number >= 1),
    quote           text        NOT NULL CHECK (char_length(btrim(quote)) BETWEEN 1 AND 600),
    char_start      integer     NOT NULL CHECK (char_start >= 0),
    char_end        integer     NOT NULL CHECK (char_end > char_start),
    created_at      timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    UNIQUE (product_id, normalised_surface),
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, run_id)
        REFERENCES otdel.knowledge_runs (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, product_id)
        REFERENCES otdel.products (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, material_id, page_id)
        REFERENCES otdel.material_pages (bureau_id, material_id, id) ON DELETE CASCADE
);

CREATE INDEX product_aliases_product_idx ON otdel.product_aliases (bureau_id, product_id);
CREATE INDEX product_aliases_surface_idx
    ON otdel.product_aliases (bureau_id, partner_id, normalised_surface);

ALTER TABLE otdel.product_aliases ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.product_aliases FORCE ROW LEVEL SECURITY;
CREATE POLICY product_aliases_bureau_isolation ON otdel.product_aliases
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Identity across materials.
--
-- Two materials of one partner produce two product rows for one profile, by design
-- (0004). A passport that ignores that shows the reader two half-products; a passport
-- that merges them on a matching string invents an identity nobody proved. The link row
-- is the third option: it records the *proposal*, states what it rests on, and keeps the
-- two rows separate.
--
-- `state = 'linked'` is unrepresentable without a page on each side. That is the whole
-- rule — "linked only by evidence" — expressed as a constraint rather than a convention.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.product_identity_links (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id       uuid        NOT NULL,
    partner_id      uuid        NOT NULL,
    product_id      uuid        NOT NULL,
    other_product_id uuid       NOT NULL,
    state           text        NOT NULL CHECK (state IN ('linked', 'unclear')),
    -- What the proposal rests on, named rather than scored. No confidence column: nothing
    -- here measures one.
    basis           text        NOT NULL
                              CHECK (basis IN ('identical_designation_quoted',
                                               'alias_quoted_in_both',
                                               'name_similarity_only')),
    note            text        CHECK (note IS NULL OR char_length(note) <= 1000),

    -- The page on each side that shows the designation. Required for a link.
    material_id     uuid,
    page_id         uuid,
    other_material_id uuid,
    other_page_id   uuid,
    created_at      timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    UNIQUE (product_id, other_product_id),
    CONSTRAINT product_identity_links_two_distinct_products
        CHECK (product_id <> other_product_id),
    -- Evidence on both sides, or it is not a link.
    CONSTRAINT product_identity_links_linked_needs_both_pages
        CHECK (state <> 'linked'
               OR (page_id IS NOT NULL AND other_page_id IS NOT NULL
                   AND material_id IS NOT NULL AND other_material_id IS NOT NULL)),
    -- A resemblance between names is not proof of identity, whatever else is attached.
    CONSTRAINT product_identity_links_similarity_is_never_a_link
        CHECK (state <> 'linked' OR basis <> 'name_similarity_only'),

    FOREIGN KEY (bureau_id, product_id)
        REFERENCES otdel.products (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, other_product_id)
        REFERENCES otdel.products (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, material_id, page_id)
        REFERENCES otdel.material_pages (bureau_id, material_id, id) ON DELETE SET NULL (material_id, page_id),
    FOREIGN KEY (bureau_id, other_material_id, other_page_id)
        REFERENCES otdel.material_pages (bureau_id, material_id, id) ON DELETE SET NULL (other_material_id, other_page_id)
);

CREATE INDEX product_identity_links_product_idx
    ON otdel.product_identity_links (bureau_id, product_id);
CREATE INDEX product_identity_links_partner_idx
    ON otdel.product_identity_links (bureau_id, partner_id, state);

ALTER TABLE otdel.product_identity_links ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.product_identity_links FORCE ROW LEVEL SECURITY;
CREATE POLICY product_identity_links_bureau_isolation ON otdel.product_identity_links
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Glossary: senses and synonyms.
--
-- 0004 gives a term one definition per material. A catalogue that uses «консоль» for a
-- bracket in one section and for a cantilever load scheme in another has two senses, and
-- storing one definition means the second usage is either wrong or missing. A sense is a
-- reading with its own evidence; a synonym is a surface form, recorded and never merged.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.glossary_senses (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id       uuid        NOT NULL,
    partner_id      uuid        NOT NULL,
    material_id     uuid        NOT NULL,
    run_id          uuid        NOT NULL,
    term_id         uuid        NOT NULL,
    -- Short disambiguator: «в контексте кабельных лотков». Not a number.
    label           text        NOT NULL CHECK (char_length(btrim(label)) BETWEEN 1 AND 200),
    normalised_label text       NOT NULL CHECK (char_length(normalised_label) BETWEEN 1 AND 200),
    definition      text        NOT NULL CHECK (char_length(btrim(definition)) BETWEEN 1 AND 1000),
    definition_is_model_context boolean NOT NULL DEFAULT true,

    page_id         uuid        NOT NULL,
    page_number     integer     NOT NULL CHECK (page_number >= 1),
    quote           text        NOT NULL CHECK (char_length(btrim(quote)) BETWEEN 1 AND 600),
    char_start      integer     NOT NULL CHECK (char_start >= 0),
    char_end        integer     NOT NULL CHECK (char_end > char_start),
    created_at      timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    UNIQUE (term_id, normalised_label),
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, run_id)
        REFERENCES otdel.knowledge_runs (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, term_id)
        REFERENCES otdel.glossary_terms (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, material_id, page_id)
        REFERENCES otdel.material_pages (bureau_id, material_id, id) ON DELETE CASCADE
);

CREATE INDEX glossary_senses_term_idx ON otdel.glossary_senses (bureau_id, term_id);

ALTER TABLE otdel.glossary_senses ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.glossary_senses FORCE ROW LEVEL SECURITY;
CREATE POLICY glossary_senses_bureau_isolation ON otdel.glossary_senses
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

CREATE TABLE otdel.glossary_synonyms (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id       uuid        NOT NULL,
    partner_id      uuid        NOT NULL,
    material_id     uuid        NOT NULL,
    run_id          uuid        NOT NULL,
    term_id         uuid        NOT NULL,
    surface         text        NOT NULL CHECK (char_length(btrim(surface)) BETWEEN 1 AND 200),
    normalised_surface text     NOT NULL CHECK (char_length(normalised_surface) BETWEEN 1 AND 200),
    -- Same discipline as a product alias: `unclear` is available and is not a merge.
    relation        text        NOT NULL CHECK (relation IN ('synonym', 'abbreviation', 'unclear')),

    page_id         uuid        NOT NULL,
    page_number     integer     NOT NULL CHECK (page_number >= 1),
    quote           text        NOT NULL CHECK (char_length(btrim(quote)) BETWEEN 1 AND 600),
    char_start      integer     NOT NULL CHECK (char_start >= 0),
    char_end        integer     NOT NULL CHECK (char_end > char_start),
    created_at      timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    UNIQUE (term_id, normalised_surface),
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, run_id)
        REFERENCES otdel.knowledge_runs (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, term_id)
        REFERENCES otdel.glossary_terms (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, material_id, page_id)
        REFERENCES otdel.material_pages (bureau_id, material_id, id) ON DELETE CASCADE
);

CREATE INDEX glossary_synonyms_term_idx ON otdel.glossary_synonyms (bureau_id, term_id);

ALTER TABLE otdel.glossary_synonyms ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.glossary_synonyms FORCE ROW LEVEL SECURITY;
CREATE POLICY glossary_synonyms_bureau_isolation ON otdel.glossary_synonyms
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- The application map: task → product → parameters → questions → constraints → sources.
--
-- The question a buyer actually arrives with is "чем закрепить лоток к бетону", not
-- "какая безопасная рабочая нагрузка у BP21". An application row is that task, tied to
-- the product the material offers for it; its details are the parameters that have to be
-- known, the constraints that limit it, and the questions that have to be asked because
-- the material does not settle them.
--
-- A parameter or a constraint is a claim and carries its page. A question is not a claim
-- about the product — it is what has to be asked — so it may stand without one.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.product_applications (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id       uuid        NOT NULL,
    partner_id      uuid        NOT NULL,
    material_id     uuid        NOT NULL,
    run_id          uuid        NOT NULL,
    -- NULL when the material states a task the offering as a whole serves.
    product_id      uuid,
    task            text        NOT NULL CHECK (char_length(btrim(task)) BETWEEN 1 AND 500),
    normalised_task text        NOT NULL CHECK (char_length(normalised_task) BETWEEN 1 AND 500),
    summary         text        CHECK (summary IS NULL OR char_length(summary) <= 1000),
    -- The model's own framing of the task, kept apart from the quoted evidence.
    model_context   text        CHECK (model_context IS NULL OR char_length(model_context) <= 1000),

    page_id         uuid        NOT NULL,
    page_number     integer     NOT NULL CHECK (page_number >= 1),
    quote           text        NOT NULL CHECK (char_length(btrim(quote)) BETWEEN 1 AND 600),
    char_start      integer     NOT NULL CHECK (char_start >= 0),
    char_end        integer     NOT NULL CHECK (char_end > char_start),
    created_at      timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, run_id)
        REFERENCES otdel.knowledge_runs (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, product_id)
        REFERENCES otdel.products (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, material_id, page_id)
        REFERENCES otdel.material_pages (bureau_id, material_id, id) ON DELETE CASCADE
);

-- One task per product per material. `product_id` is nullable, so the uniqueness lives in
-- an index with a coalesced key rather than a table constraint.
CREATE UNIQUE INDEX product_applications_task_is_unique
    ON otdel.product_applications
       (material_id, coalesce(product_id, '00000000-0000-0000-0000-000000000000'::uuid), normalised_task);
CREATE INDEX product_applications_product_idx
    ON otdel.product_applications (bureau_id, product_id);
CREATE INDEX product_applications_partner_idx
    ON otdel.product_applications (bureau_id, partner_id, created_at DESC);

ALTER TABLE otdel.product_applications ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.product_applications FORCE ROW LEVEL SECURITY;
CREATE POLICY product_applications_bureau_isolation ON otdel.product_applications
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

CREATE TABLE otdel.application_details (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id       uuid        NOT NULL,
    partner_id      uuid        NOT NULL,
    material_id     uuid        NOT NULL,
    run_id          uuid        NOT NULL,
    application_id  uuid        NOT NULL,
    kind            text        NOT NULL CHECK (kind IN ('parameter', 'constraint', 'question')),
    label           text        NOT NULL CHECK (char_length(btrim(label)) BETWEEN 1 AND 200),
    -- What the source says. NULL only for a question, which asserts nothing.
    value_text      text        CHECK (value_text IS NULL OR char_length(btrim(value_text)) BETWEEN 1 AND 200),
    unit            text        CHECK (unit IS NULL OR char_length(btrim(unit)) BETWEEN 1 AND 40),
    -- Who a question is for. Meaningless for the other kinds, and forbidden there.
    audience        text        CHECK (audience IS NULL OR audience IN ('partner', 'industry')),

    page_id         uuid,
    page_number     integer     CHECK (page_number IS NULL OR page_number >= 1),
    quote           text        CHECK (quote IS NULL OR char_length(btrim(quote)) BETWEEN 1 AND 600),
    char_start      integer     CHECK (char_start IS NULL OR char_start >= 0),
    char_end        integer,
    created_at      timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),

    -- A parameter or a constraint states something about the product, so it carries its
    -- page and its value. A question does neither.
    CONSTRAINT application_details_claims_are_sourced
        CHECK (kind = 'question'
               OR (page_id IS NOT NULL AND page_number IS NOT NULL
                   AND btrim(coalesce(quote, '')) <> ''
                   AND char_start IS NOT NULL AND char_end IS NOT NULL
                   AND char_end > char_start
                   AND btrim(coalesce(value_text, '')) <> '')),
    -- A question without an addressee cannot be routed (0004 makes the same rule for a
    -- gap's question); the other kinds have no addressee at all.
    CONSTRAINT application_details_audience_belongs_to_questions
        CHECK ((kind = 'question') = (audience IS NOT NULL)),

    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, run_id)
        REFERENCES otdel.knowledge_runs (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, application_id)
        REFERENCES otdel.product_applications (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, material_id, page_id)
        REFERENCES otdel.material_pages (bureau_id, material_id, id) ON DELETE CASCADE
);

CREATE INDEX application_details_application_idx
    ON otdel.application_details (bureau_id, application_id, kind);

ALTER TABLE otdel.application_details ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.application_details FORCE ROW LEVEL SECURITY;
CREATE POLICY application_details_bureau_isolation ON otdel.application_details
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Uncertainties: what the document says but nobody may read as a value.
--
-- R03 gave every table cell a verdict (`usable` / `ambiguous` / `unusable`) with reasons.
-- What was missing is the consumer: an ambiguous cell was simply not turned into a fact,
-- and therefore disappeared — the reader of the passport had no way to learn that page 37
-- holds a load table whose unit is written nowhere.
--
-- A gap (0004) records what the material does **not** say. An uncertainty records what it
-- *does* say in a form nobody may safely read. They are different questions and are kept
-- in different tables on purpose; neither is ever a fact.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.knowledge_uncertainties (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    partner_id    uuid        NOT NULL,
    material_id   uuid        NOT NULL,
    run_id        uuid        NOT NULL,
    product_id    uuid,
    kind          text        NOT NULL
                              CHECK (kind IN ('ambiguous_table_cell',
                                              'unreadable_page',
                                              'unresolved_unit',
                                              'unresolved_subject',
                                              'uninterpreted_diagram')),
    subject       text        NOT NULL CHECK (char_length(btrim(subject)) BETWEEN 1 AND 300),
    detail        text        NOT NULL CHECK (char_length(btrim(detail)) BETWEEN 1 AND 1000),
    -- The machine-readable reasons, straight from R03's vocabulary where there is one.
    reasons       text[]      NOT NULL DEFAULT '{}'
                              CHECK (array_length(reasons, 1) IS NULL OR array_length(reasons, 1) <= 20),
    -- The cell's own text when there is one. Not a claim — the point is that it cannot
    -- be read as one.
    quote         text        CHECK (quote IS NULL OR char_length(quote) <= 600),
    page_id       uuid,
    page_number   integer     CHECK (page_number IS NULL OR page_number >= 1),
    region_id     uuid,
    status        text        NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'resolved')),
    created_at    timestamptz NOT NULL DEFAULT now(),

    UNIQUE (bureau_id, id),
    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, run_id)
        REFERENCES otdel.knowledge_runs (bureau_id, id) ON DELETE CASCADE,
    FOREIGN KEY (bureau_id, product_id)
        REFERENCES otdel.products (bureau_id, id) ON DELETE SET NULL (product_id),
    FOREIGN KEY (bureau_id, material_id, page_id)
        REFERENCES otdel.material_pages (bureau_id, material_id, id) ON DELETE SET NULL (material_id, page_id),
    FOREIGN KEY (bureau_id, region_id)
        REFERENCES otdel.page_regions (bureau_id, id) ON DELETE SET NULL (region_id)
);

CREATE INDEX knowledge_uncertainties_partner_idx
    ON otdel.knowledge_uncertainties (bureau_id, partner_id, status, created_at DESC);
CREATE INDEX knowledge_uncertainties_run_idx
    ON otdel.knowledge_uncertainties (bureau_id, run_id);
CREATE INDEX knowledge_uncertainties_product_idx
    ON otdel.knowledge_uncertainties (bureau_id, product_id);

ALTER TABLE otdel.knowledge_uncertainties ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.knowledge_uncertainties FORCE ROW LEVEL SECURITY;
CREATE POLICY knowledge_uncertainties_bureau_isolation ON otdel.knowledge_uncertainties
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Facts: where the number came from, structurally.
--
-- R03 gave a table cell its subject, its property, its unit and its conditions, each with
-- its origin. R05 is the consumer: a fact read out of a cell can now say so and carry
-- that context, and a reader can tell it apart from a fact read out of running prose.
--
-- This is not a confidence score and not a quality grade. It is provenance of a second
-- kind — *which structure* the value sat in — alongside the quotation that was already
-- required. A fact with `structural_source = 'table_cell'` names the cell; one with
-- `page_text` says, honestly, that the structure was not established.
-- --------------------------------------------------------------------------

-- The composite key the foreign key below needs. Additive, and true of the existing rows
-- already (`id` is the primary key), so it cannot fail on live data.
ALTER TABLE otdel.table_cells
    ADD CONSTRAINT table_cells_bureau_scoped_id UNIQUE (bureau_id, id);

ALTER TABLE otdel.knowledge_facts
    ADD COLUMN structural_source text NOT NULL DEFAULT 'page_text'
        CHECK (structural_source IN ('page_text', 'table_cell')),
    ADD COLUMN source_cell_id uuid,
    -- The cell's own answers to "of what", "which property", "in what unit" and "when",
    -- copied at the moment the fact was accepted so a later re-read cannot silently
    -- change what this fact was checked against.
    ADD COLUMN structural_subject  text CHECK (structural_subject IS NULL OR char_length(structural_subject) <= 300),
    ADD COLUMN structural_property text CHECK (structural_property IS NULL OR char_length(structural_property) <= 300),
    ADD COLUMN structural_unit     text CHECK (structural_unit IS NULL OR char_length(structural_unit) <= 40),
    ADD COLUMN structural_conditions text[] NOT NULL DEFAULT '{}'
        CHECK (array_length(structural_conditions, 1) IS NULL
               OR array_length(structural_conditions, 1) <= 10);

-- Claiming a table origin means saying what the table said the value was *about*. The
-- subject and the property are copies, so they survive the cell; the cell id is a live
-- pointer and may not.
ALTER TABLE otdel.knowledge_facts
    ADD CONSTRAINT knowledge_facts_table_origin_names_its_context
    CHECK (structural_source <> 'table_cell'
           OR (btrim(coalesce(structural_subject, '')) <> ''
               AND btrim(coalesce(structural_property, '')) <> ''));

-- Re-reading a page replaces its cells, and the pointer then goes to NULL. Deliberately
-- *not* part of the constraint above: a fact must not become unstorable because the page
-- it came from was read again. It keeps its quotation and its recorded context, and loses
-- only the live link — which is exactly what happened.
ALTER TABLE otdel.knowledge_facts
    ADD CONSTRAINT knowledge_facts_source_cell_fk
    FOREIGN KEY (bureau_id, source_cell_id)
        REFERENCES otdel.table_cells (bureau_id, id) ON DELETE SET NULL (source_cell_id);

CREATE INDEX knowledge_facts_structural_idx
    ON otdel.knowledge_facts (bureau_id, partner_id, structural_source);

-- --------------------------------------------------------------------------
-- Gaps: which kind of unknown this is.
--
-- The requirement check asks two separate questions — "are the commercial unknowns
-- recorded" and "are the technical ones" — because a catalogue that states loads and no
-- prices must fail exactly one of them. Answering that by matching words against the gap's
-- free-text topic would put a lexicon in the publication path; the run classifies its own
-- gaps instead, and `other` is available so nothing has to be forced into a box.
-- --------------------------------------------------------------------------
ALTER TABLE otdel.knowledge_gaps
    ADD COLUMN nature text NOT NULL DEFAULT 'other'
        CHECK (nature IN ('commercial', 'technical', 'other'));

CREATE INDEX knowledge_gaps_nature_idx
    ON otdel.knowledge_gaps (bureau_id, partner_id, nature);

-- --------------------------------------------------------------------------
-- Privileges for the runtime role.
--
-- DELETE everywhere for the same reason 0004 grants it: re-running a material replaces
-- its draft. The run row is still never deleted by the application.
-- --------------------------------------------------------------------------
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.knowledge_page_coverage   TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.knowledge_declarations    TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.product_aliases           TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.product_identity_links    TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.glossary_senses           TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.glossary_synonyms         TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.product_applications      TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.application_details       TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.knowledge_uncertainties   TO otdel_app;
