import { useState } from 'react'
import { understandMaterial } from '../../api/client'
import type { KnowledgeRun, KnowledgeSummary } from '../../api/types'
import { useAuth } from '../../auth/AuthContext'
import { useKnowledge } from '../../hooks/useKnowledge'
import { runStatusPresentation } from '../../lib/format'
import { StatusMessage } from '../StatusMessage'
import { GapsList } from './GapsList'
import { GlossaryList, QaList } from './GlossaryList'
import { ProductFacts } from './ProductFacts'
import { ProviderNotice } from './ProviderNotice'

interface KnowledgePanelProps {
  partnerId: string
}

/** Counts, never a percentage: "разобрано на 70%" would be an estimate of
 *  understanding that nothing here measures. */
function summaryLine(summary: KnowledgeSummary): string {
  return (
    `Продуктов: ${summary.products_total} · характеристик: ${summary.facts_total} · ` +
    `терминов: ${summary.terms_total} · вопросов и ответов: ${summary.qa_total} · ` +
    `пробелов: ${summary.gaps_total}`
  )
}

/** What one run did, in the numbers it really recorded. */
function runCounts(run: KnowledgeRun): string {
  // What exists right now, from the stored rows. These survive a re-queue, which is
  // why they are listed even while the next run is pending.
  const parts = [
    `продуктов: ${run.products_created}`,
    `характеристик: ${run.facts_accepted}`,
    `терминов: ${run.terms_created}`,
    `пробелов: ${run.gaps_created}`,
  ]
  // Per-attempt numbers are reset when a run is queued again; showing a zero next to
  // an existing draft would read as "nothing was found". They appear once the run
  // that produced them has finished.
  const settled = !['queued', 'running'].includes(run.status)
  if (settled) {
    parts.push(`страниц с текстом разобрано: ${run.pages_considered}`)
    if (run.facts_rejected > 0) parts.push(`отклонено: ${run.facts_rejected}`)
    if (run.requests_made > 0) parts.push(`запросов к модели: ${run.requests_made}`)
  }
  return parts.join(' · ')
}

export function KnowledgePanel({ partnerId }: KnowledgePanelProps) {
  const { runMutation } = useAuth()
  const { data, error, reload } = useKnowledge(partnerId)
  const [busyMaterial, setBusyMaterial] = useState<string | null>(null)
  const [actionErrors, setActionErrors] = useState<Record<string, string>>({})

  async function requestDraft(materialId: string) {
    setBusyMaterial(materialId)
    setActionErrors((prev) => {
      const next = { ...prev }
      delete next[materialId]
      return next
    })
    try {
      await runMutation((token) => understandMaterial(partnerId, materialId, token))
      reload()
    } catch (err) {
      // The server's own reason — "нет ключа", "материал ещё не прочитан" —
      // is more useful than anything this component could invent.
      const message = err instanceof Error ? err.message : 'Не удалось поставить разбор в очередь.'
      setActionErrors((prev) => ({ ...prev, [materialId]: message }))
    } finally {
      setBusyMaterial(null)
    }
  }

  // A failed refresh does not take the draft off the screen: the knowledge shown is
  // still what the server last said, and hiding it behind a banner would lose the
  // owner's place over a two-second network blip. Only a first load with nothing to
  // show replaces the panel.
  if (!data) {
    return (
      <section aria-labelledby="knowledge-heading">
        <div className="section-heading">
          <h2 id="knowledge-heading">Знания</h2>
        </div>
        {error ? (
          <StatusMessage tone="error" onRetry={reload}>
            {error}
          </StatusMessage>
        ) : (
          <StatusMessage>Загружаем знания…</StatusMessage>
        )}
      </section>
    )
  }

  const { overview, products, glossary, qa, gaps } = data
  const providerReady = overview.provider.state === 'ready'

  return (
    <section aria-labelledby="knowledge-heading" className="knowledge">
      <div className="section-heading">
        <h2 id="knowledge-heading">Знания</h2>
      </div>

      {error ? (
        <StatusMessage tone="error" onRetry={reload}>
          {error}
        </StatusMessage>
      ) : null}

      <ProviderNotice provider={overview.provider} />

      <p className="knowledge-summary">{summaryLine(overview.summary)}</p>
      <p className="knowledge-note">
        Это черновик: каждая характеристика подтверждена дословной цитатой из материала
        партнёра. Подтверждение цитатой — не независимая проверка производителя, и ничего
        из этого ещё не опубликовано.
      </p>

      {/* Materials and their runs: the state of the draft per source document. */}
      <h3 className="knowledge-subheading">Разбор материалов</h3>

      {overview.runs.length === 0 && overview.pending_materials.length === 0 ? (
        <p className="knowledge-note">
          Прочитанных материалов пока нет: сначала загрузите и прочитайте документ.
        </p>
      ) : null}

      {/* Read materials nobody has drafted yet — including everything read before
          this phase existed. Without this list their only entry point would be a
          "разобрать заново" button that does not exist yet. */}
      {overview.pending_materials.length > 0 ? (
        <ul className="run-list" aria-label="Материалы без разбора">
          {overview.pending_materials.map((material) => (
            <li key={material.material_id} className="run" data-state="new">
              <div className="run__head">
                <strong className="run__material">{material.filename}</strong>
                <span className="run__status">разбор не запускался</span>
              </div>
              <p className="run__counts">страниц с текстом: {material.pages_with_text}</p>
              <button
                type="button"
                className="button button-outline button-small"
                onClick={() => void requestDraft(material.material_id)}
                disabled={busyMaterial === material.material_id || !providerReady}
                title={
                  providerReady
                    ? undefined
                    : 'Разбор недоступен, пока не настроен провайдер модели'
                }
              >
                {busyMaterial === material.material_id ? 'Ставим в очередь…' : 'Разобрать'}
              </button>
              {actionErrors[material.material_id] ? (
                <p className="field-error" role="alert">
                  {actionErrors[material.material_id]}
                </p>
              ) : null}
            </li>
          ))}
        </ul>
      ) : null}

      {overview.runs.length === 0 ? null : (
        <ul className="run-list" aria-label="Разбор материалов">
          {overview.runs.map((run) => {
            const presentation = runStatusPresentation(run.status)
            const canRepeat = !['queued', 'running'].includes(run.status)
            return (
              <li key={run.id} className="run" data-state={run.status}>
                <div className="run__head">
                  <strong className="run__material">{run.material_filename}</strong>
                  <span className="run__status" data-tone={presentation.tone}>
                    {presentation.label}
                  </span>
                </div>
                <p className="run__hint">{run.diagnostic || presentation.defaultHint}</p>
                <p className="run__counts">{runCounts(run)}</p>

                {run.rejections.length > 0 ? (
                  <details className="run__rejections">
                    <summary>Почему предложения модели отклонены</summary>
                    <ul>
                      {run.rejections.map((reason) => (
                        <li key={reason}>{reason}</li>
                      ))}
                    </ul>
                  </details>
                ) : null}

                {canRepeat ? (
                  <button
                    type="button"
                    className="button button-outline button-small"
                    onClick={() => void requestDraft(run.material_id)}
                    disabled={busyMaterial === run.material_id || !providerReady}
                    title={
                      providerReady
                        ? undefined
                        : 'Разбор недоступен, пока не настроен провайдер модели'
                    }
                  >
                    {busyMaterial === run.material_id ? 'Ставим в очередь…' : 'Разобрать заново'}
                  </button>
                ) : null}

                {actionErrors[run.material_id] ? (
                  <p className="field-error" role="alert">
                    {actionErrors[run.material_id]}
                  </p>
                ) : null}
              </li>
            )
          })}
        </ul>
      )}

      <h3 className="knowledge-subheading">Продукты и характеристики</h3>
      <ProductFacts partnerId={partnerId} nodes={products} />

      <h3 className="knowledge-subheading">Глоссарий</h3>
      <GlossaryList partnerId={partnerId} terms={glossary} />

      <h3 className="knowledge-subheading">Вопросы и ответы по материалам</h3>
      <QaList partnerId={partnerId} entries={qa} />

      <h3 className="knowledge-subheading">Пробелы и подготовленные вопросы</h3>
      <GapsList gaps={gaps} />
    </section>
  )
}
