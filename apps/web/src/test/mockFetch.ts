import { vi } from 'vitest'

/**
 * A small, fully-controlled stand-in for `fetch`, used only in tests (never
 * imported by application code). Each call is recorded and answered from an
 * explicit, test-authored queue/handler — there is no real network access
 * and no guessing at server behaviour.
 */
export interface RecordedRequest {
  url: string
  method: string
  headers: Record<string, string>
  body: unknown
}

export function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })
}

export function apiError(status: number, code: string, message: string, retryable = false): Response {
  return jsonResponse(status, { error: { code, message, retryable } })
}

export function installMockFetch(handler: (req: RecordedRequest) => Response | Promise<Response>) {
  const calls: RecordedRequest[] = []
  const fetchMock = vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : input.toString()
    const method = init?.method ?? 'GET'
    const headers: Record<string, string> = {}
    if (init?.headers) {
      new Headers(init.headers).forEach((value, key) => {
        headers[key] = value
      })
    }
    let body: unknown = undefined
    if (typeof init?.body === 'string') {
      try {
        body = JSON.parse(init.body)
      } catch {
        body = init.body
      }
    }
    const record: RecordedRequest = { url, method, headers, body }
    calls.push(record)
    return handler(record)
  })
  vi.stubGlobal('fetch', fetchMock)
  return { calls, fetchMock }
}
