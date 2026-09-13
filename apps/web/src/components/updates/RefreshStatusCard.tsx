import type { RefreshPlan, RefreshStatus } from '../../api/types'
import {
  refreshOutcomePresentation,
  refreshStatePresentation,
  refreshStepLabel,
  sourceStatePresentation,
} from '../../lib/format'

interface RefreshStatusCardProps {
  status: RefreshStatus
  /** The result of the last "обновить" request, when one was made in this session. */
  plan: RefreshPlan | null
  busy: boolean
  actionError: string | null
  onRefresh: () => void
  onReprocess: (materialId: string) => void
  reprocessing: string | null
}

/**
 * What the published version is missing, in the server's own words.
 *
 * Three rules this component follows and the tests hold it to:
 *
 * 1. **No invented progress.** There is no bar and no estimate. While a check is
 *    running the state says so and the page polls; nothing here claims to know
 *    how far along it is.
 * 2. **Every reason names its source.** A reason about a document carries the
 *    file name and both reading numbers, because "что-то устарело" is not an
 *    answer the owner can act on.
 * 3. **The plan reports refusals as loudly as acceptances.** A stage that needs
 *    configuration is shown with the variables it is missing, not hidden.
 */
export function RefreshStatusCard({
  status,
  plan,
  busy,
  actionError,
  onRefresh,
  onReprocess,
  reprocessing,
}: RefreshStatusCardProps) {
  const presentation = refreshStatePresentation(status.state)

  return (
    <section className="refresh" data-state={status.state} aria-labelledby="refresh-heading">
      <header className="refresh__head">
        <div>
          <h3 id="refresh-heading" className="section-heading">
            Актуальность знаний
          </h3>
          <p className="refresh__status" data-tone={presentation.tone}>
            {presentation.label}
          </p>
        </div>
        <button
          type="button"
          className="button button-outline"
          onClick={onRefresh}
          disabled={busy || status.checking}
          title={
            status.checking
              ? 'Проверка уже идёт: повторный запуск присоединится к ней'
              : undefined
          }
        >
          {busy ? 'Ставим в очередь…' : 'Обновить'}
        </button>
      </header>

      {/* The server's sentence, not one assembled here from the state. */}
      <p className="refresh__hint">{status.message || presentation.defaultHint}</p>

      {status.published ? (
        <p className="refresh__version">
          Сейчас опубликована версия {status.published.number}.
        </p>
      ) : (
        <p className="refresh__version">Опубликованной версии нет.</p>
      )}

      {actionError ? (
        <p className="field-error" role="alert">
          {actionError}
        </p>
      ) : null}

      {status.reasons.length > 0 ? (
        <div className="refresh__reasons">
          <p className="run__label">Что изменилось после публикации — дословно:</p>
          <ul aria-label="Причины, по которым нужна перепроверка">
            {status.reasons.map((reason, index) => (
              <li key={`${reason.code}-${reason.material_id ?? reason.version_id ?? index}`}>
                <span className="refresh__reason-code">{reason.code}</span>
                <span className="refresh__reason-text">{reason.message}</span>
                {reason.material_filename ? (
                  <span className="refresh__reason-source">
                    источник: {reason.material_filename}
                    {reason.drafted_revision != null && reason.content_revision != null
                      ? ` (разбор по чтению №${reason.drafted_revision}, сейчас №${reason.content_revision})`
                      : ''}
                  </span>
                ) : null}
                {reason.version_number != null ? (
                  <span className="refresh__reason-source">
                    версия: {reason.version_number}
                  </span>
                ) : null}
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {plan ? (
        <div className="refresh__plan">
          <p className="run__label">
            Последний запрос обновления — поставлено в очередь шагов: {plan.queued}
          </p>
          <p className="refresh__hint">{plan.message}</p>
          <ul aria-label="Что было запущено и что нет">
            {plan.steps.map((step, index) => {
              const outcome = refreshOutcomePresentation(step.outcome)
              return (
                <li key={`${step.kind}-${step.material_id ?? index}`} data-outcome={step.outcome}>
                  <span className="refresh__step-kind">{refreshStepLabel(step.kind)}</span>
                  <span className="refresh__step-outcome" data-tone={outcome.tone}>
                    {outcome.label}
                  </span>
                  <span className="refresh__step-message">{step.message}</span>
                </li>
              )
            })}
          </ul>
        </div>
      ) : null}

      <div className="refresh__sources">
        <p className="run__label">Материалы и их состояние в цикле</p>
        {status.sources.length === 0 ? (
          <p className="empty-state">У партнёра ещё нет материалов.</p>
        ) : (
          <ul aria-label="Состояние материалов">
            {status.sources.map((source) => {
              const state = sourceStatePresentation(source.state)
              return (
                <li key={source.material_id} data-state={source.state}>
                  <div className="refresh__source-head">
                    <span className="refresh__source-name">{source.filename}</span>
                    <span className="refresh__source-state" data-tone={state.tone}>
                      {state.label}
                    </span>
                    <button
                      type="button"
                      className="button button-ghost button-small"
                      onClick={() => onReprocess(source.material_id)}
                      disabled={
                        reprocessing === source.material_id ||
                        source.state === 'reading'
                      }
                      title={
                        source.state === 'reading'
                          ? 'Материал уже читается или стоит в очереди'
                          : 'Прочитать документ заново. Оригинал и опубликованные версии не меняются'
                      }
                    >
                      {reprocessing === source.material_id ? 'Ставим…' : 'Прочитать заново'}
                    </button>
                  </div>
                  <p className="refresh__source-message">{source.message}</p>
                  <p className="refresh__source-counts">
                    {[
                      `чтений: ${source.content_revision}`,
                      `разбор по чтению: ${source.drafted_revision ?? 'неизвестно'}`,
                      `фактов: ${source.facts_drafted}`,
                      `в опубликованной версии: ${source.claims_in_published}`,
                    ].join(' · ')}
                  </p>
                </li>
              )
            })}
          </ul>
        )}
      </div>
    </section>
  )
}
