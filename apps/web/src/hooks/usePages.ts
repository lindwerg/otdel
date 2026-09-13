import { useCallback, useEffect, useRef, useState } from 'react'
import { listPages } from '../api/client'
import type { MaterialPage } from '../api/types'
import { useAuth } from '../auth/AuthContext'

const POLL_MS = 3000
/** Pages the worker has not settled yet — the only reason to keep polling. */
const PENDING: MaterialPage['status'][] = ['pending']

function hasPending(pages: MaterialPage[]): boolean {
  return pages.some((page) => PENDING.includes(page.status))
}

/**
 * Loads the page records of one material and polls while any page is still
 * waiting to be read.
 *
 * Polling stops as soon as every page has an outcome — including `needs_ocr`,
 * which is a settled outcome and not a step on the way to something better. A
 * spinner that kept turning over a page nobody is going to read would be the
 * interface lying about work in progress.
 *
 * Race-safety follows `useMaterials`: an epoch counter, bumped on every
 * material switch and once more on unmount, invalidates in-flight responses.
 */
export function usePages(partnerId: string, materialId: string) {
  const { runRead } = useAuth()
  const [pages, setPages] = useState<MaterialPage[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const timerRef = useRef<number | null>(null)
  const epochRef = useRef(0)
  const activeEpochRef = useRef(0)
  const mountedRef = useRef(false)

  const clearTimer = useCallback(() => {
    if (timerRef.current != null) {
      window.clearTimeout(timerRef.current)
      timerRef.current = null
    }
  }, [])

  const load = useCallback(
    (epoch: number): Promise<void> => {
      setError(null)
      return runRead(() => listPages(partnerId, materialId)).then(
        (items) => {
          if (epochRef.current !== epoch) return
          setPages(items)
          if (hasPending(items)) {
            clearTimer()
            timerRef.current = window.setTimeout(() => {
              timerRef.current = null
              void load(epoch)
            }, POLL_MS)
          }
        },
        (err: unknown) => {
          if (epochRef.current !== epoch) return
          setError(err instanceof Error ? err.message : 'Не удалось загрузить страницы.')
        },
      )
    },
    [partnerId, materialId, clearTimer, runRead],
  )

  useEffect(() => {
    epochRef.current += 1
    const epoch = epochRef.current
    activeEpochRef.current = epoch
    mountedRef.current = true
    setPages(null)
    setError(null)
    clearTimer()
    void load(epoch)
    return () => {
      epochRef.current += 1
      mountedRef.current = false
      clearTimer()
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [partnerId, materialId])

  const reload = useCallback(() => {
    if (!mountedRef.current) return
    void load(activeEpochRef.current)
  }, [load])

  /** Replace one page after a retry, keeping the rest untouched. */
  const upsert = useCallback((page: MaterialPage) => {
    if (!mountedRef.current) return
    setPages((prev) => {
      if (!prev) return prev
      const next = prev.map((item) => (item.id === page.id ? page : item))
      if (hasPending(next) && timerRef.current == null) {
        timerRef.current = window.setTimeout(() => {
          timerRef.current = null
          void load(activeEpochRef.current)
        }, POLL_MS)
      }
      return next
    })
    // `load` is stable per material; including it would re-create this callback
    // on every render of the list.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [load])

  return { pages, error, reload, upsert }
}
