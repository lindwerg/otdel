import { useCallback, useEffect, useRef, useState } from 'react'
import {
  fetchRetrievalProvider,
  fetchValidationOverview,
  listVersionClaims,
  listVersionGaps,
} from '../api/client'
import type {
  RetrievalProviderState,
  ValidationOverview,
  VersionClaim,
  VersionGap,
} from '../api/types'
import { useAuth } from '../auth/AuthContext'

const POLL_MS = 4000

export interface PublicationData {
  overview: ValidationOverview
  provider: RetrievalProviderState
  /** Claims of the **published** version, or `[]` when nothing is published. */
  claims: VersionClaim[]
  gaps: VersionGap[]
}

/**
 * Loads a partner's checks and versions, and polls only while a check is
 * actually queued or running.
 *
 * `completed`, `partial` and `failed` are settled: polling through them would
 * spin over work that will not change until somebody presses the button again.
 * Same epoch-based race safety as `useResearch` — see its doc comment — so a
 * response for the previous partner can never land on the current one.
 *
 * Two deliberate shapes here.
 *
 * The installation's provider state is fetched from `/api/retrieval/provider`
 * even though the overview carries a copy of it. The copy travels with partner
 * data; the question it answers ("is an embedding provider configured on this
 * machine") is not a property of a partner, and reading it from the endpoint
 * that owns it keeps those two apart.
 *
 * The published version's claims are a second round-trip, not a client-side
 * join: they are addressed by version id, and that id is only known once the
 * overview has answered. Nothing is guessed in between — while the id is
 * unknown, there are simply no claims, which is also exactly true.
 */
export function usePublication(partnerId: string) {
  const { runRead } = useAuth()
  const [data, setData] = useState<PublicationData | null>(null)
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
      // The overview and the provider are independent, so they go together
      // rather than in a waterfall.
      return runRead(async () => {
        const [overview, provider] = await Promise.all([
          fetchValidationOverview(partnerId),
          fetchRetrievalProvider(),
        ])
        const published = overview.published
        if (!published) {
          return { overview, provider, claims: [], gaps: [] } satisfies PublicationData
        }
        const [claims, gaps] = await Promise.all([
          listVersionClaims(partnerId, published.id),
          listVersionGaps(partnerId, published.id),
        ])
        return { overview, provider, claims, gaps } satisfies PublicationData
      }).then(
        (next) => {
          if (epochRef.current !== epoch) return
          setData(next)

          const working = next.overview.runs.some(
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
          setError(err instanceof Error ? err.message : 'Не удалось загрузить версии знаний.')
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
