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

// --- Phase 1C: the product knowledge draft ---------------------------------

/**
 * State of the model adapter.
 *
 * `needs_configuration` is the normal state of this pilot until an OpenRouter
 * key is supplied: extraction keeps working, and the product role says what it
 * is waiting for instead of pretending to work. No response ever contains the
 * key itself — only the host that would be called.
 */
export interface ProviderState {
  state: 'ready' | 'needs_configuration' | 'disabled'
  provider: string
  model: string | null
  endpoint_host: string | null
  /** Environment variables the owner still has to set. */
  missing: string[]
  message: string
}

export type KnowledgeRunStatus =
  | 'queued'
  | 'running'
  | 'completed'
  | 'partial'
  | 'failed'
  | 'needs_provider'

export interface KnowledgeRun {
  id: string
  partner_id: string
  material_id: string
  /** File name of the material this run drafted, so the run can be named. */
  material_filename: string
  status: KnowledgeRunStatus
  provider: string | null
  model: string | null
  prompt_profile: string
  pages_considered: number
  requests_made: number
  input_chars: number
  categories_created: number
  products_created: number
  facts_accepted: number
  facts_rejected: number
  terms_created: number
  qa_created: number
  gaps_created: number
  questions_created: number
  /** Why candidates were refused, in the server's own words. Shown as-is. */
  rejections: string[]
  diagnostic: string | null
  started_at: string | null
  finished_at: string | null
  created_at: string
}

export interface KnowledgeSummary {
  categories_total: number
  products_total: number
  facts_total: number
  terms_total: number
  qa_total: number
  gaps_total: number
  questions_total: number
  materials_readable: number
  materials_understood: number
}

/** A read material that has never been drafted — the entry point for a first draft. */
export interface DraftableMaterial {
  material_id: string
  filename: string
  pages_with_text: number
}

export interface KnowledgeOverview {
  provider: ProviderState
  summary: KnowledgeSummary
  runs: KnowledgeRun[]
  pending_materials: DraftableMaterial[]
}

/**
 * A verbatim fragment of a page, with the offsets that locate it there.
 *
 * `quote` is the page's own wording — the server stores the fragment it matched,
 * not the model's rendering of it — which is what makes "открыть факт и увидеть
 * цитату" trustworthy.
 */
export interface FactEvidence {
  id: string
  material_id: string
  material_filename: string
  page_id: string
  page_number: number
  region_id: string | null
  quote: string
  char_start: number
  char_end: number
}

export type FactKind = 'characteristic' | 'limitation' | 'application' | 'commercial'
export type CategoryKind = 'direction' | 'family'
export type ProductKind = 'product' | 'service'
export type QuestionAudience = 'partner' | 'industry'

export interface ProductCategory {
  id: string
  partner_id: string
  material_id: string
  run_id: string
  kind: CategoryKind
  name: string
  summary: string | null
  created_at: string
}

export interface Product {
  id: string
  partner_id: string
  material_id: string
  run_id: string
  category_id: string | null
  kind: ProductKind
  name: string
  summary: string | null
  created_at: string
}

export interface KnowledgeFact {
  id: string
  partner_id: string
  material_id: string
  run_id: string
  product_id: string | null
  product_name: string | null
  kind: FactKind
  /** Always `candidate` in phase 1C: nothing here has been verified. */
  status: 'candidate'
  attribute: string
  /** The value exactly as the source writes it. Never reformatted. */
  value_text: string
  unit: string | null
  conditions: string | null
  /** The model's own words. Shown as explicitly not a quotation. */
  model_context: string | null
  evidence: FactEvidence[]
  created_at: string
}

/** A product with the facts drafted about it; `product` is null for facts about
 *  the partner's offering as a whole. */
export interface ProductNode {
  product: Product | null
  category: ProductCategory | null
  facts: KnowledgeFact[]
}

export interface GlossaryTerm {
  id: string
  partner_id: string
  material_id: string
  run_id: string
  term: string
  definition: string
  /** true when the definition is the model's wording, not the source's. */
  definition_is_model_context: boolean
  evidence: FactEvidence[]
  created_at: string
}

export interface QaEntry {
  id: string
  partner_id: string
  material_id: string
  run_id: string
  question: string
  answer: string
  /** true when the answer is the model's own wording rather than the source's. */
  answer_is_model_context: boolean
  evidence: FactEvidence[]
  created_at: string
}

export interface PreparedQuestion {
  id: string
  audience: QuestionAudience
  text: string
  /** `prepared` in this phase: nothing is sent and nothing is researched yet. */
  status: string
  created_at: string
}

export interface KnowledgeGap {
  id: string
  partner_id: string
  material_id: string
  run_id: string
  product_id: string | null
  product_name: string | null
  topic: string
  missing: string
  blocks: string | null
  question: PreparedQuestion | null
  created_at: string
}

// --- Phase 1D: bounded industry research -----------------------------------

/**
 * State of one 1D adapter. Never contains a key — the server does not send one.
 */
export interface ResearchAdapterState {
  state: 'ready' | 'needs_configuration' | 'disabled'
  provider: string
  endpoint_host: string | null
  model: string | null
  message: string
}

/** The bounds of one plan, stated before anything runs. */
export interface ResearchLimits {
  max_queries_per_plan: number
  max_results_per_query: number
  max_sources_per_plan: number
  max_page_bytes: number
  max_page_chars: number
  request_timeout_seconds: number
  plan_time_budget_seconds: number
  max_passes_per_plan: number
}

/**
 * Whether the researcher can run at all.
 *
 * `ready` requires all three halves: a search endpoint, a declared host
 * allowlist, and a model to interpret what was read. `needs_configuration` is
 * the normal state of this pilot — no search provider has been chosen — and it
 * means **nothing leaves the machine and no budget is reserved**.
 */
export interface ResearchProviderState {
  state: 'ready' | 'needs_configuration' | 'disabled'
  search: ResearchAdapterState
  fetcher: ResearchAdapterState
  model: ResearchAdapterState
  /** Environment variables the owner still has to set. */
  missing: string[]
  /** Hosts the researcher may read, exactly as declared. */
  allowed_hosts: string[]
  limits: ResearchLimits
  message: string
}

/**
 * The bureau's research money, in millionths of one currency unit.
 *
 * Integers, never floats: a budget compared as a float eventually lets one more
 * paid call through than it should. The prices are the owner's *declared*
 * tariff — not a provider's invoice — and the interface says so.
 */
export interface ResearchBudget {
  currency: string
  limit_micros: number
  reserved_micros: number
  spent_micros: number
  /** Part of `spent_micros` whose outcome nobody could confirm. Needs reconciliation. */
  unknown_micros: number
  available_micros: number
  plan_budget_micros: number
  cost_per_search_micros: number
  cost_per_fetch_micros: number
  /** Per model request made while interpreting a plan's sources. */
  cost_per_model_call_micros: number
  updated_at: string
}

export type ResearchPlanStatus =
  | 'queued'
  | 'running'
  | 'completed'
  | 'partial'
  | 'failed'
  | 'needs_provider'
  | 'budget_exhausted'
  | 'cancelled'

export interface ResearchPlan {
  id: string
  partner_id: string
  material_id: string
  material_filename: string
  /** `null` once phase 1C has re-drafted that material: the research survives, the link does not. */
  question_id: string | null
  /** The question as approved, copied verbatim. This is what was researched. */
  question_text: string
  topic: string | null
  status: ResearchPlanStatus
  provider: string | null
  model: string | null
  prompt_profile: string
  passes: number
  max_passes: number
  budget_micros: number
  reserved_micros: number
  spent_micros: number
  queries_made: number
  results_seen: number
  sources_fetched: number
  sources_skipped: number
  bytes_fetched: number
  findings_accepted: number
  findings_rejected: number
  duration_ms: number | null
  /** Why sources or conclusions were refused, in the server's own words. Shown as-is. */
  rejections: string[]
  diagnostic: string | null
  cancel_requested: boolean
  started_at: string | null
  finished_at: string | null
  created_at: string
  updated_at: string
}

export type QueryOutcome = 'ok' | 'failed' | 'unknown' | 'refused'

export interface ResearchQueryRecord {
  id: string
  plan_id: string
  ordinal: number
  /** Exactly the text that was sent — not a reconstruction. */
  query_text: string
  provider: string
  results_count: number
  cost_micros: number
  outcome: QueryOutcome
  diagnostic: string | null
  created_at: string
}

export type SourceStatus =
  | 'discovered'
  | 'skipped_host'
  | 'skipped_robots'
  | 'skipped_limit'
  | 'skipped_type'
  | 'fetched'
  | 'failed'

export interface ResearchSource {
  id: string
  plan_id: string
  query_id: string | null
  url: string
  host: string
  title: string | null
  /** The search engine's summary. Discovery, never evidence. */
  snippet: string | null
  status: SourceStatus
  http_status: number | null
  content_type: string | null
  content_bytes: number | null
  content_chars: number | null
  content_hash: string | null
  /** Only when the page declares one. `null` means "not stated", never "free to use". */
  license: string | null
  license_note: string | null
  retrieved_at: string | null
  published_at: string | null
  cost_micros: number
  diagnostic: string | null
  created_at: string
}

/** A verbatim fragment of an external page, with where and when it was read. */
export interface ExternalEvidence {
  id: string
  source_id: string
  url: string
  host: string
  retrieved_at: string | null
  content_hash: string | null
  license: string | null
  quote: string
  char_start: number
  char_end: number
}

export interface ResearchFinding {
  id: string
  partner_id: string
  plan_id: string
  /** Always `industry`. Never a claim about this partner's products. */
  scope: 'industry'
  /** Always `candidate`: nothing here has been verified. */
  status: 'candidate'
  topic: string
  attribute: string
  value_text: string
  unit: string | null
  conditions: string | null
  /** The model's own words. Shown as explicitly not a quotation. */
  model_context: string | null
  evidence: ExternalEvidence[]
  created_at: string
}

/** A 1C question addressed to industry research, and the plan approved from it. */
export interface IndustryQuestion {
  id: string
  partner_id: string
  material_id: string
  material_filename: string
  gap_id: string
  gap_topic: string
  gap_missing: string
  text: string
  status: string
  /** `null` while nobody has approved it — and nothing happens until somebody does. */
  plan_id: string | null
  created_at: string
}

export interface ResearchSummary {
  plans_total: number
  plans_active: number
  questions_open: number
  sources_fetched: number
  sources_skipped: number
  findings_total: number
  spent_micros: number
}

export interface ResearchOverview {
  provider: ResearchProviderState
  budget: ResearchBudget
  summary: ResearchSummary
  plans: ResearchPlan[]
  questions: IndustryQuestion[]
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
