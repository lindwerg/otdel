import { useState } from 'react'
import { searchPublished } from '../../api/client'
import type { RetrievalLimits, SearchResponse } from '../../api/types'
import { useAuth } from '../../auth/AuthContext'
import {
  matchKindLabel,
  searchModeLabel,
  searchStatePresentation,
  versionStatusPresentation,
} from '../../lib/format'
import { ClaimList } from './ClaimList'
import { VersionGapList } from './ReadinessMatrix'

interface SearchPanelProps {
  partnerId: string
  limits: RetrievalLimits
}

/**
 * Search inside one published version.
 *
 * Three honesty rules shape this screen.
 *
 * The **mode** is stated on every response, not just the degraded ones. `mode`
 * is what actually ran, and `degraded[]` says in words why the vector half was
 * unavailable — no embedding provider, no `pgvector`, or a version without
 * vectors of that profile. A keyword-only result set that looked complete would
 * quietly teach the owner that the knowledge base has nothing on a topic, when
 * in fact half the search was switched off.
 *
 * The **version** is named on every response. Results of one answer always
 * belong to one version (§9), and printing its number and status is how a reader
 * can tell that two searches were not silently answered from two snapshots.
 *
 * `no_published_version` and `insufficient_evidence` are rendered as named
 * states, never as an empty list. An empty list is what a broken search looks
 * like; these are what a working one looks like when there is nothing to show.
 */
export function SearchPanel({ partnerId, limits }: SearchPanelProps) {
  const { runMutation } = useAuth()
  const [query, setQuery] = useState('')
  const [busy, setBusy] = useState(false)
  const [result, setResult] = useState<SearchResponse | null>(null)
  const [error, setError] = useState<string | null>(null)

  async function submit(event: React.FormEvent) {
    event.preventDefault()
    const text = query.trim()
    if (text.length === 0) return
    setBusy(true)
    setError(null)
    try {
      // POST: the query text has no business in a URL, a log or browser history.
      const response = await runMutation((token) => searchPublished(partnerId, { query: text }, token))
      setResult(response)
    } catch (err) {
      // The server's own refusal is more useful than anything invented here.
      setError(err instanceof Error ? err.message : 'Не удалось выполнить поиск.')
    } finally {
      setBusy(false)
    }
  }

  const statePresentation = result ? searchStatePresentation(result.state) : null

  return (
    <section className="retrieval-box search" aria-labelledby="search-heading">
      <h3 id="search-heading" className="knowledge-subheading">
        Поиск по опубликованной версии
      </h3>
      <p className="knowledge-note">
        Ищется только опубликованная версия знаний. Черновики этапов разбора и исследования через
        этот поиск не видны вовсе.
      </p>

      <form className="retrieval-form" onSubmit={(event) => void submit(event)}>
        <label className="field">
          <span>Запрос</span>
          <input
            type="text"
            value={query}
            maxLength={limits.max_query_chars}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="например: толщина покрытия"
          />
        </label>
        <button
          type="submit"
          className="button button-outline button-small"
          disabled={busy || query.trim().length === 0}
        >
          {busy ? 'Ищем…' : 'Найти'}
        </button>
      </form>

      {error ? (
        <p className="field-error" role="alert">
          {error}
        </p>
      ) : null}

      {result && statePresentation ? (
        <div className="retrieval-result" data-state={result.state}>
          <p className="retrieval-result__state" data-tone={statePresentation.tone}>
            {statePresentation.label}
          </p>
          <p className="retrieval-result__message">{result.message || statePresentation.defaultHint}</p>

          <p className="retrieval-result__version">
            {result.version
              ? `версия ${result.version.number} · ${versionStatusPresentation(result.version.status).label}`
              : 'версия не закреплена: публиковать пока нечего'}
          </p>

          <p className="retrieval-result__mode" data-mode={result.mode}>
            режим: {searchModeLabel(result.mode)}
          </p>
          {result.degraded.length > 0 ? (
            <ul className="retrieval-result__degraded" aria-label="Почему режим поиска неполный">
              {result.degraded.map((reason) => (
                <li key={reason}>{reason}</li>
              ))}
            </ul>
          ) : null}

          {result.items.length > 0 ? (
            <ul className="hit-list" aria-label="Найденные утверждения">
              {result.items.map((hit) => (
                <li key={hit.claim.id} className="hit">
                  <p className="hit__matched">
                    {/* Why this was found — never a confidence percentage: `score`
                        is comparable inside this one response and nowhere else. */}
                    {hit.matched_by.map(matchKindLabel).join(', ')}
                  </p>
                  <ClaimList
                    partnerId={partnerId}
                    claims={[hit.claim]}
                    label={`Утверждение: ${hit.claim.attribute}`}
                  />
                </li>
              ))}
            </ul>
          ) : null}

          {result.gaps.length > 0 ? (
            <>
              <h4 className="retrieval-result__subheading">Что версия об этом не знает</h4>
              <VersionGapList gaps={result.gaps} />
            </>
          ) : null}
        </div>
      ) : null}
    </section>
  )
}
