import { act, renderHook, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { Material } from '../../api/types'
import { jsonResponse } from '../../test/mockFetch'
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

afterEach(() => {
  vi.unstubAllGlobals()
  vi.useRealTimers()
})

describe('useMaterials', () => {
  it('loads the list once and does not keep polling once every material is in a terminal state', async () => {
    const fetchMock = vi.fn(async () =>
      jsonResponse(200, { items: [makeMaterial({ status: 'completed' })] }),
    )
    vi.stubGlobal('fetch', fetchMock)

    const { result } = renderHook(() => useMaterials('partner-a'))
    // Real timers for the initial (non-polling) load: it's a plain promise
    // chain, so there is nothing for fake timers to advance yet.
    await waitFor(() => expect(result.current.materials).toHaveLength(1))
    expect(fetchMock).toHaveBeenCalledTimes(1)

    vi.useFakeTimers()
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000)
    })
    expect(fetchMock).toHaveBeenCalledTimes(1) // no further polling once settled
  })

  it('polls while a material is queued/processing and picks up the persisted completion', async () => {
    let call = 0
    const fetchMock = vi.fn(async () => {
      call += 1
      const status = call === 1 ? 'queued' : 'completed'
      return jsonResponse(200, { items: [makeMaterial({ status })] })
    })
    vi.stubGlobal('fetch', fetchMock)

    const { result } = renderHook(() => useMaterials('partner-a'))
    await waitFor(() => expect(result.current.materials?.[0]?.status).toBe('queued'))

    // Real timers here (not faked): the poll's setTimeout(POLL_MS) was
    // already armed with the real clock while awaiting the assertion above,
    // so switching to fake timers now would not affect that already-pending
    // timer. Waiting out the real ~3s interval keeps this deterministic.
    await waitFor(() => expect(result.current.materials?.[0]?.status).toBe('completed'), {
      timeout: 5000,
      interval: 100,
    })
    expect(fetchMock).toHaveBeenCalledTimes(2)
  })

  it('drops a stale response from the previous partner after switching (race guard)', async () => {
    vi.useRealTimers()
    let resolveA!: (r: Response) => void
    const pendingA = new Promise<Response>((resolve) => {
      resolveA = resolve
    })
    const fetchMock = vi.fn((input: string | URL | Request) => {
      const url = typeof input === 'string' ? input : input.toString()
      if (url.includes('partner-a')) return pendingA
      return Promise.resolve(
        jsonResponse(200, { items: [makeMaterial({ id: 'b1', partner_id: 'partner-b', status: 'completed' })] }),
      )
    })
    vi.stubGlobal('fetch', fetchMock)

    const { result, rerender } = renderHook(({ partnerId }) => useMaterials(partnerId), {
      initialProps: { partnerId: 'partner-a' },
    })

    // Switch partner before A's request resolves.
    rerender({ partnerId: 'partner-b' })
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
    const fetchMock = vi.fn(async () => jsonResponse(200, { items: [] }))
    vi.stubGlobal('fetch', fetchMock)

    const { result, unmount } = renderHook(() => useMaterials('partner-a'))
    await waitFor(() => expect(result.current.materials).toEqual([]))
    expect(fetchMock).toHaveBeenCalledTimes(1)

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
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it('upsert ignores a material belonging to a different partner_id', async () => {
    const fetchMock = vi.fn(async () => jsonResponse(200, { items: [] }))
    vi.stubGlobal('fetch', fetchMock)
    const { result } = renderHook(() => useMaterials('partner-a'))
    await waitFor(() => expect(result.current.materials).toEqual([]))

    act(() => {
      result.current.upsert(makeMaterial({ id: 'foreign', partner_id: 'partner-other' }))
    })

    expect(result.current.materials).toEqual([])
  })
})
