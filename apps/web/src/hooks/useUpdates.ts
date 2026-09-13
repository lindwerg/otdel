import { useCallback, useEffect, useRef, useState } from 'react'
import { fetchRefreshStatus, fetchRetentionPolicy, listEvents } from '../api/client'
import type { HistoryEvent, RefreshStatus, RetentionPolicy } from '../api/types'
import { useAuth } from '../auth/AuthContext'

const POLL_MS = 4000
/** How much history one screen asks for. The endpoint caps this at 200. */
const EVENT_LIMIT = 50

export interface UpdatesData {
  status: RefreshStatus
  events: HistoryEvent[]
  retention: RetentionPolicy
}

/**
 * Loads the partner's refresh status, history and the installation's retention
 * policy, and polls only while a check is actually running.
 *
 * Same epoch-based race safety as `usePublication` — see its doc comment — so a
 * response for the previous partner can never land on the current one.
 *
 * The three reads go together because they are independent and the screen shows
 * them at once. The retention policy is a property of the installation rather
 * than of the partner, and it is read from the endpoint that owns it for the
 * same reason `usePublication` reads the provider state from `/retrieval/provider`
 * instead of taking the copy that travels with partner data.
 *
 * Polling is armed from work the server says is genuinely unfinished: a check
 * that is queued or running, a document being read, or a drafting run that is
 * queued or running. `checking` alone was not enough — `POST /refresh` most
 * often queues extraction and drafting and no check, so the card would sit
 * showing «Читается» for ever without asking again, while the plan told the
 * owner to watch it. Nothing here estimates how long any of it takes.
 */
export function useUpdates(partnerId: string) {
  const { runRead } = useAuth()
  const [data, setData] = useState<UpdatesData | null>(null)
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
      return runRead(async () => {
        const [status, events, retention] = await Promise.all([
          fetchRefreshStatus(partnerId),
          listEvents(partnerId, EVENT_LIMIT),
          fetchRetentionPolicy(),
        ])
        return { status, events, retention } satisfies UpdatesData
      }).then(
        (next) => {
          if (epochRef.current !== epoch) return
          setData(next)

          const working =
            next.status.checking ||
            next.status.sources.some(
              (source) =>
                source.state === 'reading' ||
                source.draft_status === 'queued' ||
                source.draft_status === 'running',
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
          setError(
            err instanceof Error ? err.message : 'Не удалось загрузить состояние обновлений.',
          )
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
