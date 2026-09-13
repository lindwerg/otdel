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
  /**
   * `null` for a `validate_partner` job, which is about the partner rather than about
   * one document. Every other kind names its material, and the database ties the two
   * together.
   */
  material_id: string | null
  partner_id: string
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

// --- Phase 1E: checked knowledge versions, search and answers ---------------

/**
 * State of the deterministic half of 1E.
 *
 * It has no `state` field on purpose (`implementation-contract.md`, 1E): checking
 * and publishing are rule-based and cannot be switched off, so there is nothing
 * for a state to say. A shape with a `state: 'ready'` here would invite the
 * reading that verification, too, depends on a configured model.
 */
export interface ValidationModeView {
  mode: 'deterministic'
  message: string
}

/** One optional adapter — the same shape 1D uses. Never carries a key. */
export interface AdapterView {
  state: 'ready' | 'needs_configuration' | 'disabled'
  provider: string
  endpoint_host: string | null
  model: string | null
  message: string
}

/**
 * Whether vectors exist at all.
 *
 * `extension_missing` is not a bug and not something the migration can repair:
 * `vector` is not a trusted extension and the migration role is not superuser.
 * `no_embeddings` means the extension is there and nothing is being computed,
 * because no embedding provider is configured. Both are named states, and in
 * both search still works on keywords.
 */
export interface VectorView {
  state: 'ready' | 'no_embeddings' | 'extension_missing'
  profile: string | null
  message: string
}

/** Bounds of one search or answer, stated before anything is asked. */
export interface RetrievalLimits {
  max_query_chars: number
  max_results: number
  max_answer_claims: number
  max_answer_chars: number
  chunk_max_chars: number
  /** `null` until an embedding provider is configured: there is no dimension to
   *  state, and stating one anyway would describe vectors nobody is computing. */
}

/**
 * The *actual* retrieval mode of the installation, not the intended one.
 *
 * Deliberately a separate union from `SearchMode`: the installation says
 * `keyword_only`, one response says `keyword`. The contract spells them
 * differently and this file mirrors the wire, so neither value is silently
 * translated into the other.
 */
export type RetrievalSearchMode = 'hybrid' | 'keyword_only'

export interface RetrievalProviderState {
  state: 'ready' | 'needs_configuration' | 'disabled'
  validation: ValidationModeView
  embedding: AdapterView
  answer: AdapterView
  vector: VectorView
  search_mode: RetrievalSearchMode
  /** Environment variables the owner still has to set. */
  missing: string[]
  limits: RetrievalLimits
  message: string
}

/**
 * Status of one immutable knowledge version.
 *
 * `blocked` is the one that must not be read as a failure: the readiness rules
 * were not met, so the version exists as the record of a check and is **not**
 * published. `superseded` and `revoked` are settled outcomes too — a version
 * that was once pinned stays readable, which is the whole point of pinning.
 */
export type VersionStatus =
  | 'draft'
  | 'validating'
  | 'published'
  | 'blocked'
  | 'superseded'
  | 'revoked'

/** Readiness is decided separately for these four, and for nothing else. */
export type ReadinessTopic =
  | 'product_description'
  | 'audience_hypotheses'
  | 'characteristic_answers'
  | 'commercial_answers'

/** `limited` means answering is possible with stated caveats — not "almost ready". */
export type ReadinessState = 'ready' | 'limited' | 'blocked'

/**
 * Readiness of one topic.
 *
 * Readiness is the **availability of knowledge**, never a permission to send
 * anything, to promise compatibility or to take on an obligation. `reason` says
 * in words what the state means here, and the interface shows it as-is.
 */
export interface ReadinessEntry {
  topic: ReadinessTopic
  state: ReadinessState
  reason: string
}

export interface KnowledgeVersion {
  id: string
  partner_id: string
  /** Partner's own sequence, from 1. It grows and is never reused. */
  number: number
  status: VersionStatus
  validation_run_id: string | null
  /** Which candidates, at which revisions, went in. A late run with an older
   *  fingerprint than the published one is not published. */
  input_fingerprint: string
  /** Phase 1F: the same candidates without their verdicts, so "is this still
   *  built from what the documents say" is answerable without a new check.
   *  `null` for a version published before it was recorded — reported as a
   *  comparison that cannot be made, never as "nothing changed". */
  candidate_fingerprint: string | null
  claims_total: number
  claims_source_supported: number
  claims_hypothesis: number
  claims_unknown: number
  claims_conflicted: number
  claims_stale: number
  gaps_open: number
  chunks_total: number
  chunks_embedded: number
  embedding_profile: string | null
  readiness: ReadinessEntry[]
  /** Why this version was not published, in the server's own words. Shown as-is. */
  blocked_reasons: string[]
  created_at: string
  published_at: string | null
  superseded_at: string | null
  revoked_at: string | null
  /** Mandatory on retraction: a withdrawal without a reason is indistinguishable
   *  from a malfunction. */
  revoked_reason: string | null
}

/**
 * Verdict of the deterministic check on one statement.
 *
 * `source_supported` means **the cited source says so** — not that a manufacturer
 * confirmed it and not that it is true (`block-01-spec.md` §6.7). Everything else
 * is explicitly not confirmed by its source, and the interface must show that.
 */
export type ClaimStatus =
  | 'source_supported'
  | 'hypothesis'
  | 'unknown'
  | 'conflicted'
  | 'stale'

/** Which candidate this snapshot was taken from: a 1C fact or a 1D conclusion. */
export type ClaimOrigin = 'partner_material' | 'industry_research'

/** An industry statement stays industry-wide inside a version, and has no product. */
export type ClaimScope = 'partner' | 'industry'

export type EvidenceSourceKind = 'material' | 'external'

/**
 * A quotation **copied into the version** at publication time.
 *
 * The snapshot does not point at page text that could be re-read and changed;
 * `quote` is the fragment as it was. Fields of the other `source_kind` are null:
 * a material citation has no `url`, an external one has no page number.
 */
export interface VersionEvidence {
  id: string
  claim_id: string
  source_kind: EvidenceSourceKind
  material_id: string | null
  material_filename: string | null
  page_number: number | null
  region_id: string | null
  url: string | null
  host: string | null
  retrieved_at: string | null
  content_hash: string | null
  quote: string
  char_start: number
  char_end: number
}

export interface VersionClaim {
  id: string
  version_id: string
  origin: ClaimOrigin
  /** The candidate this came from — traceability only. Deleting the candidate
   *  does not change the published version. */
  origin_id: string
  scope: ClaimScope
  product_name: string | null
  /** The same four kinds phase 1C drafts with — a version does not invent new ones. */
  kind: FactKind
  status: ClaimStatus
  attribute: string
  /** The value exactly as the source writes it. Never reformatted. */
  value_text: string
  unit: string | null
  conditions: string | null
  /** The model's own words. Shown as explicitly not a quotation. */
  model_context: string | null
  /** Why the verdict is what it is, in words. Shown as-is when present. */
  check_note: string | null
  evidence: VersionEvidence[]
  created_at: string
}

export interface VersionGap {
  id: string
  version_id: string
  origin_id: string
  product_name: string | null
  topic: string
  missing: string
  blocks: string | null
  /** Which of the four readiness topics this gap limits. */
  blocks_topics: ReadinessTopic[]
  created_at: string
}

/**
 * Status of one check.
 *
 * There is deliberately no `needs_provider` here: the check is deterministic and
 * does not depend on a model at all.
 */
export type ValidationRunStatus = 'queued' | 'running' | 'completed' | 'partial' | 'failed'

export interface ValidationRun {
  id: string
  partner_id: string
  status: ValidationRunStatus
  prompt_profile: string
  version_id: string | null
  version_number: number | null
  claims_considered: number
  claims_source_supported: number
  claims_hypothesis: number
  claims_unknown: number
  claims_conflicted: number
  claims_stale: number
  claims_rejected: number
  gaps_carried: number
  chunks_created: number
  chunks_embedded: number
  /** How many statements got a model's second opinion. `0` is the normal state
   *  without a key and does not lower the run's status. */
  model_reviewed: number
  /** Whether this run published a version. `false` whenever `blocked_reasons` is non-empty. */
  published: boolean
  /** Why candidates were refused, in the server's own words. Shown as-is. */
  rejections: string[]
  blocked_reasons: string[]
  diagnostic: string | null
  started_at: string | null
  finished_at: string | null
  created_at: string
}

/** How many candidates the partner has right now, so the interface never offers
 *  a check where there is nothing to check. */
export interface CandidateSummary {
  facts: number
  findings: number
  gaps_open: number
  materials_drafted: number
}

export interface ValidationOverview {
  provider: RetrievalProviderState
  published: KnowledgeVersion | null
  runs: ValidationRun[]
  versions: KnowledgeVersion[]
  candidates: CandidateSummary
}

/** The version one answer is pinned to. Results never mix two versions. */
export interface VersionRef {
  id: string
  number: number
  status: VersionStatus
  published_at: string | null
}

/** Why one statement was found. Never empty. */
export type MatchKind = 'exact' | 'keyword' | 'vector'

/** The mode one response actually ran in. See `RetrievalSearchMode`. */
export type SearchMode = 'hybrid' | 'keyword'

/**
 * `no_published_version` is not an error and not an empty result: the partner has
 * nothing published, because no check ran, the check was blocked, or the version
 * was retracted. It is a named state.
 */
export type SearchState = 'ok' | 'no_published_version' | 'insufficient_evidence'

export interface SearchHit {
  claim: VersionClaim
  /** Comparability inside this one response — not a probability and not a percentage. */
  score: number
  matched_by: MatchKind[]
}

export interface SearchResponse {
  state: SearchState
  version: VersionRef | null
  mode: SearchMode
  /** Why the mode is not the full one, in words. Shown as-is. */
  degraded: string[]
  items: SearchHit[]
  gaps: VersionGap[]
  message: string
}

/**
 * `evidence_only` — statements with citations exist, prose does not, because the
 * model is not configured or its answer failed the citation check.
 * `insufficient_evidence` — the version exists and holds nothing suitable; no
 * guess is substituted (`block-01-spec.md` §13.5).
 */
export type AnswerState =
  | 'answered'
  | 'evidence_only'
  | 'insufficient_evidence'
  | 'no_published_version'

export interface AnswerResponse {
  state: AnswerState
  version: VersionRef | null
  mode: SearchMode
  degraded: string[]
  /** `null` in every state except `answered`. */
  text: string | null
  /** `true` whenever `text` is not null: prose is the model's wording, not a
   *  quotation, and the interface is obliged to mark it. */
  answer_is_model_context: boolean
  claims: VersionClaim[]
  citations: VersionEvidence[]
  conditions: string[]
  gaps: VersionGap[]
  readiness: ReadinessEntry[]
  /** Caveats in words: limited readiness, a `conflicted` statement among the
   *  findings, a `stale` source. Shown as-is. */
  limitations: string[]
  rejections: string[]
  message: string
}

/**
 * Body of a search request.
 *
 * Optional members are genuinely absent rather than `null` — this is a request
 * body, where an omitted key means "server decides" (current published version,
 * default limit), which is not the same thing as sending an explicit null.
 */
export interface SearchRequest {
  query: string
  version_id?: string
  /**
   * Product *name*, not an id. Compared in folded form, so case and spacing do not
   * matter. The server validates this body strictly, so a field that is not here is a
   * 400 rather than a filter that is quietly ignored.
   */
  product?: string
  limit?: number
}

export interface AnswerRequest {
  question: string
  version_id?: string
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

// --- phase 1F: the update cycle ------------------------------------------------------

/**
 * Where the published knowledge stands relative to the partner's documents.
 *
 * Computed on every request from rows that exist — there is no stored "stale"
 * flag, so the interface cannot show one that somebody forgot to update.
 */
export type RefreshState =
  | 'never_published'
  | 'current'
  | 'revalidation_required'
  | 'checking'
  | 'retracted'

export type RefreshReasonCode =
  | 'material_not_read'
  | 'material_not_drafted'
  | 'source_reread'
  | 'candidates_changed'
  | 'comparison_unavailable'
  | 'last_check_blocked'
  | 'last_check_failed'
  | 'version_retracted'
  | 'nothing_published'

export interface RefreshReason {
  code: RefreshReasonCode
  /** Shown verbatim. */
  message: string
  material_id: string | null
  material_filename: string | null
  version_id: string | null
  version_number: number | null
  content_revision: number | null
  drafted_revision: number | null
}

export type SourceState =
  | 'reading'
  | 'unreadable'
  | 'not_drafted'
  | 'drafted'
  | 'reread_after_draft'

export interface SourceRefresh {
  material_id: string
  filename: string
  material_status: MaterialStatus
  state: SourceState
  /** How many times this stored original has been *read*. */
  content_revision: number
  /** Which reading the current draft used; `null` = unknown, never "current". */
  drafted_revision: number | null
  /**
   * The 1C run's own status, or `null` when the product role never ran over this
   * document. Needed because `drafted_revision` is recorded when a run *starts*:
   * a run that then failed leaves a number that looks exactly like success.
   */
  draft_status:
    | 'queued'
    | 'running'
    | 'completed'
    | 'partial'
    | 'failed'
    | 'needs_provider'
    | null
  facts_drafted: number
  claims_in_published: number
  message: string
}

export interface VersionRef1F {
  id: string
  number: number
  status: string
  published_at: string | null
}

export interface RefreshStatus {
  state: RefreshState
  published: VersionRef1F | null
  latest: VersionRef1F | null
  reasons: RefreshReason[]
  sources: SourceRefresh[]
  candidate_fingerprint: string
  published_candidate_fingerprint: string | null
  checking: boolean
  message: string
  computed_at: string
}

export type RefreshStepKind = 'extraction' | 'understanding' | 'validation'

/** `queued` is the only outcome that means work will happen. */
export type RefreshStepOutcome =
  | 'queued'
  | 'already_running'
  | 'up_to_date'
  | 'needs_provider'
  | 'waiting'

export interface RefreshStep {
  kind: RefreshStepKind
  outcome: RefreshStepOutcome
  material_id: string | null
  material_filename: string | null
  job_id: string | null
  message: string
}

export interface RefreshPlan {
  steps: RefreshStep[]
  /** How many steps were really queued. Counts, never a percentage. */
  queued: number
  message: string
  requested_at: string
}

export type EventKind =
  | 'material_uploaded'
  | 'material_duplicate'
  | 'material_reprocess_requested'
  | 'material_extraction_finished'
  | 'understanding_queued'
  | 'understanding_finished'
  | 'validation_queued'
  | 'validation_finished'
  | 'version_published'
  | 'version_blocked'
  | 'version_superseded'
  | 'version_retracted'
  | 'refresh_requested'
  | 'export_read'
  | 'job_failed'
  | 'retention_applied'

export type EventActor = 'owner' | 'worker' | 'system'

export interface HistoryEvent {
  id: string
  partner_id: string | null
  kind: EventKind
  actor: EventActor
  material_id: string | null
  version_id: string | null
  job_id: string | null
  run_id: string | null
  /** The server's sentence, shown as it is. */
  summary: string
  detail: Record<string, unknown>
  occurred_at: string
}

export type ChangeKind = 'added' | 'removed' | 'changed'

export interface ClaimSide {
  claim_id: string
  status: ClaimStatus
  value_text: string
  unit: string | null
  conditions: string | null
  /** `filename#page` or a host, as the version recorded it. */
  sources: string[]
}

export interface ClaimChange {
  kind: ChangeKind
  scope: ClaimScope
  product_name: string | null
  attribute: string
  before: ClaimSide | null
  after: ClaimSide | null
  fields: string[]
  message: string
}

export interface ReadinessChange {
  topic: ReadinessTopic
  before: ReadinessState | null
  after: ReadinessState | null
  reason: string
}

export interface GapChange {
  kind: ChangeKind
  topic: string
  missing: string
  product_name: string | null
}

export interface ChangeCounts {
  added: number
  removed: number
  changed: number
  unchanged: number
}

export interface VersionChanges {
  /** `null` for a partner's first version: there is nothing to compare with. */
  from: VersionRef1F | null
  to: VersionRef1F
  counts: ChangeCounts
  claims: ClaimChange[]
  readiness: ReadinessChange[]
  gaps: GapChange[]
  /** What the comparison cannot see. Shown next to the result. */
  limitations: string[]
  message: string
}

export type RetentionState = 'keep_everything' | 'enabled'

export interface RetentionPreview {
  events_prunable: number
  jobs_prunable: number
  events_total: number
  jobs_total: number
  oldest_event: string | null
}

export interface RetentionPolicy {
  state: RetentionState
  event_days: number | null
  job_days: number | null
  keep_per_kind: number
  sweep_interval_seconds: number
  preview: RetentionPreview
  /** What retention never removes, in words. */
  protected: string[]
  last_sweep: HistoryEvent | null
  message: string
}

export interface ExportManifest {
  schema: string
  generated_at: string
  bureau_slug: string
  partner_id: string
  partner_name: string
  version_id: string
  version_number: number
  version_status: string
  published_at: string | null
  superseded_at: string | null
  input_fingerprint: string
  candidate_fingerprint: string | null
  claims_total: number
  claims_source_supported: number
  gaps_total: number
  /** The caveats that travel with the file. */
  disclosure: string[]
}

export interface ExportDocument {
  manifest: ExportManifest
  version: KnowledgeVersion
  claims: VersionClaim[]
  gaps: VersionGap[]
}
