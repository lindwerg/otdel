import { useEffect, useRef, useState } from 'react'
import { getPage, getPageView, originalMaterialUrl, retryPage } from '../api/client'
import type { Material, MaterialPage, PageDetail, PageView } from '../api/types'
import { useAuth } from '../auth/AuthContext'
import {
  diagramInterpretationNote,
  extractionCountsLine,
  extractionToolsLine,
  pageStatusPresentation,
  RETRYABLE_PAGE_STATUSES,
  textSourceLabel,
} from '../lib/format'
import { usePages } from '../hooks/usePages'
import { PageRegions } from './PageRegions'
import { PageSourceMap } from './PageSourceMap'
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
  if (page.drawing_count > 0) facts.push(`рисунков: ${page.drawing_count}`)
  if (page.table_count > 0) facts.push(`таблиц: ${page.table_count}`)
  if (page.ocr_engine) facts.push(`движок: ${page.ocr_engine}`)
  // Which reading produced this page, next to the tools that produced it: two
  // pages of the same document may come from different readings, and a fact
  // without its revision cannot be told apart from a stale one.
  if (page.extraction_revision) facts.push(`ревизия чтения: ${page.extraction_revision}`)
  if (page.attempts > 1) facts.push(`попыток чтения: ${page.attempts}`)
  return facts
}

/**
 * What was done with a drawing on this page — which is nothing, and it says so.
 *
 * Silence here would be read as "the drawing was understood": a diagram is the
 * one thing on a technical page that a reader most expects to have been
 * interpreted, and nothing in this phase interprets one.
 */
function DiagramNote({ page }: { page: MaterialPage }) {
  const note = diagramInterpretationNote(page.diagram_interpretation, page.drawing_count)
  if (!note) return null
  return <StatusMessage tone="warn">{note}</StatusMessage>
}

export function MaterialPages({ material, onPageChanged }: MaterialPagesProps) {
  const { runMutation, runRead } = useAuth()
  const { pages, error, reload, upsert } = usePages(material.partner_id, material.id)
  const [openPage, setOpenPage] = useState<number | null>(null)
  const [detail, setDetail] = useState<PageDetail | null>(null)
  const [detailError, setDetailError] = useState<string | null>(null)
  const [pageView, setPageView] = useState<PageView | null>(null)
  const [viewError, setViewError] = useState<string | null>(null)
  const [highlightedRegionId, setHighlightedRegionId] = useState<string | null>(null)
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

  function closeDetail() {
    setOpenPage(null)
    setDetail(null)
    setPageView(null)
    setHighlightedRegionId(null)
  }

  async function toggleDetail(pageNumber: number) {
    if (openPage === pageNumber) {
      closeDetail()
      return
    }
    setOpenPage(pageNumber)
    setDetail(null)
    setDetailError(null)
    setPageView(null)
    setViewError(null)
    setHighlightedRegionId(null)

    // Two independent questions — what was read, and where it sits — asked at
    // once. `allSettled`, because a page map that cannot be built is not a
    // reason to withhold the text: each half reports its own outcome.
    const [detailResult, viewResult] = await Promise.allSettled([
      runRead(() => getPage(material.partner_id, material.id, pageNumber)),
      runRead(() => getPageView(material.partner_id, material.id, pageNumber)),
    ])

    if (detailResult.status === 'fulfilled') {
      setDetail(detailResult.value)
    } else {
      const err: unknown = detailResult.reason
      setDetailError(err instanceof Error ? err.message : 'Не удалось открыть страницу.')
    }

    if (viewResult.status === 'fulfilled') {
      setPageView(viewResult.value)
    } else {
      const err: unknown = viewResult.reason
      setViewError(
        err instanceof Error ? err.message : 'Не удалось получить схему областей страницы.',
      )
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
        closeDetail()
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
                        <DiagramNote page={detail.page} />

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

                        {viewError ? (
                          <p className="page-note">
                            Схему областей получить не удалось: {viewError} Ниже — то, что
                            известно о самих областях.
                          </p>
                        ) : null}
                        {pageView ? (
                          <PageSourceMap
                            view={pageView}
                            highlightedRegionId={highlightedRegionId}
                          />
                        ) : null}

                        <PageRegions
                          regions={detail.regions}
                          highlightedRegionId={highlightedRegionId}
                          onHighlight={
                            pageView
                              ? (regionId) =>
                                  setHighlightedRegionId((current) =>
                                    current === regionId ? null : regionId,
                                  )
                              : undefined
                          }
                        />
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
