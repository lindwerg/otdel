import type {
  AnswerRequest,
  AnswerResponse,
  ApiErrorBody,
  ExportDocument,
  GlossaryTerm,
  HistoryEvent,
  Job,
  KnowledgeGap,
  KnowledgeOverview,
  KnowledgeRun,
  KnowledgeVersion,
  ListResponse,
  Material,
  MaterialPage,
  PageDetail,
  PageView,
  Partner,
  ProductNode,
  ProviderState,
  QaEntry,
  RefreshPlan,
  RefreshStatus,
  ResearchBudget,
  ResearchFinding,
  ResearchOverview,
  ResearchPlan,
  ResearchProviderState,
  ResearchQueryRecord,
  ResearchSource,
  RetentionPolicy,
  RetrievalProviderState,
  SearchRequest,
  SearchResponse,
  SessionResponse,
  ValidationOverview,
  ValidationRun,
  VersionChanges,
  VersionClaim,
  VersionGap,
} from './types'

/** Server-side limit per docs/implementation-contract.md ("До 25 MiB на файл"). */
export const MAX_UPLOAD_BYTES = 25 * 1024 * 1024

/**
 * Thrown for every non-2xx API response. Carries the server's own
 * `{code, message, retryable}` when the body actually matches the
 * documented error shape, so callers can show the server's own message
 * instead of a made-up one.
 */
export class ApiError extends Error {
  readonly status: number
  readonly code: string | null
  readonly retryable: boolean

  constructor(status: number, body: ApiErrorBody['error'] | null) {
    super(body?.message || `Запрос завершился с ошибкой (код ${status}).`)
    this.name = 'ApiError'
    this.status = status
    this.code = body?.code ?? null
    this.retryable = body?.retryable ?? false
  }
}

/** True when the server was unreachable at all (no HTTP response). */
export class NetworkError extends Error {
  constructor(cause: unknown) {
    super('Сервер недоступен. Проверьте соединение и повторите попытку.')
    this.name = 'NetworkError'
    this.cause = cause
  }
}

/**
 * A stale/rejected CSRF token (backend code `invalid_csrf_token`, HTTP 403).
 * Distinct from a generic 403 so callers only ever attempt the bounded
 * "refresh token and replay once" recovery for this exact, well-defined case
 * — never for an ambiguous network error or an unrelated 403.
 */
export function isCsrfError(err: unknown): boolean {
  return err instanceof ApiError && err.status === 403 && err.code === 'invalid_csrf_token'
}

/** The session is gone/expired server-side (HTTP 401). */
export function isSessionExpiredError(err: unknown): boolean {
  return err instanceof ApiError && err.status === 401
}

async function readApiError(response: Response): Promise<ApiError> {
  let body: ApiErrorBody['error'] | null = null
  try {
    const parsed = (await response.json()) as Partial<ApiErrorBody>
    if (parsed && typeof parsed === 'object' && parsed.error) {
      body = parsed.error
    }
  } catch {
    // Response had no (valid) JSON body; ApiError falls back to a generic message.
  }
  return new ApiError(response.status, body)
}

interface RequestOptions {
  method?: 'GET' | 'POST' | 'PATCH' | 'DELETE'
  body?: unknown
  /** Required for every mutating request per the contract (X-CSRF-Token). */
  csrfToken?: string | null
}

async function apiRequest<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const method = options.method ?? 'GET'
  const headers: Record<string, string> = {}
  let body: BodyInit | undefined

  if (options.body !== undefined) {
    headers['Content-Type'] = 'application/json'
    body = JSON.stringify(options.body)
  }

  if (method !== 'GET' && options.csrfToken) {
    headers['X-CSRF-Token'] = options.csrfToken
  }

  let response: Response
  try {
    response = await fetch(`/api${path}`, {
      method,
      headers,
      credentials: 'include',
      body,
    })
  } catch (cause) {
    throw new NetworkError(cause)
  }

  if (!response.ok) {
    throw await readApiError(response)
  }
  if (response.status === 204) {
    return undefined as T
  }
  return (await response.json()) as T
}

// --- Session -----------------------------------------------------------

export function login(password: string): Promise<SessionResponse> {
  return apiRequest<SessionResponse>('/session', { method: 'POST', body: { password } })
}

export function fetchSession(): Promise<SessionResponse> {
  return apiRequest<SessionResponse>('/session')
}

export function logout(csrfToken: string): Promise<void> {
  return apiRequest<void>('/session', { method: 'DELETE', csrfToken })
}

// --- Partners ------------------------------------------------------------

export async function listPartners(): Promise<Partner[]> {
  const res = await apiRequest<ListResponse<Partner>>('/partners')
  return res.items
}

export function createPartner(
  input: { name: string; note?: string },
  csrfToken: string,
): Promise<Partner> {
  return apiRequest<Partner>('/partners', { method: 'POST', body: input, csrfToken })
}

export function getPartner(id: string): Promise<Partner> {
  return apiRequest<Partner>(`/partners/${encodeURIComponent(id)}`)
}

export function updatePartner(
  id: string,
  patch: { name?: string; note?: string },
  csrfToken: string,
): Promise<Partner> {
  return apiRequest<Partner>(`/partners/${encodeURIComponent(id)}`, {
    method: 'PATCH',
    body: patch,
    csrfToken,
  })
}

// --- Materials -------------------------------------------------------------

export async function listMaterials(partnerId: string): Promise<Material[]> {
  const res = await apiRequest<ListResponse<Material>>(
    `/partners/${encodeURIComponent(partnerId)}/materials`,
  )
  return res.items
}

export function retryMaterial(
  partnerId: string,
  materialId: string,
  csrfToken: string,
): Promise<Material> {
  return apiRequest<Material>(
    `/partners/${encodeURIComponent(partnerId)}/materials/${encodeURIComponent(materialId)}/retry`,
    { method: 'POST', csrfToken },
  )
}

export function getMaterial(partnerId: string, materialId: string): Promise<Material> {
  return apiRequest<Material>(
    `/partners/${encodeURIComponent(partnerId)}/materials/${encodeURIComponent(materialId)}`,
  )
}

/**
 * Authorised download of the stored original.
 *
 * `page` appends the standard PDF open parameter (`#page=N`), which browser and
 * desktop viewers honour. It is a real link into the source document — phase 1B
 * stores no page images, so nothing here pretends a rendered page exists.
 */
export function originalMaterialUrl(
  partnerId: string,
  materialId: string,
  page?: number,
): string {
  const base = `/api/partners/${encodeURIComponent(partnerId)}/materials/${encodeURIComponent(materialId)}/original`
  return page && page > 0 ? `${base}#page=${page}` : base
}

// --- Pages (phase 1B) ------------------------------------------------------

function pagesPath(partnerId: string, materialId: string): string {
  return `/partners/${encodeURIComponent(partnerId)}/materials/${encodeURIComponent(materialId)}/pages`
}

export async function listPages(
  partnerId: string,
  materialId: string,
): Promise<MaterialPage[]> {
  const res = await apiRequest<ListResponse<MaterialPage>>(pagesPath(partnerId, materialId))
  return res.items
}

export function getPage(
  partnerId: string,
  materialId: string,
  pageNumber: number,
): Promise<PageDetail> {
  return apiRequest<PageDetail>(
    `${pagesPath(partnerId, materialId)}/${encodeURIComponent(String(pageNumber))}`,
  )
}

/**
 * Where the regions of one page sit, in that page's own coordinates.
 *
 * A separate request from `getPage` because it answers a different question —
 * "where on the sheet", not "what was read" — and because a page whose size was
 * never recorded still has regions worth listing. It returns no page image:
 * nothing in this phase renders the document.
 */
export function getPageView(
  partnerId: string,
  materialId: string,
  pageNumber: number,
): Promise<PageView> {
  return apiRequest<PageView>(
    `${pagesPath(partnerId, materialId)}/${encodeURIComponent(String(pageNumber))}/view`,
  )
}

export function retryPage(
  partnerId: string,
  materialId: string,
  pageNumber: number,
  csrfToken: string,
): Promise<MaterialPage> {
  return apiRequest<MaterialPage>(
    `${pagesPath(partnerId, materialId)}/${encodeURIComponent(String(pageNumber))}/retry`,
    { method: 'POST', csrfToken },
  )
}

// --- Knowledge (phase 1C) --------------------------------------------------

function knowledgePath(partnerId: string, suffix = ''): string {
  return `/partners/${encodeURIComponent(partnerId)}/knowledge${suffix}`
}

/**
 * State of the model adapter. Not partner data: it describes the installation,
 * and it never contains the key — only which provider, model and host would be
 * used.
 */
export function fetchProviderState(): Promise<ProviderState> {
  return apiRequest<ProviderState>('/knowledge/provider')
}

export function fetchKnowledgeOverview(partnerId: string): Promise<KnowledgeOverview> {
  return apiRequest<KnowledgeOverview>(knowledgePath(partnerId))
}

export async function listKnowledgeProducts(partnerId: string): Promise<ProductNode[]> {
  const res = await apiRequest<ListResponse<ProductNode>>(knowledgePath(partnerId, '/products'))
  return res.items
}

export async function listGlossary(partnerId: string): Promise<GlossaryTerm[]> {
  const res = await apiRequest<ListResponse<GlossaryTerm>>(knowledgePath(partnerId, '/glossary'))
  return res.items
}

export async function listKnowledgeQa(partnerId: string): Promise<QaEntry[]> {
  const res = await apiRequest<ListResponse<QaEntry>>(knowledgePath(partnerId, '/qa'))
  return res.items
}

export async function listGaps(partnerId: string): Promise<KnowledgeGap[]> {
  const res = await apiRequest<ListResponse<KnowledgeGap>>(knowledgePath(partnerId, '/gaps'))
  return res.items
}

/**
 * Queue the product role over one material.
 *
 * Idempotent server-side: while a run is queued or running, this returns that
 * run instead of starting a second one. A 409 means the server refused with a
 * stated reason (no key configured, or the material has not been read yet) —
 * the caller shows that reason rather than a generic failure.
 */
export function understandMaterial(
  partnerId: string,
  materialId: string,
  csrfToken: string,
): Promise<KnowledgeRun> {
  return apiRequest<KnowledgeRun>(
    `/partners/${encodeURIComponent(partnerId)}/materials/${encodeURIComponent(materialId)}/understand`,
    { method: 'POST', csrfToken },
  )
}

// --- Research (phase 1D) ---------------------------------------------------

function researchPath(partnerId: string, suffix = ''): string {
  return `/partners/${encodeURIComponent(partnerId)}/research${suffix}`
}

/**
 * State of the researcher. Not partner data: it describes the installation, and
 * it never contains a key — only which endpoint, which hosts and which model
 * would be used.
 */
export function fetchResearchProviderState(): Promise<ResearchProviderState> {
  return apiRequest<ResearchProviderState>('/research/provider')
}

export function fetchResearchBudget(): Promise<ResearchBudget> {
  return apiRequest<ResearchBudget>('/research/budget')
}

export function fetchResearchOverview(partnerId: string): Promise<ResearchOverview> {
  return apiRequest<ResearchOverview>(researchPath(partnerId))
}

export async function listResearchFindings(partnerId: string): Promise<ResearchFinding[]> {
  const res = await apiRequest<ListResponse<ResearchFinding>>(
    researchPath(partnerId, '/findings'),
  )
  return res.items
}

export async function listResearchSources(
  partnerId: string,
  planId: string,
): Promise<ResearchSource[]> {
  const res = await apiRequest<ListResponse<ResearchSource>>(
    researchPath(partnerId, `/plans/${encodeURIComponent(planId)}/sources`),
  )
  return res.items
}

export async function listResearchQueries(
  partnerId: string,
  planId: string,
): Promise<ResearchQueryRecord[]> {
  const res = await apiRequest<ListResponse<ResearchQueryRecord>>(
    researchPath(partnerId, `/plans/${encodeURIComponent(planId)}/queries`),
  )
  return res.items
}

/**
 * Approve one industry question for bounded research.
 *
 * Idempotent server-side: while a plan is queued or running this returns that
 * plan. A settled plan is put back in the queue, bounded by the pass limit. A
 * 409 means the server refused with a stated reason — nothing configured, the
 * budget is empty, or the question has used every pass it was allowed — and the
 * caller shows that reason rather than a generic failure.
 */
export function approveResearch(
  partnerId: string,
  questionId: string,
  csrfToken: string,
): Promise<ResearchPlan> {
  return apiRequest<ResearchPlan>(
    researchPath(partnerId, `/questions/${encodeURIComponent(questionId)}/plan`),
    { method: 'POST', csrfToken },
  )
}

/**
 * Ask a running plan to stop.
 *
 * The worker settles it at its next checkpoint, which is always *before* a
 * chargeable call — so stopping never leaves money half-spent.
 */
export function stopResearch(
  partnerId: string,
  planId: string,
  csrfToken: string,
): Promise<ResearchPlan> {
  return apiRequest<ResearchPlan>(
    researchPath(partnerId, `/plans/${encodeURIComponent(planId)}/stop`),
    { method: 'POST', csrfToken },
  )
}

// --- Knowledge versions, search and answers (phase 1E) ---------------------

function versionsPath(partnerId: string, suffix = ''): string {
  return `/partners/${encodeURIComponent(partnerId)}/versions${suffix}`
}

function retrievalPath(partnerId: string, suffix: string): string {
  return `/partners/${encodeURIComponent(partnerId)}/retrieval${suffix}`
}

/**
 * What this installation can do with published knowledge.
 *
 * Not partner data: it describes the machine. Note what it does *not* gate —
 * checking and publishing are deterministic and need no model at all. The
 * adapters here decide only two optional halves: prose answers, and the vector
 * side of search. No key is ever returned; only the host that would be called.
 */
export function fetchRetrievalProvider(): Promise<RetrievalProviderState> {
  return apiRequest<RetrievalProviderState>('/retrieval/provider')
}

export function fetchValidationOverview(partnerId: string): Promise<ValidationOverview> {
  return apiRequest<ValidationOverview>(`/partners/${encodeURIComponent(partnerId)}/validation`)
}

export async function listVersions(partnerId: string): Promise<KnowledgeVersion[]> {
  const res = await apiRequest<ListResponse<KnowledgeVersion>>(versionsPath(partnerId))
  return res.items
}

export function fetchVersion(partnerId: string, versionId: string): Promise<KnowledgeVersion> {
  return apiRequest<KnowledgeVersion>(
    versionsPath(partnerId, `/${encodeURIComponent(versionId)}`),
  )
}

export async function listVersionClaims(
  partnerId: string,
  versionId: string,
): Promise<VersionClaim[]> {
  const res = await apiRequest<ListResponse<VersionClaim>>(
    versionsPath(partnerId, `/${encodeURIComponent(versionId)}/claims`),
  )
  return res.items
}

export async function listVersionGaps(
  partnerId: string,
  versionId: string,
): Promise<VersionGap[]> {
  const res = await apiRequest<ListResponse<VersionGap>>(
    versionsPath(partnerId, `/${encodeURIComponent(versionId)}/gaps`),
  )
  return res.items
}

/**
 * Queue the deterministic check over every candidate this partner has.
 *
 * Idempotent server-side: while a run is queued or running this returns that same
 * run. A 409 means the partner has no candidates at all — the caller shows that
 * reason rather than a generic failure.
 */
export function startValidation(partnerId: string, csrfToken: string): Promise<ValidationRun> {
  return apiRequest<ValidationRun>(`/partners/${encodeURIComponent(partnerId)}/validate`, {
    method: 'POST',
    csrfToken,
  })
}

/**
 * Withdraw a published version, with a stated reason.
 *
 * `reason` is required by the contract (1–1000 characters) for one reason: a
 * withdrawal without a reason cannot be told apart from a malfunction. A 409
 * means the version was not `published` when the request arrived.
 */
export function retractVersion(
  partnerId: string,
  versionId: string,
  reason: string,
  csrfToken: string,
): Promise<KnowledgeVersion> {
  return apiRequest<KnowledgeVersion>(
    versionsPath(partnerId, `/${encodeURIComponent(versionId)}/retract`),
    { method: 'POST', body: { reason }, csrfToken },
  )
}

/**
 * Search inside **one published version**. A 1C/1D draft is not visible here.
 *
 * POST, and therefore CSRF-protected, because the request carries the text
 * somebody typed: a question does not belong in a URL, in a server log or in
 * browser history.
 */
export function searchPublished(
  partnerId: string,
  body: SearchRequest,
  csrfToken: string,
): Promise<SearchResponse> {
  return apiRequest<SearchResponse>(retrievalPath(partnerId, '/search'), {
    method: 'POST',
    body,
    csrfToken,
  })
}

/** Ask inside one published version. POST for the same reason as the search. */
export function askPublished(
  partnerId: string,
  body: AnswerRequest,
  csrfToken: string,
): Promise<AnswerResponse> {
  return apiRequest<AnswerResponse>(retrievalPath(partnerId, '/answer'), {
    method: 'POST',
    body,
    csrfToken,
  })
}

// --- Jobs --------------------------------------------------------------

export async function listJobs(partnerId: string): Promise<Job[]> {
  const res = await apiRequest<ListResponse<Job>>(
    `/partners/${encodeURIComponent(partnerId)}/jobs`,
  )
  return res.items
}

// --- phase 1F: updates, history, comparison, export, retention -----------------------

function partnerPath(partnerId: string, suffix: string): string {
  return `/partners/${encodeURIComponent(partnerId)}${suffix}`
}

/** Where the published version stands relative to the partner's documents, and why. */
export function fetchRefreshStatus(partnerId: string): Promise<RefreshStatus> {
  return apiRequest<RefreshStatus>(partnerPath(partnerId, '/refresh'))
}

/**
 * Start whatever is missing, in dependency order.
 *
 * The answer is a report of what was really queued and why the rest was not —
 * never an assertion that a cycle is "in progress".
 */
export function requestRefresh(partnerId: string, csrfToken: string): Promise<RefreshPlan> {
  return apiRequest<RefreshPlan>(partnerPath(partnerId, '/refresh'), {
    method: 'POST',
    body: {},
    csrfToken,
  })
}

/** Read one document again, on purpose. Allowed only for a finished material. */
export function reprocessMaterial(
  partnerId: string,
  materialId: string,
  csrfToken: string,
): Promise<Material> {
  return apiRequest<Material>(
    partnerPath(partnerId, `/materials/${encodeURIComponent(materialId)}/reprocess`),
    { method: 'POST', body: {}, csrfToken },
  )
}

export async function listEvents(partnerId: string, limit?: number): Promise<HistoryEvent[]> {
  const query = limit == null ? '' : `?limit=${encodeURIComponent(String(limit))}`
  const res = await apiRequest<ListResponse<HistoryEvent>>(
    partnerPath(partnerId, `/events${query}`),
  )
  return res.items
}

/** What this version says that the previous published one did not. */
export function fetchVersionChanges(
  partnerId: string,
  versionId: string,
  against?: string,
): Promise<VersionChanges> {
  const query = against == null ? '' : `?against=${encodeURIComponent(against)}`
  return apiRequest<VersionChanges>(
    partnerPath(partnerId, `/versions/${encodeURIComponent(versionId)}/changes${query}`),
  )
}

/**
 * The read-only copy of one published (or once-published) version.
 *
 * Used by the interface to offer a download; a downstream agent calls the same
 * endpoint directly. Reading it is recorded in the partner's history.
 */
export function fetchVersionExport(
  partnerId: string,
  versionId: string,
): Promise<ExportDocument> {
  return apiRequest<ExportDocument>(
    partnerPath(partnerId, `/versions/${encodeURIComponent(versionId)}/export`),
  )
}

/** The installation's retention policy, what it protects, and what a sweep would remove. */
export function fetchRetentionPolicy(): Promise<RetentionPolicy> {
  return apiRequest<RetentionPolicy>('/retention')
}

// --- Upload (real progress via XMLHttpRequest; fetch has no portable
// upload-progress event, so XHR is used specifically to report a genuine
// percentage instead of inventing one) --------------------------------------

export interface UploadProgress {
  loaded: number
  total: number
}

export function uploadMaterial(
  partnerId: string,
  file: File,
  csrfToken: string,
  onProgress?: (progress: UploadProgress) => void,
): Promise<Material> {
  return new Promise<Material>((resolve, reject) => {
    const xhr = new XMLHttpRequest()
    xhr.open('POST', `/api/partners/${encodeURIComponent(partnerId)}/materials`)
    xhr.withCredentials = true
    xhr.setRequestHeader('X-CSRF-Token', csrfToken)

    if (onProgress) {
      xhr.upload.onprogress = (event) => {
        if (event.lengthComputable) {
          onProgress({ loaded: event.loaded, total: event.total })
        }
      }
    }

    xhr.onload = () => {
      const status = xhr.status
      let parsed: unknown = null
      try {
        parsed = xhr.responseText ? JSON.parse(xhr.responseText) : null
      } catch {
        parsed = null
      }
      if (status >= 200 && status < 300) {
        resolve(parsed as Material)
      } else {
        const errorBody =
          parsed && typeof parsed === 'object' && 'error' in (parsed as Record<string, unknown>)
            ? ((parsed as ApiErrorBody).error ?? null)
            : null
        reject(new ApiError(status, errorBody))
      }
    }
    xhr.onerror = () => reject(new NetworkError(new Error('XHR network error')))
    xhr.onabort = () => reject(new NetworkError(new Error('upload aborted')))

    const formData = new FormData()
    formData.append('file', file, file.name)
    xhr.send(formData)
  })
}
