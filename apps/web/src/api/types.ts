// Exact shapes from docs/implementation-contract.md ("API этапа 1A").
// snake_case fields are kept as-is (not remapped) to stay a literal mirror of
// the wire contract, and to avoid mapping bugs.

export interface SessionResponse {
  authenticated: true
  csrf_token: string
}

export interface Partner {
  id: string
  name: string
  note: string | null
  created_at: string
  updated_at: string
}

/** Material processing status (see implementation-contract.md). */
export type MaterialStatus =
  | 'queued'
  | 'processing'
  | 'completed'
  | 'partial'
  | 'failed'
  | 'quarantined'

/**
 * Phase 1B roll-up of a material's page outcomes.
 *
 * `null` on a material means it has not been read at all — deliberately not a
 * summary full of zeros, which would read as "nothing wrong, nothing found".
 * The counters come from the page rows on the server, so they never claim more
 * than the pages themselves say.
 */
export interface ExtractionSummary {
  pages_total: number
  pages_extracted: number
  pages_empty: number
  pages_needs_ocr: number
  pages_partial: number
  pages_failed: number
  pages_pending: number
  parser_name: string | null
  parser_version: string | null
  ocr_engine: string | null
  ocr_version: string | null
  started_at: string | null
  finished_at: string | null
  diagnostic: string | null
}

export interface Material {
  id: string
  partner_id: string
  filename: string
  media_type: string
  size_bytes: number
  sha256: string
  status: MaterialStatus
  page_count: number | null
  created_at: string
  error: string | null
  extraction: ExtractionSummary | null
}

export interface Job {
  id: string
  partner_id: string
  material_id: string
  page_number: number | null
  kind: string
  status: string
  stage: string | null
  attempts: number
  created_at: string
  updated_at: string
  error: string | null
}

// --- Phase 1B: pages and their evidence ------------------------------------

/** Outcome of reading one page. Every page has exactly one. */
export type PageStatus = 'pending' | 'extracted' | 'empty' | 'needs_ocr' | 'partial' | 'failed'

/** Where a page's text came from. `none` means no text was obtained. */
export type TextSource = 'none' | 'text_layer' | 'ocr'

export type RegionKind = 'heading' | 'paragraph' | 'footnote' | 'table'

/** Classification of a cell's verbatim text — never a parsed value. */
export type CellValueKind = 'empty' | 'number' | 'text'

/** Rectangle in PDF user space (origin bottom-left, units = points). */
export interface BoundingBox {
  x0: number
  y0: number
  x1: number
  y1: number
}

export interface MaterialPage {
  id: string
  material_id: string
  page_number: number
  status: PageStatus
  text_source: TextSource
  char_count: number
  word_count: number
  image_count: number
  width_pt: number | null
  height_pt: number | null
  rotation: number
  parser_name: string | null
  parser_version: string | null
  ocr_engine: string | null
  ocr_version: string | null
  ocr_language: string | null
  duration_ms: number | null
  attempts: number
  /** The server's own explanation of the status, in words. Shown as-is. */
  diagnostic: string | null
  extracted_at: string | null
  region_count: number
  table_count: number
}

export interface TableCell {
  id: string
  region_id: string
  row_index: number
  column_index: number
  is_header: boolean
  /** The verbatim source fragment. Rendered as-is; never reformatted. */
  raw_text: string
  value_kind: CellValueKind
  unit: string | null
  column_header: string | null
  bbox: BoundingBox | null
}

export interface PageRegion {
  id: string
  page_id: string
  page_number: number
  ordinal: number
  kind: RegionKind
  text: string
  source: TextSource
  /** `null` when the adapter could not place the region — not a guessed box. */
  bbox: BoundingBox | null
  row_count: number | null
  column_count: number | null
  cells: TableCell[]
}

export interface PageDetail {
  page: MaterialPage
  text: string | null
  regions: PageRegion[]
}

export interface ApiErrorBody {
  error: {
    code: string
    message: string
    retryable: boolean
  }
}

export interface ListResponse<T> {
  items: T[]
}
