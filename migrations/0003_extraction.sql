-- OTDEL block 1, phase 1B — per-page reading of stored originals.
--
-- Applied by the *migration* role, like 0001/0002. Migrations 0001 and 0002 are frozen
-- (they have been applied to the pilot database with real data), so every change here is
-- additive and lives in its own file.
--
-- Three derived tables are added: pages, structural regions on a page, and the cells of
-- extracted tables. They are *derived* data — they can be recomputed from the original —
-- which is why they cascade on delete and why the runtime role may DELETE from the two
-- innermost ones: re-reading a page must be able to replace its previous regions instead
-- of accumulating duplicates. The originals themselves remain undeletable by the
-- application.

-- --------------------------------------------------------------------------
-- Materials: bookkeeping of the extraction run.
--
-- No aggregate counters are stored here. The per-status page counts the API returns are
-- computed from otdel.material_pages on read, so they cannot drift away from the pages
-- they claim to summarise, and a partially written run cannot leave behind a number that
-- says "32 pages read" when the page rows say otherwise.
-- --------------------------------------------------------------------------
ALTER TABLE otdel.materials
    ADD COLUMN extraction_started_at  timestamptz,
    ADD COLUMN extraction_finished_at timestamptz,
    ADD COLUMN parser_name            text CHECK (parser_name IS NULL OR char_length(parser_name) <= 100),
    ADD COLUMN parser_version         text CHECK (parser_version IS NULL OR char_length(parser_version) <= 100),
    ADD COLUMN ocr_engine             text CHECK (ocr_engine IS NULL OR char_length(ocr_engine) <= 100),
    ADD COLUMN ocr_version            text CHECK (ocr_version IS NULL OR char_length(ocr_version) <= 100),
    ADD COLUMN extraction_diagnostic  text CHECK (extraction_diagnostic IS NULL OR char_length(extraction_diagnostic) <= 2000);

-- --------------------------------------------------------------------------
-- Jobs: single-page repeats and a transient/permanent distinction for retries.
-- --------------------------------------------------------------------------
ALTER TABLE otdel.jobs DROP CONSTRAINT jobs_kind_check;
ALTER TABLE otdel.jobs
    ADD CONSTRAINT jobs_kind_check CHECK (kind IN ('extract_document', 'extract_page'));

ALTER TABLE otdel.jobs
    ADD COLUMN page_number integer CHECK (page_number IS NULL OR page_number >= 1),
    -- `permanent` means "repeating this exact job cannot help" (a file that is not a
    -- PDF); `transient` means it can (the object store blinked). The worker uses it to
    -- decide between scheduling another attempt and stopping.
    ADD COLUMN error_kind  text CHECK (error_kind IS NULL OR error_kind IN ('transient', 'permanent'));

-- A page job names a page; a document job never does. Making the mismatch
-- unrepresentable keeps the worker from having to guess what an `extract_page` row with
-- no page number was supposed to mean.
ALTER TABLE otdel.jobs
    ADD CONSTRAINT jobs_page_number_matches_kind
    CHECK ((kind = 'extract_page') = (page_number IS NOT NULL));

-- --------------------------------------------------------------------------
-- Pages
--
-- One row per page of a material, with an explicit outcome. `status` has no value that
-- means "fine, probably": a page is extracted, genuinely empty, awaiting recognition,
-- knowingly incomplete, or failed. `diagnostic` carries the reason in words.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.material_pages (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    partner_id    uuid        NOT NULL,
    material_id   uuid        NOT NULL,
    page_number   integer     NOT NULL CHECK (page_number >= 1),
    status        text        NOT NULL DEFAULT 'pending'
                              CHECK (status IN ('pending', 'extracted', 'empty', 'needs_ocr', 'partial', 'failed')),
    text_source   text        NOT NULL DEFAULT 'none'
                              CHECK (text_source IN ('none', 'text_layer', 'ocr')),
    text_content  text,
    char_count    integer     NOT NULL DEFAULT 0 CHECK (char_count >= 0),
    word_count    integer     NOT NULL DEFAULT 0 CHECK (word_count >= 0),
    image_count   integer     NOT NULL DEFAULT 0 CHECK (image_count >= 0),
    -- Page geometry in PDF points, so a stored region can be placed on the original.
    width_pt      double precision CHECK (width_pt IS NULL OR width_pt > 0),
    height_pt     double precision CHECK (height_pt IS NULL OR height_pt > 0),
    rotation      integer     NOT NULL DEFAULT 0 CHECK (rotation IN (0, 90, 180, 270)),
    -- Which adapter produced this, and in which version: a later re-run with a newer
    -- parser must be distinguishable from the old result (spec §6.1).
    parser_name   text        CHECK (parser_name IS NULL OR char_length(parser_name) <= 100),
    parser_version text       CHECK (parser_version IS NULL OR char_length(parser_version) <= 100),
    ocr_engine    text        CHECK (ocr_engine IS NULL OR char_length(ocr_engine) <= 100),
    ocr_version   text        CHECK (ocr_version IS NULL OR char_length(ocr_version) <= 100),
    ocr_language  text        CHECK (ocr_language IS NULL OR char_length(ocr_language) <= 100),
    duration_ms   integer     CHECK (duration_ms IS NULL OR duration_ms >= 0),
    attempts      integer     NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    diagnostic    text        CHECK (diagnostic IS NULL OR char_length(diagnostic) <= 2000),
    extracted_at  timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),

    -- Re-reading page 7 updates page 7. Repeating a run can never produce a second row
    -- for the same page, which is what makes retry idempotent at the storage level.
    UNIQUE (material_id, page_number),
    UNIQUE (bureau_id, id),
    -- Referenced by page_regions: a region must belong to a page of the same bureau and
    -- the same material it claims.
    UNIQUE (bureau_id, material_id, id),

    -- A page either has text and says where it came from, or has neither. Whitespace is
    -- not text: `text_source = 'text_layer'` with a blank body would be a parser that
    -- gave up while looking like it succeeded.
    CONSTRAINT material_pages_text_source_agrees_with_text
        CHECK ((text_source = 'none') = (btrim(coalesce(text_content, '')) = '')),
    -- Claiming OCR requires naming the engine that did it. There is no way to record
    -- "recognised" without recording *what* recognised it.
    CONSTRAINT material_pages_ocr_claims_name_an_engine
        CHECK (text_source <> 'ocr' OR ocr_engine IS NOT NULL),

    FOREIGN KEY (bureau_id, partner_id, material_id)
        REFERENCES otdel.materials (bureau_id, partner_id, id) ON DELETE CASCADE
);

CREATE INDEX material_pages_material_idx ON otdel.material_pages (material_id, page_number);
CREATE INDEX material_pages_status_idx ON otdel.material_pages (bureau_id, material_id, status);

ALTER TABLE otdel.material_pages ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.material_pages FORCE ROW LEVEL SECURITY;
CREATE POLICY material_pages_bureau_isolation ON otdel.material_pages
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Source regions
--
-- A block of a page with its coordinates when the adapter could place it. Coordinates
-- are nullable on purpose: an adapter that does not know where something is says so
-- instead of reporting a rectangle it invented.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.page_regions (
    id           uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id    uuid        NOT NULL,
    material_id  uuid        NOT NULL,
    page_id      uuid        NOT NULL,
    page_number  integer     NOT NULL CHECK (page_number >= 1),
    ordinal      integer     NOT NULL CHECK (ordinal >= 0),
    kind         text        NOT NULL CHECK (kind IN ('heading', 'paragraph', 'footnote', 'table')),
    text_content text        NOT NULL DEFAULT '',
    source       text        NOT NULL CHECK (source IN ('text_layer', 'ocr')),
    x0           double precision,
    y0           double precision,
    x1           double precision,
    y1           double precision,
    row_count    integer     CHECK (row_count IS NULL OR row_count > 0),
    column_count integer     CHECK (column_count IS NULL OR column_count > 0),
    created_at   timestamptz NOT NULL DEFAULT now(),

    UNIQUE (page_id, ordinal),
    UNIQUE (bureau_id, id),

    -- Either the whole rectangle is known or none of it is.
    CONSTRAINT page_regions_bbox_is_complete
        CHECK (num_nonnulls(x0, y0, x1, y1) IN (0, 4)),
    CONSTRAINT page_regions_bbox_is_ordered
        CHECK (x0 IS NULL OR (x1 >= x0 AND y1 >= y0)),
    -- Only a table has a shape, and a table always has one.
    CONSTRAINT page_regions_shape_belongs_to_tables
        CHECK ((kind = 'table') = (row_count IS NOT NULL AND column_count IS NOT NULL)),

    FOREIGN KEY (bureau_id, material_id, page_id)
        REFERENCES otdel.material_pages (bureau_id, material_id, id) ON DELETE CASCADE
);

CREATE INDEX page_regions_page_idx ON otdel.page_regions (page_id, ordinal);
CREATE INDEX page_regions_material_idx ON otdel.page_regions (bureau_id, material_id, page_number);

ALTER TABLE otdel.page_regions ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.page_regions FORCE ROW LEVEL SECURITY;
CREATE POLICY page_regions_bureau_isolation ON otdel.page_regions
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Table cells
--
-- `raw_text` is the verbatim fragment of the source. There is deliberately **no numeric
-- column**: parsing "21/21D" or "40 … 60" into a number is a judgement the extractor is
-- not entitled to make, and a blank cell must stay blank rather than become a zero
-- (spec §6.4). `value_kind` only classifies the text; `unit` is recorded only when a
-- unit is literally present in the cell or in its column header, which is itself kept
-- verbatim in `column_header`.
-- --------------------------------------------------------------------------
CREATE TABLE otdel.table_cells (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    bureau_id     uuid        NOT NULL,
    region_id     uuid        NOT NULL,
    row_index     integer     NOT NULL CHECK (row_index >= 0),
    column_index  integer     NOT NULL CHECK (column_index >= 0),
    is_header     boolean     NOT NULL DEFAULT false,
    raw_text      text        NOT NULL,
    value_kind    text        NOT NULL CHECK (value_kind IN ('empty', 'number', 'text')),
    unit          text        CHECK (unit IS NULL OR char_length(unit) <= 40),
    column_header text,
    x0            double precision,
    y0            double precision,
    x1            double precision,
    y1            double precision,

    UNIQUE (region_id, row_index, column_index),

    CONSTRAINT table_cells_empty_means_blank
        CHECK ((value_kind = 'empty') = (btrim(raw_text) = '')),
    CONSTRAINT table_cells_bbox_is_complete
        CHECK (num_nonnulls(x0, y0, x1, y1) IN (0, 4)),

    FOREIGN KEY (bureau_id, region_id)
        REFERENCES otdel.page_regions (bureau_id, id) ON DELETE CASCADE
);

CREATE INDEX table_cells_region_idx ON otdel.table_cells (region_id, row_index, column_index);

ALTER TABLE otdel.table_cells ENABLE ROW LEVEL SECURITY;
ALTER TABLE otdel.table_cells FORCE ROW LEVEL SECURITY;
CREATE POLICY table_cells_bureau_isolation ON otdel.table_cells
    USING (bureau_id = otdel.current_bureau_id())
    WITH CHECK (bureau_id = otdel.current_bureau_id());

-- --------------------------------------------------------------------------
-- Privileges for the runtime role.
--
-- DELETE is granted on the two derived tables only. Re-reading a page replaces its
-- regions and cells; nothing in the application may delete a material, a partner or a
-- page row, so the original and the record that a page exists at all stay put.
-- --------------------------------------------------------------------------
GRANT SELECT, INSERT, UPDATE         ON otdel.material_pages TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.page_regions   TO otdel_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON otdel.table_cells    TO otdel_app;
