import { useCallback, useEffect, useRef, useState } from 'react'
import { listMaterials } from '../api/client'
import type { Material, MaterialStatus } from '../api/types'

const POLL_MS = 3000
const PENDING_STATUSES: MaterialStatus[] = ['queued', 'processing']

function hasPending(items: Material[]): boolean {
  return items.some((m) => PENDING_STATUSES.includes(m.status))
}

/**
 * Loads a partner's materials and polls the server (GET .../materials) while
 * any of them are still queued/processing, so status text reflects real,
 * persisted server state — never a client-invented percentage or timer.
 * Polling pauses (rather than retrying silently forever) on a fetch error,
 * surfacing it instead with a manual retry action.
 *
 * Race-safety: every fetch/timer is tagged with an "epoch" bumped whenever
 * `partnerId` changes (and once more on unmount). A response belonging to a
 * stale epoch is dropped instead of overwriting the current partner's state
 * — a plain single "mounted" boolean is not sufficient here, because it
 * gets flipped back to true by the *next* partner's effect run before an
 * in-flight request from the *previous* partner has necessarily settled.
 * `upsert` additionally checks the material's own `partner_id` field, since
 * it can be invoked directly (upload/retry) independently of this effect.
 */
export function useMaterials(partnerId: string) {
  const [materials, setMaterials] = useState<Material[] | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const materialsRef = useRef<Material[]>([])
  const timerRef = useRef<number | null>(null)
  const epochRef = useRef(0)
  // The epoch the *currently mounted* effect run owns. Deliberately distinct
  // from `epochRef`, which is also bumped on cleanup to invalidate in-flight
  // work: reading `epochRef.current` outside the effect (e.g. from a late
  // `upsert`) would hand out the already-bumped, post-unmount value and so
  // pass every staleness check it is supposed to fail.
  const activeEpochRef = useRef(0)
  const mountedRef = useRef(false)

  const clearTimer = useCallback(() => {
    if (timerRef.current != null) {
      window.clearTimeout(timerRef.current)
      timerRef.current = null
    }
  }, [])

  const armPoll = useCallback(
    (epoch: number) => {
      clearTimer()
      timerRef.current = window.setTimeout(async () => {
        timerRef.current = null
        try {
          const items = await listMaterials(partnerId)
          if (epochRef.current !== epoch) return // stale: partner switched or unmounted meanwhile
          materialsRef.current = items
          setMaterials(items)
          setLoadError(null)
          if (hasPending(items)) armPoll(epoch)
        } catch (err) {
          if (epochRef.current !== epoch) return
          setLoadError(
            err instanceof Error ? err.message : 'Не удалось обновить статус материалов.',
          )
        }
      }, POLL_MS)
    },
    [partnerId, clearTimer],
  )

  const load = useCallback(
    (epoch: number) => {
      setLoadError(null)
      return listMaterials(partnerId).then(
        (items) => {
          if (epochRef.current !== epoch) return
          materialsRef.current = items
          setMaterials(items)
          if (hasPending(items)) armPoll(epoch)
        },
        (err: unknown) => {
          if (epochRef.current !== epoch) return
          setLoadError(err instanceof Error ? err.message : 'Не удалось загрузить материалы.')
        },
      )
    },
    [partnerId, armPoll],
  )

  useEffect(() => {
    epochRef.current += 1
    const epoch = epochRef.current
    activeEpochRef.current = epoch
    mountedRef.current = true
    materialsRef.current = []
    setMaterials(null)
    setLoadError(null)
    clearTimer()
    void load(epoch)
    return () => {
      // Bump again so a response from THIS epoch that resolves after
      // unmount is also treated as stale, even if no next effect follows
      // (real unmount, not just a partner switch).
      epochRef.current += 1
      mountedRef.current = false
      clearTimer()
    }
    // `load`/`clearTimer` are recreated per-partnerId already; re-running
    // this effect must happen exactly once per partner switch.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [partnerId])

  const upsert = useCallback(
    (material: Material) => {
      // An upload/retry request outlives this hook: the user can close the
      // partner (or the whole workspace can unmount, e.g. on logout) while it
      // is still in flight, and its `.then(upsert)` still runs afterwards.
      // Without this guard such a late call would write state into an
      // unmounted hook and — worse — re-arm polling that nothing will ever
      // clear, because the effect cleanup has already run.
      if (!mountedRef.current) return
      // Defense in depth against a late upload/retry response landing after
      // the user has since switched to a different partner (see doc comment
      // above): the material's own partner_id is the ground truth here.
      if (material.partner_id !== partnerId) return
      const list = materialsRef.current
      const idx = list.findIndex((m) => m.id === material.id)
      const next = idx >= 0 ? list.map((m, i) => (i === idx ? material : m)) : [material, ...list]
      materialsRef.current = next
      setMaterials(next)
      if (hasPending(next) && timerRef.current == null) {
        // The mounted run's own epoch — never `epochRef.current`, which a
        // cleanup may already have bumped past it.
        armPoll(activeEpochRef.current)
      }
    },
    [partnerId, armPoll],
  )

  const reload = useCallback(() => {
    if (!mountedRef.current) return
    void load(activeEpochRef.current)
  }, [load])

  return { materials, loadError, reload, upsert }
}
