import { useState } from 'react'
import { approveResearch, stopResearch } from '../../api/client'
import type { ResearchPlan, ResearchSummary } from '../../api/types'
import { useAuth } from '../../auth/AuthContext'
import { useResearch } from '../../hooks/useResearch'
import { formatMicros, planStatusPresentation } from '../../lib/format'
import { StatusMessage } from '../StatusMessage'
import { BudgetMeter } from './BudgetMeter'
import { FindingList } from './FindingList'
import { ResearchNotice } from './ResearchNotice'
import { SourceJournal } from './SourceJournal'

interface ResearchPanelProps {
  partnerId: string
}

/** Counts, never a percentage: nothing here measures "how well researched" anything is. */
function summaryLine(summary: ResearchSummary, currency: string): string {
  return (
    `Исследований: ${summary.plans_total} · вопросов без исследования: ${summary.questions_open} · ` +
    `источников прочитано: ${summary.sources_fetched} · не прочитано: ${summary.sources_skipped} · ` +
    `выводов: ${summary.findings_total} · израсходовано: ${formatMicros(summary.spent_micros, currency)}`
  )
}

/** What one plan did, in the numbers it really recorded. */
function planCounts(plan: ResearchPlan, currency: string): string {
  const parts = [
    `запросов: ${plan.queries_made}`,
    `найдено ссылок: ${plan.results_seen}`,
    `прочитано страниц: ${plan.sources_fetched}`,
    `выводов: ${plan.findings_accepted}`,
  ]
  if (plan.sources_skipped > 0) parts.push(`не прочитано: ${plan.sources_skipped}`)
  if (plan.findings_rejected > 0) parts.push(`отклонено выводов: ${plan.findings_rejected}`)
  // The counters above describe the *last* pass — `start_pass` resets them. The money
  // does not reset, so it is labelled as the total it is rather than read as one more
  // number about the same pass.
  parts.push(`расход за все проходы: ${formatMicros(plan.spent_micros, currency)}`)
  parts.push(`проход ${plan.passes} из ${plan.max_passes}`)
  return parts.join(' · ')
}

export function ResearchPanel({ partnerId }: ResearchPanelProps) {
  const { runMutation } = useAuth()
  const { data, error, reload } = useResearch(partnerId)
  const [busy, setBusy] = useState<string | null>(null)
  const [actionErrors, setActionErrors] = useState<Record<string, string>>({})
  const [openJournal, setOpenJournal] = useState<string | null>(null)

  function clearError(key: string) {
    setActionErrors((prev) => {
      const next = { ...prev }
      delete next[key]
      return next
    })
  }

  async function act(key: string, run: (token: string) => Promise<unknown>, fallback: string) {
    setBusy(key)
    clearError(key)
    try {
      await runMutation(run)
      reload()
    } catch (err) {
      // The server's own reason — "нет поискового провайдера", "бюджет исчерпан",
      // "предел проходов" — is more useful than anything this component could invent.
      setActionErrors((prev) => ({
        ...prev,
        [key]: err instanceof Error ? err.message : fallback,
      }))
    } finally {
      setBusy(null)
    }
  }

  // A failed refresh does not take the research off the screen: what is shown is
  // still what the server last said, and hiding it behind a banner would lose the
  // owner's place over a two-second network blip.
  if (!data) {
    return (
      <section aria-labelledby="research-heading">
        <div className="section-heading">
          <h2 id="research-heading">Исследование</h2>
        </div>
        {error ? (
          <StatusMessage tone="error" onRetry={reload}>
            {error}
          </StatusMessage>
        ) : (
          <StatusMessage>Загружаем исследования…</StatusMessage>
        )}
      </section>
    )
  }

  const { overview, findings } = data
  const ready = overview.provider.state === 'ready'
  const currency = overview.budget.currency
  const pending = overview.questions.filter((question) => question.plan_id === null)

  return (
    <section aria-labelledby="research-heading" className="knowledge research">
      <div className="section-heading">
        <h2 id="research-heading">Исследование</h2>
      </div>

      {error ? (
        <StatusMessage tone="error" onRetry={reload}>
          {error}
        </StatusMessage>
      ) : null}

      <ResearchNotice provider={overview.provider} />
      <BudgetMeter budget={overview.budget} />

      <p className="knowledge-summary">{summaryLine(overview.summary, currency)}</p>
      <p className="knowledge-note">
        Исследуются только вопросы, которые вы утвердили. Название партнёра во внешний поиск не
        отправляется, читаются только объявленные хосты, и найденное остаётся кандидатом
        отраслевого знания — не характеристикой партнёра и не опубликованным фактом.
      </p>

      {/* The approval queue: 1C's industry questions, waiting for a decision. */}
      <h3 className="knowledge-subheading">Вопросы, ожидающие решения</h3>
      {pending.length === 0 ? (
        <p className="knowledge-note">
          Вопросов для отраслевого исследования нет. Они появляются из пробелов, найденных при
          разборе материалов.
        </p>
      ) : (
        <ul className="question-list" aria-label="Вопросы для отраслевого исследования">
          {pending.map((question) => (
            <li key={question.id} className="question">
              <p className="question__text">{question.text}</p>
              <p className="question__origin">
                пробел «{question.gap_topic}» в материале {question.material_filename}:{' '}
                {question.gap_missing}
              </p>
              <button
                type="button"
                className="button button-outline button-small"
                onClick={() =>
                  void act(
                    question.id,
                    (token) => approveResearch(partnerId, question.id, token),
                    'Не удалось поставить исследование в очередь.',
                  )
                }
                disabled={busy === question.id || !ready}
                title={ready ? undefined : 'Исследование недоступно, пока не настроен поиск'}
              >
                {busy === question.id ? 'Ставим в очередь…' : 'Исследовать'}
              </button>
              {actionErrors[question.id] ? (
                <p className="field-error" role="alert">
                  {actionErrors[question.id]}
                </p>
              ) : null}
            </li>
          ))}
        </ul>
      )}

      <h3 className="knowledge-subheading">Исследования</h3>
      {overview.plans.length === 0 ? (
        <p className="knowledge-note">Ни одно исследование ещё не запускалось.</p>
      ) : (
        <ul className="plan-list" aria-label="Исследования">
          {overview.plans.map((plan) => {
            const presentation = planStatusPresentation(plan.status)
            const active = plan.status === 'queued' || plan.status === 'running'
            const journalOpen = openJournal === plan.id
            return (
              <li key={plan.id} className="plan" data-state={plan.status}>
                <div className="plan__head">
                  <strong className="plan__question">{plan.question_text}</strong>
                  <span className="plan__status" data-tone={presentation.tone}>
                    {presentation.label}
                  </span>
                </div>

                <p className="plan__hint">{plan.diagnostic || presentation.defaultHint}</p>
                <p className="plan__counts">{planCounts(plan, currency)}</p>

                {plan.rejections.length > 0 ? (
                  <details className="run__rejections">
                    <summary>Что не было прочитано и почему</summary>
                    <ul>
                      {plan.rejections.map((reason) => (
                        <li key={reason}>{reason}</li>
                      ))}
                    </ul>
                  </details>
                ) : null}

                <div className="plan__actions">
                  <button
                    type="button"
                    className="button button-ghost button-small"
                    onClick={() => setOpenJournal(journalOpen ? null : plan.id)}
                    aria-expanded={journalOpen}
                  >
                    {journalOpen ? 'Скрыть журнал источников' : 'Журнал источников'}
                  </button>

                  {active ? (
                    <button
                      type="button"
                      className="button button-outline button-small"
                      onClick={() =>
                        void act(
                          plan.id,
                          (token) => stopResearch(partnerId, plan.id, token),
                          'Не удалось остановить исследование.',
                        )
                      }
                      disabled={busy === plan.id || plan.cancel_requested}
                    >
                      {plan.cancel_requested ? 'Останавливается…' : 'Остановить'}
                    </button>
                  ) : (
                    <button
                      type="button"
                      className="button button-outline button-small"
                      onClick={() =>
                        plan.question_id
                          ? void act(
                              plan.id,
                              (token) =>
                                approveResearch(partnerId, plan.question_id as string, token),
                              'Не удалось запустить исследование заново.',
                            )
                          : undefined
                      }
                      disabled={
                        busy === plan.id ||
                        !ready ||
                        plan.question_id === null ||
                        plan.passes >= plan.max_passes
                      }
                      title={
                        plan.question_id === null
                          ? 'Исходный вопрос заменён новым разбором материала'
                          : plan.passes >= plan.max_passes
                            ? 'Предел проходов исследования исчерпан'
                            : undefined
                      }
                    >
                      {busy === plan.id ? 'Ставим в очередь…' : 'Исследовать заново'}
                    </button>
                  )}
                </div>

                {actionErrors[plan.id] ? (
                  <p className="field-error" role="alert">
                    {actionErrors[plan.id]}
                  </p>
                ) : null}

                {journalOpen ? (
                  <SourceJournal partnerId={partnerId} planId={plan.id} currency={currency} />
                ) : null}
              </li>
            )
          })}
        </ul>
      )}

      <h3 className="knowledge-subheading">Отраслевые выводы</h3>
      <FindingList findings={findings} />
    </section>
  )
}
