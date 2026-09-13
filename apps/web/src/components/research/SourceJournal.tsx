import { useCallback, useEffect, useRef, useState } from 'react'
import { listResearchQueries, listResearchSources } from '../../api/client'
import type { ResearchQueryRecord, ResearchSource } from '../../api/types'
import { useAuth } from '../../auth/AuthContext'
import {
  formatBytes,
  formatDateTime,
  formatMicros,
  queryOutcomeLabel,
  sourceStatusPresentation,
} from '../../lib/format'
import { StatusMessage } from '../StatusMessage'

interface SourceJournalProps {
  partnerId: string
  planId: string
  currency: string
}

/**
 * What one plan actually asked, and what it actually read.
 *
 * Loaded on demand — a plan's journal is not something the overview needs, and
 * fetching every plan's sources to render a list nobody opened would be a
 * request nobody asked for.
 *
 * Every discovered URL appears, including the ones that were never opened, with
 * the reason. That is the whole point of a journal: "эта ссылка не читалась,
 * потому что её хост не разрешён" is information, and a list that silently
 * dropped those rows would make the search look as if it had found nothing.
 */
export function SourceJournal({ partnerId, planId, currency }: SourceJournalProps) {
  const { runRead } = useAuth()
  const [queries, setQueries] = useState<ResearchQueryRecord[] | null>(null)
  const [sources, setSources] = useState<ResearchSource[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  // Which plan the component is currently showing. A response for a plan the owner has
  // since closed must not land on the one they opened next — the same race `useResearch`
  // guards, and the same guard.
  const epochRef = useRef(0)

  // The error is cleared on success rather than at the start of the call: clearing it
  // here would be a synchronous state update inside the effect below, costing an extra
  // render pass to show a blank state nobody sees.
  const load = useCallback(
    (epoch: number) => {
      return runRead(() =>
        Promise.all([
          listResearchQueries(partnerId, planId),
          listResearchSources(partnerId, planId),
        ]),
      ).then(
        ([loadedQueries, loadedSources]) => {
          if (epochRef.current !== epoch) return
          setError(null)
          setQueries(loadedQueries)
          setSources(loadedSources)
        },
        (err: unknown) => {
          if (epochRef.current !== epoch) return
          setError(err instanceof Error ? err.message : 'Не удалось загрузить журнал источников.')
        },
      )
    },
    [partnerId, planId, runRead],
  )

  useEffect(() => {
    epochRef.current += 1
    const epoch = epochRef.current
    void load(epoch)
    return () => {
      // Unmounted or switched plans: whatever is still in flight belongs to the past.
      epochRef.current += 1
    }
  }, [load])

  if (error) {
    return (
      <StatusMessage tone="error" onRetry={() => void load(epochRef.current)}>
        {error}
      </StatusMessage>
    )
  }
  if (!queries || !sources) {
    return <p className="knowledge-note">Загружаем журнал…</p>
  }

  return (
    <div className="journal">
      <h5 className="journal__heading">Запросы, которые были отправлены</h5>
      {queries.length === 0 ? (
        <p className="knowledge-note">Ни одного запроса не отправлялось.</p>
      ) : (
        <ul className="journal__queries">
          {queries.map((query) => (
            <li key={query.id} className="journal__query" data-outcome={query.outcome}>
              <code className="journal__query-text">{query.query_text}</code>
              <span className="journal__query-meta">
                {queryOutcomeLabel(query.outcome)} · результатов: {query.results_count} ·{' '}
                {formatMicros(query.cost_micros, currency)}
              </span>
              {query.diagnostic ? (
                <span className="journal__query-reason">{query.diagnostic}</span>
              ) : null}
            </li>
          ))}
        </ul>
      )}

      <h5 className="journal__heading">Источники</h5>
      {sources.length === 0 ? (
        <p className="knowledge-note">Источников не найдено.</p>
      ) : (
        <ul className="journal__sources">
          {sources.map((source) => {
            const presentation = sourceStatusPresentation(source.status)
            return (
              <li key={source.id} className="journal__source" data-status={source.status}>
                <div className="journal__source-head">
                  <a
                    className="journal__source-url"
                    href={source.url}
                    target="_blank"
                    rel="noreferrer nofollow"
                  >
                    {source.url}
                  </a>
                  <span className="journal__source-status" data-tone={presentation.tone}>
                    {presentation.label}
                  </span>
                </div>

                <p className="journal__source-reason">
                  {source.diagnostic || presentation.defaultHint}
                </p>

                {source.status === 'fetched' ? (
                  <p className="journal__source-meta">
                    {source.retrieved_at ? `прочитано ${formatDateTime(source.retrieved_at)}` : ''}
                    {source.content_bytes != null ? ` · ${formatBytes(source.content_bytes)}` : ''}
                    {source.content_chars != null ? ` · ${source.content_chars} символов` : ''}
                    {source.content_hash ? ` · sha256 ${source.content_hash.slice(0, 12)}…` : ''}
                  </p>
                ) : null}

                <p className="journal__source-license">
                  {source.license
                    ? `лицензия: ${source.license}`
                    : source.license_note || 'лицензия не объявлена источником'}
                </p>

                {source.snippet ? (
                  <p className="journal__source-snippet">
                    <span className="journal__snippet-badge">
                      описание поисковика — не цитата источника
                    </span>
                    {source.snippet}
                  </p>
                ) : null}
              </li>
            )
          })}
        </ul>
      )}
    </div>
  )
}
