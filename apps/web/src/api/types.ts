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

/** Material processing status, phase 1A only (see implementation-contract.md). */
export type MaterialStatus =
  | 'queued'
  | 'processing'
  | 'completed'
  | 'partial'
  | 'failed'
  | 'quarantined'

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
}

export interface Job {
  id: string
  partner_id: string
  material_id: string
  kind: string
  status: string
  stage: string | null
  attempts: number
  created_at: string
  updated_at: string
  error: string | null
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
