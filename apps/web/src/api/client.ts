import type {
  ApiErrorBody,
  Job,
  ListResponse,
  Material,
  Partner,
  SessionResponse,
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

export function originalMaterialUrl(partnerId: string, materialId: string): string {
  return `/api/partners/${encodeURIComponent(partnerId)}/materials/${encodeURIComponent(materialId)}/original`
}

// --- Jobs --------------------------------------------------------------

export async function listJobs(partnerId: string): Promise<Job[]> {
  const res = await apiRequest<ListResponse<Job>>(
    `/partners/${encodeURIComponent(partnerId)}/jobs`,
  )
  return res.items
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
