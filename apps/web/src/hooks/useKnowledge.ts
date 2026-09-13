import { useCallback, useEffect, useRef, useState } from 'react'
import {
  fetchKnowledgeOverview,
  listGaps,
  listGlossary,
  listKnowledgeProducts,
  listKnowledgeQa,
} from '../api/client'
import type {
  GlossaryTerm,
  KnowledgeGap,
  KnowledgeOverview,
  ProductNode,
  QaEntry,
} from '../api/types'
import { useAuth } from '../auth/AuthContext'

const POLL_MS = 4000

export interface KnowledgeData {
  overview: KnowledgeOverview
  products: ProductNode[]
  glossary: GlossaryTerm[]
  qa: QaEntry[]
  gaps: KnowledgeGap[]
}

/**
 * Loads a partner's product draft, and polls only while a run is actually
 * queued or running.
 *
 * `needs_provider` is a settled outcome, not a step on the way to something
 * better: polling through it would show a spinner over work that will never
 * start until a key is configured. The same reasoning as `usePages` — see its
 * doc comment — and the same epoch-based race safety, so a response for the
 * previous partner can never land on the current one.
 */
export function useKnowledge(partnerId: string) {
  const { runRead } = useAuth()
  const [data, setData] = useState<KnowledgeData | null>(null)
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
      // The four collections are independent of each other, so they are
      // fetched together rather than in a waterfall.
      return runRead(() =>
        Promise.all([
          fetchKnowledgeOverview(partnerId),
          listKnowledgeProducts(partnerId),
          listGlossary(partnerId),
          listKnowledgeQa(partnerId),
          listGaps(partnerId),
        ]),
      ).then(
        ([overview, products, glossary, qa, gaps]) => {
          if (epochRef.current !== epoch) return
          setData({ overview, products, glossary, qa, gaps })

          const working = overview.runs.some(
            (run) => run.status === 'queued' || run.status === 'running',
          )
          if (working) {
            clearTimer()
            timerRef.current = window.setTimeout(() => {
              timerRef.current = null
              void load(epoch)
            }, POLL_MS)
          }
        },
        (err: unknown) => {
          if (epochRef.current !== epoch) return
          setError(err instanceof Error ? err.message : 'Не удалось загрузить знания.')
        },
      )
    },
    [partnerId, clearTimer, runRead],
  )

  useEffect(() => {
    epochRef.current += 1
    const epoch = epochRef.current
    activeEpochRef.current = epoch
    mountedRef.current = true
    setData(null)
    setError(null)
    clearTimer()
    void load(epoch)
    return () => {
      epochRef.current += 1
      mountedRef.current = false
      clearTimer()
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [partnerId])

  const reload = useCallback(() => {
    if (!mountedRef.current) return
    // Drop a scheduled poll first: otherwise an older in-flight tick can land after
    // this reload and overwrite the fresher state with its own.
    clearTimer()
    void load(activeEpochRef.current)
  }, [clearTimer, load])

  return { data, error, reload }
}
