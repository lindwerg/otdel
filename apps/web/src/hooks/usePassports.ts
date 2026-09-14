import { useCallback, useEffect, useRef, useState } from 'react'
import {
  listApplications,
  listCoverage,
  listPassports,
  listUncertainties,
} from '../api/client'
import type {
  CoverageReport,
  KnowledgeUncertainty,
  ProductApplication,
  ProductPassport,
} from '../api/types'
import { useAuth } from '../auth/AuthContext'

const POLL_MS = 4000

export interface PassportData {
  passports: ProductPassport[]
  coverage: CoverageReport[]
  applications: ProductApplication[]
  uncertainties: KnowledgeUncertainty[]
}

/**
 * Loads a partner's product base, and polls only while a run is still working.
 *
 * The four collections are fetched together rather than in a waterfall, and —
 * more importantly — they are fetched *unconditionally*. A view that loaded the
 * passports eagerly and the uncertainties on demand would render a product base
 * that looks finished whenever the second request is slow, which is the exact
 * shape of the failure this surface exists to remove.
 *
 * Race safety follows `useKnowledge`: an epoch guards every response, so a reply
 * for the previous partner can never land on the current one.
 */
export function usePassports(partnerId: string) {
  const { runRead } = useAuth()
  const [data, setData] = useState<PassportData | null>(null)
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
      return runRead(() =>
        Promise.all([
          listPassports(partnerId),
          listCoverage(partnerId),
          listApplications(partnerId),
          listUncertainties(partnerId),
        ]),
      ).then(
        ([passports, coverage, applications, uncertainties]) => {
          if (epochRef.current !== epoch) return
          setData({ passports, coverage, applications, uncertainties })

          const working = coverage.some(
            (report) => report.status === 'queued' || report.status === 'running',
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
          setError(err instanceof Error ? err.message : 'Не удалось загрузить паспорта.')
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
    clearTimer()
    void load(activeEpochRef.current)
  }, [clearTimer, load])

  return { data, error, reload }
}
