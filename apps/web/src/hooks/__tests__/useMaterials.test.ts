import { act, renderHook, waitFor } from '@testing-library/react'
import { createElement, type ReactNode } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { Material } from '../../api/types'
import { AuthProvider, useAuth } from '../../auth/AuthContext'
import { apiError, jsonResponse } from '../../test/mockFetch'
import { useMaterials } from '../useMaterials'

function makeMaterial(overrides: Partial<Material>): Material {
  return {
    id: 'm1',
    partner_id: 'partner-a',
    filename: 'file.pdf',
    media_type: 'application/pdf',
    size_bytes: 1024,
    sha256: 'abc',
    status: 'queued',
    page_count: null,
    created_at: '2026-01-01T00:00:00Z',
    error: null,
    ...overrides,
  }
}

// The hook reads `runRead` from the auth context, so it is exercised inside a
// real AuthProvider rather than against a stubbed context — that is the whole
// point of the session-expiry assertions below.
function wrapper({ children }: { children: ReactNode }) {
  return createElement(AuthProvider, null, children)
}

type MaterialsHandler = (call: number, url: string) => Response | Promise<Response>

/**
 * Answers GET /api/session with a valid session and routes everything else
 * to the test's own materials handler, counted separately so assertions stay
 * about materials requests only.
 */
function stubApi(materials: MaterialsHandler) {
  let materialCalls = 0
  const fetchMock = vi.fn((input: string | URL | Request) => {
    const url = typeof input === 'string' ? input : input.toString()
    if (url === '/api/session') {
      return Promise.resolve(jsonResponse(200, { authenticated: true, csrf_token: 'csrf-1' }))
    }
    materialCalls += 1
    return Promise.resolve(materials(materialCalls, url))
  })
  vi.stubGlobal('fetch', fetchMock)
  return { materialCalls: () => materialCalls }
}

function renderMaterials(partnerId = 'partner-a') {
  return renderHook(({ id }: { id: string }) => ({ ...useMaterials(id), auth: useAuth() }), {
    wrapper,
    initialProps: { id: partnerId },
  })
}

afterEach(() => {
  vi.unstubAllGlobals()
  vi.useRealTimers()
})

describe('useMaterials', () => {
  it('loads the list once and does not keep polling once every material is in a terminal state', async () => {
    const api = stubApi(() => jsonResponse(200, { items: [makeMaterial({ status: 'completed' })] }))

    const { result } = renderMaterials()
    // Real timers for the initial (non-polling) load: it's a plain promise
    // chain, so there is nothing for fake timers to advance yet.
    await waitFor(() => expect(result.current.materials).toHaveLength(1))
    expect(api.materialCalls()).toBe(1)

    vi.useFakeTimers()
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000)
    })
    expect(api.materialCalls()).toBe(1) // no further polling once settled
  })

  it('polls while a material is queued/processing and picks up the persisted completion', async () => {
    const api = stubApi((call) =>
      jsonResponse(200, { items: [makeMaterial({ status: call === 1 ? 'queued' : 'completed' })] }),
    )

    const { result } = renderMaterials()
    await waitFor(() => expect(result.current.materials?.[0]?.status).toBe('queued'))

    // Real timers here (not faked): the poll's setTimeout(POLL_MS) was
    // already armed with the real clock while awaiting the assertion above,
    // so switching to fake timers now would not affect that already-pending
    // timer. Waiting out the real ~3s interval keeps this deterministic.
    await waitFor(() => expect(result.current.materials?.[0]?.status).toBe('completed'), {
      timeout: 5000,
      interval: 100,
    })
    expect(api.materialCalls()).toBe(2)
  })

  it('raises session expiry (not just a load error) when the initial load returns 401', async () => {
    stubApi(() => apiError(401, 'unauthorized', 'Сессия недействительна'))

    const { result } = renderMaterials()

    await waitFor(() => expect(result.current.auth.state.status).toBe('anonymous'))
    // The whole point: the user is told to log in again instead of being
    // left with a retry button that can only ever produce another 401.
    expect(result.current.auth.state).toEqual({ status: 'anonymous', reason: 'expired' })
    expect(result.current.loadError).toMatch(/Сессия истекла/)
  })

  it('raises session expiry when a background poll — with no user action at all — returns 401', async () => {
    const api = stubApi((call) =>
      call === 1
        ? jsonResponse(200, { items: [makeMaterial({ status: 'processing' })] })
        : apiError(401, 'unauthorized', 'Сессия недействительна'),
    )

    const { result } = renderMaterials()
    await waitFor(() => expect(result.current.materials?.[0]?.status).toBe('processing'))
    expect(result.current.auth.state.status).toBe('authenticated')

    // Real timers, for the same reason as the polling test above.
    await waitFor(() => expect(result.current.auth.state.status).toBe('anonymous'), {
      timeout: 5000,
      interval: 100,
    })
    expect(result.current.auth.state).toEqual({ status: 'anonymous', reason: 'expired' })

    // Polling stopped at the 401 instead of hammering the server.
    expect(api.materialCalls()).toBe(2)
  })

  it('drops a stale response from the previous partner after switching (race guard)', async () => {
    vi.useRealTimers()
    let resolveA!: (r: Response) => void
    const pendingA = new Promise<Response>((resolve) => {
      resolveA = resolve
    })
    stubApi((_call, url) => {
      if (url.includes('partner-a')) return pendingA
      return jsonResponse(200, {
        items: [makeMaterial({ id: 'b1', partner_id: 'partner-b', status: 'completed' })],
      })
    })

    const { result, rerender } = renderMaterials('partner-a')

    // Switch partner before A's request resolves.
    rerender({ id: 'partner-b' })
    await waitFor(() => expect(result.current.materials?.[0]?.partner_id).toBe('partner-b'))

    // Now let the stale A response resolve late.
    await act(async () => {
      resolveA(jsonResponse(200, { items: [makeMaterial({ id: 'a1', partner_id: 'partner-a', status: 'queued' })] }))
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(result.current.materials).toHaveLength(1)
    expect(result.current.materials?.[0]?.partner_id).toBe('partner-b')
  })

  it('ignores a retry/upload response that lands after unmount and never re-arms polling', async () => {
    const api = stubApi(() => jsonResponse(200, { items: [] }))

    const { result, unmount } = renderMaterials()
    await waitFor(() => expect(result.current.materials).toEqual([]))
    expect(api.materialCalls()).toBe(1)

    // Grab the callback the way MaterialsPanel does: a retry/upload request
    // captured `upsert` before unmounting and resolves only afterwards.
    const lateUpsert = result.current.upsert
    unmount()

    vi.useFakeTimers()
    act(() => {
      // A *pending* material: this is precisely the input that would ask the
      // hook to start polling again.
      lateUpsert(makeMaterial({ id: 'late', status: 'processing' }))
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(30_000)
    })

    // No orphan poll: the unmounted hook must not keep hitting the server.
    expect(api.materialCalls()).toBe(1)
  })

  it('upsert ignores a material belonging to a different partner_id', async () => {
    stubApi(() => jsonResponse(200, { items: [] }))
    const { result } = renderMaterials()
    await waitFor(() => expect(result.current.materials).toEqual([]))

    act(() => {
      result.current.upsert(makeMaterial({ id: 'foreign', partner_id: 'partner-other' }))
    })

    expect(result.current.materials).toEqual([])
  })
})
