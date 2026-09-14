-- OTDEL block 1, R03 — structural context of a table cell, and the page's revision.
--
-- Migrations 0001–0007 are applied and frozen; everything here is additive and lives in
-- its own file. No column is dropped and no existing column changes meaning, so a reader
-- written against 0003 keeps working: `raw_text`, `value_kind`, `unit`, `column_header`
-- and the four coordinates are exactly what they were.
--
-- Why this exists. Phase 1B stored a cell's verbatim text and nothing about what the cell
-- *was*. The string `безопасная рабочая нагрузка (Н)` — a column label — reached the
-- knowledge base as a product characteristic, because no stored field distinguished a
-- label from a measurement and the next phase fell back on flat page text, where the
-- product, the unit and the loading scheme are all lost. These columns carry that missing
-- half, and carry it *with its origin*, so an inference is never mistaken for a reading.

-- --------------------------------------------------------------------------
-- Pages: which reading produced the current regions, and what is on the page
-- besides text.
-- --------------------------------------------------------------------------
ALTER TABLE otdel.material_pages
    -- A fresh value every time the page is read. Evidence stored against a page can name
    -- the revision its coordinates came from, so a later re-read is recognisable as a
    -- different reading instead of silently replacing the one a fact was checked against.
    -- (R02 generalises this to a document-level revision line; this is the per-page
    -- anchor the source evidence needs now.)
    ADD COLUMN extraction_revision uuid,
    ADD COLUMN drawing_count integer NOT NULL DEFAULT 0 CHECK (drawing_count >= 0),
    -- Recognising the letters around a load diagram is not reading the diagram. There is
    -- deliberately no value here that means "understood": a phase that really interprets
    -- drawings has to add one, rather than quietly reusing 'not_attempted'.
    ADD COLUMN diagram_interpretation text NOT NULL DEFAULT 'none'
        CHECK (diagram_interpretation IN ('none', 'not_attempted'));

-- A page carrying no drawing cannot be awaiting interpretation, and a page carrying one
-- cannot claim there is nothing to interpret.
ALTER TABLE otdel.material_pages
    ADD CONSTRAINT material_pages_diagram_state_matches_drawings
    CHECK ((drawing_count > 0) = (diagram_interpretation = 'not_attempted'));

-- --------------------------------------------------------------------------
-- Table cells: role, verdict and structural context.
-- --------------------------------------------------------------------------
ALTER TABLE otdel.table_cells
    -- What the cell *is*. A header is a label; a label is never a value.
    ADD COLUMN role text NOT NULL DEFAULT 'data'
        CHECK (role IN ('column_header', 'row_header', 'data')),
    -- How far it may be trusted as a value. A category the parser can defend — there is
    -- deliberately no numeric confidence column, because nothing here measures one and a
    -- fabricated 0.87 is worse than an honest "the unit is written nowhere".
    ADD COLUMN usability text NOT NULL DEFAULT 'usable'
        CHECK (usability IN ('usable', 'ambiguous', 'unusable')),
    ADD COLUMN ambiguity_reasons text[] NOT NULL DEFAULT '{}',
    -- Column labels outermost first; a two-row header keeps `Нагрузка` and `кН` apart.
    ADD COLUMN column_header_path jsonb NOT NULL DEFAULT '[]'::jsonb,
    ADD COLUMN row_header_path    jsonb NOT NULL DEFAULT '[]'::jsonb,
    -- The product, the property and the unit as separate questions with separate
    -- evidence. Collapsing them into one string is how a label became a characteristic.
    ADD COLUMN subject  jsonb,
    ADD COLUMN property jsonb,
    ADD COLUMN unit_ref jsonb,
    ADD COLUMN conditions jsonb NOT NULL DEFAULT '[]'::jsonb;

-- --------------------------------------------------------------------------
-- Backfill, before the constraints below can be trusted.
--
-- Rows written by phase 1B predate the verdict and would otherwise default to "usable",
-- which is the very claim this package exists to stop. They are restated from what 1B did
-- record — `is_header` and a blank `raw_text` — and nothing is invented beyond that. Their
-- remaining context stays empty and their pages keep `extraction_revision = NULL`: they
-- were read by an earlier revision, and saying so is the honest result. Re-reading a page
-- replaces its cells outright and fills the rest in.
-- --------------------------------------------------------------------------
UPDATE otdel.table_cells
   SET role = 'column_header',
       usability = 'unusable',
       ambiguity_reasons = ARRAY['header_is_not_a_value']
 WHERE is_header;

UPDATE otdel.table_cells
   SET usability = 'unusable',
       ambiguity_reasons =
           (SELECT array_agg(DISTINCT reason)
              FROM unnest(ambiguity_reasons || ARRAY['blank_cell']) AS reason)
 WHERE value_kind = 'empty'
   AND usability <> 'unusable';

-- Everything else read by the old extractor has no proven column header — that is exactly
-- what was missing — so it is doubtful rather than usable.
UPDATE otdel.table_cells
   SET usability = 'ambiguous',
       ambiguity_reasons = ARRAY['no_column_header']
 WHERE usability = 'usable'
   AND column_header IS NULL;

UPDATE otdel.table_cells
   SET usability = 'ambiguous',
       ambiguity_reasons = ARRAY['unit_unresolved']
 WHERE usability = 'usable'
   AND unit IS NULL;

-- The remainder (a header, a unit and a non-blank value all present) keeps `usable` with
-- no reasons, which the constraint below requires.

-- The verdict is derived from the reasons, so the two can never drift apart in storage
-- either: no reasons means usable, and a usable cell has nothing to explain.
ALTER TABLE otdel.table_cells
    ADD CONSTRAINT table_cells_verdict_agrees_with_reasons
    CHECK ((usability = 'usable') = (cardinality(ambiguity_reasons) = 0));

-- A header cell is never usable as a value. Enforced in the schema rather than only in
-- the extractor: this is the rule the reported defect broke, and it should not depend on
-- which code path wrote the row.
ALTER TABLE otdel.table_cells
    ADD CONSTRAINT table_cells_a_header_is_not_a_value
    CHECK (role = 'data' OR usability = 'unusable');

-- A blank cell is never a value either. Blank stays blank; it does not become a zero.
ALTER TABLE otdel.table_cells
    ADD CONSTRAINT table_cells_blank_is_not_a_value
    CHECK (value_kind <> 'empty' OR usability = 'unusable');

-- The path/context columns are arrays and objects, not scalars: catching a malformed
-- write here is cheaper than discovering it in a consumer.
ALTER TABLE otdel.table_cells
    ADD CONSTRAINT table_cells_context_paths_are_arrays
    CHECK (
        jsonb_typeof(column_header_path) = 'array'
        AND jsonb_typeof(row_header_path) = 'array'
        AND jsonb_typeof(conditions) = 'array'
        AND (subject  IS NULL OR jsonb_typeof(subject)  = 'object')
        AND (property IS NULL OR jsonb_typeof(property) = 'object')
        AND (unit_ref IS NULL OR jsonb_typeof(unit_ref) = 'object')
    );

-- Finding the cells a person still has to settle is a routine query for the review
-- interface, and it is bureau-scoped like everything else.
CREATE INDEX table_cells_usability_idx ON otdel.table_cells (bureau_id, region_id, usability);

-- No new grants: the runtime role's privileges on these tables are unchanged, and RLS is
-- already enabled and forced on all three (0003).
