import { useState } from 'react'
import { retractVersion, startValidation } from '../../api/client'
import type { CandidateSummary, ValidationRun } from '../../api/types'
import { useAuth } from '../../auth/AuthContext'
import { usePublication } from '../../hooks/usePublication'
import { formatDateTime, validationRunStatusPresentation } from '../../lib/format'
import { StatusMessage } from '../StatusMessage'
import { AskPanel } from './AskPanel'
import { ClaimList } from './ClaimList'
import { ReadinessMatrix, VersionGapList } from './ReadinessMatrix'
import { RetrievalNotice } from './RetrievalNotice'
import { SearchPanel } from './SearchPanel'
import { VersionCard } from './VersionCard'

interface PublicationPanelProps {
  partnerId: string
}

/** How many candidates exist right now — counts, never a readiness score. */
function candidatesLine(candidates: CandidateSummary): string {
  return (
    `кандидатов-фактов: ${candidates.facts} · отраслевых выводов: ${candidates.findings} · ` +
    `открытых пробелов: ${candidates.gaps_open} · разобранных материалов: ${candidates.materials_drafted}`
  )
}

/** What one check did, in the numbers it really recorded. */
function runCounts(run: ValidationRun): string {
  const parts = [
    `рассмотрено: ${run.claims_considered}`,
    `подтверждено источником: ${run.claims_source_supported}`,
    `гипотез: ${run.claims_hypothesis}`,
    `не установлено: ${run.claims_unknown}`,
    `противоречий: ${run.claims_conflicted}`,
    `устаревших: ${run.claims_stale}`,
    `отклонено: ${run.claims_rejected}`,
    `пробелов перенесено: ${run.gaps_carried}`,
    `фрагментов: ${run.chunks_created}`,
    `с векторами: ${run.chunks_embedded}`,
    // 0 here is the normal state of an installation without a model key, and it
    // does not lower the run's status — so it is labelled, not hidden.
    `второе мнение модели: ${run.model_reviewed}`,
  ]
  return parts.join(' · ')
}

/**
 * Checking, publishing, and everything that follows from a published version.
 *
 * The panel is organised around one rule: a **candidate is never shown here as
 * published**. Everything on this screen comes from a version snapshot — the
 * counters, the claims, the citations — and the 1C/1D drafts live on their own
 * tabs, where they are labelled as drafts. That is why the claim list below the
 * published version is loaded by version id and not assembled from the draft.
 *
 * The check button is disabled when the partner has no candidates at all, with
 * the reason in its `title`: the server answers 409 in that case, and a button
 * whose only possible outcome is a refusal is worse than a button that explains
 * itself. Nothing else disables it — in particular not the model adapter, which
 * this phase does not need.
 *
 * Retraction asks for a reason before it will act, because the contract makes the
 * reason mandatory for a reason: a version that vanished without one cannot be
 * told apart from one that broke.
 *
 * A failed refresh does not take the versions off the screen. What is shown is
 * still what the server last said, and losing the owner's place over a two-second
 * network blip helps nobody.
 */
export function PublicationPanel({ partnerId }: PublicationPanelProps) {
  const { runMutation } = useAuth()
  const { data, error, reload } = usePublication(partnerId)
  const [busy, setBusy] = useState<string | null>(null)
  const [actionErrors, setActionErrors] = useState<Record<string, string>>({})
  const [retractReason, setRetractReason] = useState('')

  async function act(key: string, run: (token: string) => Promise<unknown>, fallback: string) {
    setBusy(key)
    setActionErrors((prev) => {
      const next = { ...prev }
      delete next[key]
      return next
    })
    try {
      await runMutation(run)
      reload()
    } catch (err) {
      // The server's own reason — "нет ни одного кандидата", "версия не
      // опубликована" — beats anything this component could invent.
      setActionErrors((prev) => ({
        ...prev,
        [key]: err instanceof Error ? err.message : fallback,
      }))
    } finally {
      setBusy(null)
    }
  }

  if (!data) {
    return (
      <section aria-labelledby="publication-heading">
        <div className="section-heading">
          <h2 id="publication-heading">Версии и поиск</h2>
        </div>
        {error ? (
          <StatusMessage tone="error" onRetry={reload}>
            {error}
          </StatusMessage>
        ) : (
          <StatusMessage>Загружаем версии знаний…</StatusMessage>
        )}
      </section>
    )
  }

  const { overview, provider, claims, gaps } = data
  const published = overview.published
  const candidates = overview.candidates
  const hasCandidates = candidates.facts + candidates.findings > 0
  const currentRun = overview.runs[0] ?? null
  const checking = currentRun?.status === 'queued' || currentRun?.status === 'running'
  const reasonGiven = retractReason.trim().length > 0

  return (
    <section aria-labelledby="publication-heading" className="knowledge publication">
      <div className="section-heading">
        <h2 id="publication-heading">Версии и поиск</h2>
      </div>

      {error ? (
        <StatusMessage tone="error" onRetry={reload}>
          {error}
        </StatusMessage>
      ) : null}

      <RetrievalNotice provider={provider} />

      <p className="knowledge-summary">{candidatesLine(candidates)}</p>
      <p className="knowledge-note">
        Проверка применяет детерминированные правила ко всем кандидатам партнёра и либо собирает
        неизменяемую версию знаний, либо честно оставляет её неопубликованной. Искать и
        спрашивать можно только по опубликованной версии.
      </p>

      <div className="publication-actions">
        <button
          type="button"
          className="button button-outline button-small"
          onClick={() =>
            void act(
              'validate',
              (token) => startValidation(partnerId, token),
              'Не удалось поставить проверку в очередь.',
            )
          }
          disabled={busy === 'validate' || checking || !hasCandidates}
          title={
            hasCandidates
              ? checking
                ? 'Проверка уже идёт: повторный запуск вернёт тот же запуск'
                : undefined
              : 'Проверять нечего: у партнёра нет ни одного кандидата'
          }
        >
          {busy === 'validate' ? 'Ставим в очередь…' : 'Проверить и опубликовать'}
        </button>
      </div>

      {actionErrors.validate ? (
        <p className="field-error" role="alert">
          {actionErrors.validate}
        </p>
      ) : null}

      <h3 className="knowledge-subheading">Проверка</h3>
      {currentRun === null ? (
        <p className="knowledge-note">
          Проверка ещё не запускалась. Пока её не было, опубликованной версии нет и поиск по
          партнёру невозможен.
        </p>
      ) : (
        <div className="run" data-state={currentRun.status}>
          <div className="run__head">
            <strong>
              {validationRunStatusPresentation(currentRun.status).label}
              {currentRun.version_number === null
                ? ''
                : ` · версия ${currentRun.version_number}`}
            </strong>
            <span className="run__published">
              {currentRun.published ? 'версия опубликована' : 'версия не опубликована'}
            </span>
          </div>
          <p className="run__hint">
            {currentRun.diagnostic || validationRunStatusPresentation(currentRun.status).defaultHint}
          </p>
          <p className="run__counts">{runCounts(currentRun)}</p>
          <p className="run__meta">
            профиль правил: {currentRun.prompt_profile} · начата{' '}
            {currentRun.started_at ? formatDateTime(currentRun.started_at) : '—'}
            {currentRun.finished_at
              ? ` · завершена ${formatDateTime(currentRun.finished_at)}`
              : ''}
          </p>

          {currentRun.blocked_reasons.length > 0 ? (
            <div className="run__blocked">
              <p className="run__label">Почему публикации не было — дословно:</p>
              <ul aria-label="Причины, по которым проверка не опубликовала версию">
                {currentRun.blocked_reasons.map((reason) => (
                  <li key={reason}>{reason}</li>
                ))}
              </ul>
            </div>
          ) : null}

          {currentRun.rejections.length > 0 ? (
            <details className="run__rejections">
              <summary>Что не вошло в версию и почему</summary>
              <ul>
                {currentRun.rejections.map((reason) => (
                  <li key={reason}>{reason}</li>
                ))}
              </ul>
            </details>
          ) : null}
        </div>
      )}

      <h3 className="knowledge-subheading">Опубликованная версия</h3>
      {published === null ? (
        <p className="knowledge-note">
          Опубликованной версии нет. Это состояние, а не ошибка: проверка либо не проходила, либо
          её правила готовности не были выполнены, либо версию отозвали. Поиск и ответы по
          партнёру в таком состоянии не работают и не делают вид, что работают.
        </p>
      ) : (
        <>
          <VersionCard version={published} />
          <ReadinessMatrix readiness={published.readiness} />

          <div className="retract">
            <label className="field">
              <span>Причина отзыва версии</span>
              <input
                type="text"
                value={retractReason}
                maxLength={1000}
                onChange={(event) => setRetractReason(event.target.value)}
                placeholder="например: партнёр прислал новый прайс"
              />
            </label>
            <button
              type="button"
              className="button button-outline button-small"
              onClick={() =>
                void act(
                  'retract',
                  (token) =>
                    retractVersion(partnerId, published.id, retractReason.trim(), token),
                  'Не удалось отозвать версию.',
                )
              }
              disabled={busy === 'retract' || !reasonGiven}
              title={
                reasonGiven
                  ? undefined
                  : 'Укажите причину: отзыв без причины не отличить от сбоя'
              }
            >
              {busy === 'retract' ? 'Отзываем…' : 'Отозвать версию'}
            </button>
            <p className="retract__note">
              Отзыв снимает версию с публикации: поиск и ответы по партнёру перестают работать,
              пока не будет опубликована новая версия.
            </p>
          </div>

          {actionErrors.retract ? (
            <p className="field-error" role="alert">
              {actionErrors.retract}
            </p>
          ) : null}

          <h3 className="knowledge-subheading">Утверждения опубликованной версии</h3>
          <ClaimList
            partnerId={partnerId}
            claims={claims}
            label="Утверждения опубликованной версии"
            emptyNote="В опубликованной версии нет ни одного утверждения."
          />

          <h3 className="knowledge-subheading">Пробелы версии</h3>
          <VersionGapList gaps={gaps} />
        </>
      )}

      <h3 className="knowledge-subheading">История версий</h3>
      {overview.versions.length === 0 ? (
        <p className="knowledge-note">Версий ещё не собиралось.</p>
      ) : (
        <ul className="version-list" aria-label="История версий знаний">
          {overview.versions.map((version) => (
            <li key={version.id}>
              <VersionCard version={version} />
            </li>
          ))}
        </ul>
      )}

      <SearchPanel partnerId={partnerId} limits={provider.limits} />
      <AskPanel partnerId={partnerId} limits={provider.limits} />
    </section>
  )
}
