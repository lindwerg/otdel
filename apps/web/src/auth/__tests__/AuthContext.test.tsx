import { act, renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiError } from '../../api/client'
import { apiError, installMockFetch, jsonResponse } from '../../test/mockFetch'
import { AuthProvider, SessionExpiredError, useAuth } from '../AuthContext'

function wrapper({ children }: { children: ReactNode }) {
  return <AuthProvider>{children}</AuthProvider>
}

afterEach(() => {
  vi.unstubAllGlobals()
})

async function renderAuthenticated(sessionHandler: (callCount: number) => Response) {
  let sessionCalls = 0
  installMockFetch((req) => {
    if (req.url === '/api/session' && req.method === 'GET') {
      sessionCalls += 1
      return sessionHandler(sessionCalls)
    }
    throw new Error(`unhandled request in test setup: ${req.method} ${req.url}`)
  })
  const view = renderHook(() => useAuth(), { wrapper })
  await waitFor(() => expect(view.result.current.state.status).toBe('authenticated'))
  return view
}

describe('AuthProvider: initial session check', () => {
  it('starts "checking" then becomes "authenticated" on a valid session', async () => {
    const { result } = await renderAuthenticated(() => jsonResponse(200, { authenticated: true, csrf_token: 'csrf-abc' }))
    expect(result.current.state).toEqual({ status: 'authenticated', csrfToken: 'csrf-abc' })
    expect(result.current.csrfToken).toBe('csrf-abc')
  })

  it('becomes "anonymous" with reason "initial" on a 401 from GET /api/session', async () => {
    installMockFetch(() => apiError(401, 'unauthorized', 'Требуется вход'))
    const { result } = renderHook(() => useAuth(), { wrapper })
    await waitFor(() => expect(result.current.state.status).toBe('anonymous'))
    // Nothing was ever on screen to preserve: this must not be reported as
    // an expiry (which would keep a workspace mounted behind a re-login).
    expect(result.current.state).toEqual({ status: 'anonymous', reason: 'initial' })
  })

  it('becomes "unreachable" when the server cannot be reached at all', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(() => Promise.reject(new TypeError('network down'))),
    )
    const { result } = renderHook(() => useAuth(), { wrapper })
    await waitFor(() => expect(result.current.state.status).toBe('unreachable'))
  })
})

describe('runMutation', () => {
  it('refreshes the CSRF token exactly once and replays the mutation on invalid_csrf_token', async () => {
    const { result } = await renderAuthenticated((n) =>
      jsonResponse(200, { authenticated: true, csrf_token: n === 1 ? 'initial-token' : 'fresh-token' }),
    )
    expect(result.current.csrfToken).toBe('initial-token')

    const seenTokens: string[] = []
    const outcome = await act(async () =>
      result.current.runMutation(async (token) => {
        seenTokens.push(token)
        if (token === 'initial-token') {
          throw new ApiError(403, { code: 'invalid_csrf_token', message: 'stale', retryable: true })
        }
        return `ok:${token}`
      }),
    )

    expect(seenTokens).toEqual(['initial-token', 'fresh-token'])
    expect(outcome).toBe('ok:fresh-token')
    expect(result.current.csrfToken).toBe('fresh-token')
  })

  it('does not loop forever if the replayed request also fails with invalid_csrf_token', async () => {
    const { result } = await renderAuthenticated(() => jsonResponse(200, { authenticated: true, csrf_token: 'token' }))

    let attempts = 0
    await expect(
      act(async () =>
        result.current.runMutation(async () => {
          attempts += 1
          throw new ApiError(403, { code: 'invalid_csrf_token', message: 'still stale', retryable: true })
        }),
      ),
    ).rejects.toBeInstanceOf(ApiError)
    expect(attempts).toBe(2) // exactly one retry, never an unbounded loop
  })

  it('on a real 401, flips to anonymous/expired and throws SessionExpiredError (caller keeps its own state)', async () => {
    const { result } = await renderAuthenticated(() => jsonResponse(200, { authenticated: true, csrf_token: 'token' }))

    let caught: unknown
    await act(async () => {
      try {
        await result.current.runMutation(async () => {
          throw new ApiError(401, { code: 'unauthorized', message: 'expired', retryable: false })
        })
      } catch (err) {
        caught = err
      }
    })

    expect(caught).toBeInstanceOf(SessionExpiredError)
    expect(result.current.state).toEqual({ status: 'anonymous', reason: 'expired' })
  })

  it('treats a 401 from the CSRF-refresh GET /api/session as expiry, not as a raw failure', async () => {
    // First GET establishes the session; the refresh GET triggered by the
    // 403 finds the session already gone.
    const { result } = await renderAuthenticated((n) =>
      n === 1
        ? jsonResponse(200, { authenticated: true, csrf_token: 'token' })
        : apiError(401, 'unauthorized', 'Сессия недействительна'),
    )

    let attempts = 0
    let caught: unknown
    await act(async () => {
      try {
        await result.current.runMutation(async () => {
          attempts += 1
          throw new ApiError(403, { code: 'invalid_csrf_token', message: 'stale', retryable: true })
        })
      } catch (err) {
        caught = err
      }
    })

    // The mutation is never replayed: there is no valid token to replay with.
    expect(attempts).toBe(1)
    expect(caught).toBeInstanceOf(SessionExpiredError)
    expect(result.current.state).toEqual({ status: 'anonymous', reason: 'expired' })
  })

  it('treats a 401 from the single replayed mutation as expiry, not as a raw failure', async () => {
    const { result } = await renderAuthenticated((n) =>
      jsonResponse(200, { authenticated: true, csrf_token: n === 1 ? 'initial-token' : 'fresh-token' }),
    )

    const seenTokens: string[] = []
    let caught: unknown
    await act(async () => {
      try {
        await result.current.runMutation(async (token) => {
          seenTokens.push(token)
          if (token === 'initial-token') {
            throw new ApiError(403, { code: 'invalid_csrf_token', message: 'stale', retryable: true })
          }
          // The session died between the 403 and the replay.
          throw new ApiError(401, { code: 'unauthorized', message: 'expired', retryable: false })
        })
      } catch (err) {
        caught = err
      }
    })

    expect(seenTokens).toEqual(['initial-token', 'fresh-token']) // exactly one replay
    expect(caught).toBeInstanceOf(SessionExpiredError)
    expect(result.current.state).toEqual({ status: 'anonymous', reason: 'expired' })
  })

  it('does not mistake an unrelated error from the replay for expiry', async () => {
    const { result } = await renderAuthenticated((n) =>
      jsonResponse(200, { authenticated: true, csrf_token: n === 1 ? 'initial-token' : 'fresh-token' }),
    )

    let caught: unknown
    await act(async () => {
      try {
        await result.current.runMutation(async (token) => {
          if (token === 'initial-token') {
            throw new ApiError(403, { code: 'invalid_csrf_token', message: 'stale', retryable: true })
          }
          throw new ApiError(422, { code: 'invalid_name', message: 'Название слишком длинное', retryable: false })
        })
      } catch (err) {
        caught = err
      }
    })

    expect(caught).toBeInstanceOf(ApiError)
    expect((caught as ApiError).status).toBe(422)
    expect(result.current.state.status).toBe('authenticated')
  })

  it('does not retry an ambiguous network error or an unrelated error', async () => {
    const { result } = await renderAuthenticated(() => jsonResponse(200, { authenticated: true, csrf_token: 'token' }))

    let attempts = 0
    await expect(
      act(async () =>
        result.current.runMutation(async () => {
          attempts += 1
          throw new TypeError('failed to fetch')
        }),
      ),
    ).rejects.toBeInstanceOf(TypeError)
    expect(attempts).toBe(1)
    // Session must still be considered valid: an ambiguous error is not
    // treated as proof of session expiry.
    expect(result.current.state.status).toBe('authenticated')
  })
})

describe('logout', () => {
  it('ends as anonymous/logged-out — never as "expired" — when the server confirms it', async () => {
    installMockFetch((req) => {
      if (req.url === '/api/session' && req.method === 'GET') {
        return jsonResponse(200, { authenticated: true, csrf_token: 'token' })
      }
      if (req.url === '/api/session' && req.method === 'DELETE') {
        expect(req.headers['x-csrf-token']).toBe('token')
        return new Response(null, { status: 204 })
      }
      throw new Error(`unhandled request: ${req.method} ${req.url}`)
    })
    const { result } = renderHook(() => useAuth(), { wrapper })
    await waitFor(() => expect(result.current.state.status).toBe('authenticated'))

    await act(async () => result.current.logout())

    // 'logged-out' is what lets App.tsx unmount the workspace (discarding
    // retained drafts) and show the ordinary login screen instead of a
    // "session expired" prompt over preserved work.
    expect(result.current.state).toEqual({ status: 'anonymous', reason: 'logged-out' })
  })

  it('keeps the session authenticated and surfaces the error if DELETE /api/session fails', async () => {
    let sessionCalls = 0
    installMockFetch((req) => {
      if (req.url === '/api/session' && req.method === 'GET') {
        sessionCalls += 1
        return jsonResponse(200, { authenticated: true, csrf_token: 'token' })
      }
      if (req.url === '/api/session' && req.method === 'DELETE') {
        return apiError(500, 'internal', 'Сервер недоступен')
      }
      throw new Error(`unhandled request: ${req.method} ${req.url}`)
    })
    const { result } = renderHook(() => useAuth(), { wrapper })
    await waitFor(() => expect(result.current.state.status).toBe('authenticated'))

    await expect(act(async () => result.current.logout())).rejects.toBeInstanceOf(ApiError)
    // The HttpOnly cookie is still live server-side; the UI must not claim
    // otherwise.
    expect(result.current.state.status).toBe('authenticated')
  })
})
