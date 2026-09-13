import { useCallback, useEffect, useRef, useState } from 'react'
import { fetchResearchOverview, listResearchFindings } from '../api/client'
import type { ResearchFinding, ResearchOverview } from '../api/types'
import { useAuth } from '../auth/AuthContext'

const POLL_MS = 4000

export interface ResearchData {
  overview: ResearchOverview
  findings: ResearchFinding[]
}

/**
 * Loads a partner's research, and polls only while a plan is actually queued or
 * running.
 *
 * `needs_provider`, `budget_exhausted` and `cancelled` are settled outcomes, not
 * steps on the way to something better: polling through them would spin over
 * work that will never start until the owner configures something, raises a
 * budget or presses the button again. Same reasoning as `useKnowledge` — see its
 * doc comment — and the same epoch-based race safety, so a response for the
 * previous partner can never land on the current one.
 */
export function useResearch(partnerId: string) {
  const { runRead } = useAuth()
  const [data, setData] = useState<ResearchData | null>(null)
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
      // The two collections are independent, so they are fetched together rather
      // than in a waterfall.
      return runRead(() =>
        Promise.all([fetchResearchOverview(partnerId), listResearchFindings(partnerId)]),
      ).then(
        ([overview, findings]) => {
          if (epochRef.current !== epoch) return
          setData({ overview, findings })

          const working = overview.plans.some(
            (plan) => plan.status === 'queued' || plan.status === 'running',
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
          setError(err instanceof Error ? err.message : 'Не удалось загрузить исследования.')
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
    // Drop a scheduled poll first: otherwise an older in-flight tick can land
    // after this reload and overwrite the fresher state with its own.
    clearTimer()
    void load(activeEpochRef.current)
  }, [clearTimer, load])

  return { data, error, reload }
}
