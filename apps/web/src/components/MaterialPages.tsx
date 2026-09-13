import { useEffect, useRef, useState } from 'react'
import { getPage, originalMaterialUrl, retryPage } from '../api/client'
import type { Material, MaterialPage, PageDetail } from '../api/types'
import { useAuth } from '../auth/AuthContext'
import {
  extractionCountsLine,
  extractionToolsLine,
  pageStatusPresentation,
  RETRYABLE_PAGE_STATUSES,
  textSourceLabel,
} from '../lib/format'
import { usePages } from '../hooks/usePages'
import { PageRegions } from './PageRegions'
import { StatusMessage } from './StatusMessage'

interface MaterialPagesProps {
  material: Material
  onPageChanged: () => void
}

/** Facts about a page, each one recorded by the server. No estimates. */
function pageFacts(page: MaterialPage): string[] {
  const facts: string[] = [textSourceLabel(page.text_source)]
  if (page.char_count > 0) facts.push(`${page.char_count} симв.`)
  if (page.image_count > 0) facts.push(`изображений: ${page.image_count}`)
  if (page.table_count > 0) facts.push(`таблиц: ${page.table_count}`)
  if (page.ocr_engine) facts.push(`движок: ${page.ocr_engine}`)
  if (page.attempts > 1) facts.push(`попыток чтения: ${page.attempts}`)
  return facts
}

export function MaterialPages({ material, onPageChanged }: MaterialPagesProps) {
  const { runMutation, runRead } = useAuth()
  const { pages, error, reload, upsert } = usePages(material.partner_id, material.id)
  const [openPage, setOpenPage] = useState<number | null>(null)
  const [detail, setDetail] = useState<PageDetail | null>(null)
  const [detailError, setDetailError] = useState<string | null>(null)
  const [busyPage, setBusyPage] = useState<number | null>(null)
  const [pageErrors, setPageErrors] = useState<Record<number, string>>({})

  // A single-page re-read changes the material's roll-up, but the material list
  // has no reason to poll for it (the material itself stays `partial`). When the
  // last pending page settles, ask the parent to refresh the summary once —
  // rather than leaving stale counters next to fresh page rows.
  const hadPending = useRef(false)
  useEffect(() => {
    const pending = (pages ?? []).some((page) => page.status === 'pending')
    if (hadPending.current && !pending) onPageChanged()
    hadPending.current = pending
  }, [pages, onPageChanged])

  async function toggleDetail(pageNumber: number) {
    if (openPage === pageNumber) {
      setOpenPage(null)
      setDetail(null)
      return
    }
    setOpenPage(pageNumber)
    setDetail(null)
    setDetailError(null)
    try {
      setDetail(await runRead(() => getPage(material.partner_id, material.id, pageNumber)))
    } catch (err) {
      setDetailError(err instanceof Error ? err.message : 'Не удалось открыть страницу.')
    }
  }

  async function handleRetry(page: MaterialPage) {
    setBusyPage(page.page_number)
    setPageErrors((prev) => {
      const next = { ...prev }
      delete next[page.page_number]
      return next
    })
    try {
      const updated = await runMutation((token) =>
        retryPage(material.partner_id, material.id, page.page_number, token),
      )
      upsert(updated)
      if (openPage === page.page_number) {
        setOpenPage(null)
        setDetail(null)
      }
      onPageChanged()
    } catch (err) {
      const message = err instanceof Error ? err.message : 'Не удалось перечитать страницу.'
      setPageErrors((prev) => ({ ...prev, [page.page_number]: message }))
    } finally {
      setBusyPage(null)
    }
  }

  const summary = material.extraction

  return (
    <section className="page-panel" aria-label={`Страницы материала ${material.filename}`}>
      {summary ? (
        <p className="page-summary">
          {extractionCountsLine(summary)}
          {extractionToolsLine(summary) ? (
            <span className="page-summary__tools"> {extractionToolsLine(summary)}</span>
          ) : null}
        </p>
      ) : null}

      {summary?.diagnostic ? (
        <StatusMessage tone="warn">{summary.diagnostic}</StatusMessage>
      ) : null}

      {pages === null && !error ? <StatusMessage>Загружаем страницы…</StatusMessage> : null}

      {error ? (
        <StatusMessage tone="error" onRetry={reload}>
          {error}
        </StatusMessage>
      ) : null}

      {pages && pages.length === 0 ? (
        <p className="page-note">
          Страницы ещё не учтены. Обработчик запишет их, как только возьмёт материал в работу.
        </p>
      ) : null}

      {pages && pages.length > 0 ? (
        <ol className="page-list">
          {pages.map((page) => {
            const presentation = pageStatusPresentation(page.status)
            const canRetry = RETRYABLE_PAGE_STATUSES.includes(page.status)
            const isOpen = openPage === page.page_number
            return (
              <li key={page.id} className="page-item" data-state={page.status}>
                <div className="page-head">
                  <span className="page-number">{page.page_number}</span>
                  <span className="page-status" data-tone={presentation.tone}>
                    {presentation.label}
                  </span>
                  <span className="page-facts">{pageFacts(page).join(' · ')}</span>
                </div>

                {/* The server's own explanation, shown verbatim. */}
                <p className="page-diagnostic">{page.diagnostic || presentation.defaultHint}</p>

                <div className="page-actions">
                  <button
                    type="button"
                    className="button button-ghost button-small"
                    aria-expanded={isOpen}
                    onClick={() => void toggleDetail(page.page_number)}
                  >
                    {isOpen ? 'Свернуть' : 'Что извлечено'}
                  </button>
                  <a
                    className="button button-ghost button-small"
                    href={originalMaterialUrl(material.partner_id, material.id, page.page_number)}
                    target="_blank"
                    rel="noreferrer"
                  >
                    Открыть оригинал, стр. {page.page_number}
                  </a>
                  {canRetry ? (
                    <button
                      type="button"
                      className="button button-outline button-small"
                      onClick={() => void handleRetry(page)}
                      disabled={busyPage === page.page_number}
                    >
                      {busyPage === page.page_number ? 'Ставим в очередь…' : 'Перечитать страницу'}
                    </button>
                  ) : null}
                </div>

                {pageErrors[page.page_number] ? (
                  <p className="field-error" role="alert">
                    {pageErrors[page.page_number]}
                  </p>
                ) : null}

                {isOpen ? (
                  <div className="page-detail">
                    {detailError ? (
                      <StatusMessage tone="error">{detailError}</StatusMessage>
                    ) : null}
                    {!detail && !detailError ? (
                      <StatusMessage>Открываем страницу…</StatusMessage>
                    ) : null}
                    {detail ? (
                      <>
                        {detail.text ? (
                          <details className="page-text">
                            <summary>Текст страницы</summary>
                            <pre>{detail.text}</pre>
                          </details>
                        ) : (
                          <p className="page-note">
                            Текста для этой страницы не сохранено — система не выдумывает его.
                          </p>
                        )}
                        <PageRegions regions={detail.regions} />
                      </>
                    ) : null}
                  </div>
                ) : null}
              </li>
            )
          })}
        </ol>
      ) : null}
    </section>
  )
}
