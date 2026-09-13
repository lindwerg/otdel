import { describe, expect, it } from 'vitest'
import type { ExtractionSummary, KnowledgeFact } from '../../api/types'
import {
  evidenceSourceLine,
  extractionCountsLine,
  extractionToolsLine,
  factKindLabel,
  factValueLine,
  formatBytes,
  formatMicros,
  materialStatusPresentation,
  pageStatusPresentation,
  pagesNeedingAttention,
  planStatusPresentation,
  queryOutcomeLabel,
  questionAudienceLabel,
  RETRYABLE_MATERIAL_STATUSES,
  RETRYABLE_PAGE_STATUSES,
  runStatusPresentation,
  sourceStatusPresentation,
} from '../format'

describe('formatBytes', () => {
  it('renders bytes below 1024 as Б', () => {
    expect(formatBytes(512)).toBe('512 Б')
  })

  it('renders KiB with one decimal below 10', () => {
    expect(formatBytes(1536)).toBe('1.5 КиБ')
  })

  it('renders whole MiB without decimals at 10 or above', () => {
    expect(formatBytes(25 * 1024 * 1024)).toBe('25 МиБ')
  })
})

describe('materialStatusPresentation', () => {
  it('labels "queued" as waiting to be processed, not as already read', () => {
    const presentation = materialStatusPresentation('queued')
    expect(presentation.label).toBe('В очереди')
    expect(presentation.defaultHint).toMatch(/ожидает обработки/i)
  })

  it('gives "partial" a warn tone, distinct from full success', () => {
    expect(materialStatusPresentation('partial').tone).toBe('warn')
    expect(materialStatusPresentation('completed').tone).toBe('success')
  })
})

// --- Phase 1B ---------------------------------------------------------------

describe('pageStatusPresentation', () => {
  it('does not present "needs_ocr" as a finished or empty page', () => {
    const presentation = pageStatusPresentation('needs_ocr')
    expect(presentation.label).toBe('Нужно распознавание')
    expect(presentation.tone).toBe('warn')
    expect(presentation.defaultHint).toMatch(/распознавание не выполнено/i)
    expect(presentation.label).not.toMatch(/готов|прочитан|пуст/i)
  })

  it('reserves "Пустая" for a page that genuinely holds nothing', () => {
    expect(pageStatusPresentation('empty').label).toBe('Пустая')
    expect(pageStatusPresentation('empty').defaultHint).toMatch(/нет ни текста/i)
  })

  it('keeps an unknown status readable instead of blank', () => {
    const presentation = pageStatusPresentation('какой-то-новый' as never)
    expect(presentation.label).toBe('какой-то-новый')
  })
})

describe('retryable statuses match what the server accepts', () => {
  it('does not offer a material retry for a quarantined file', () => {
    // The backend refuses it, so a button here could only ever fail.
    expect(RETRYABLE_MATERIAL_STATUSES).not.toContain('quarantined')
    expect(RETRYABLE_MATERIAL_STATUSES).toEqual(['failed', 'partial'])
  })

  it('offers a page retry exactly for the unsettled outcomes', () => {
    expect(RETRYABLE_PAGE_STATUSES).toEqual(['pending', 'needs_ocr', 'partial', 'failed'])
    expect(RETRYABLE_PAGE_STATUSES).not.toContain('extracted')
    expect(RETRYABLE_PAGE_STATUSES).not.toContain('empty')
  })
})

describe('extractionCountsLine', () => {
  const summary = (overrides: Partial<ExtractionSummary>): ExtractionSummary => ({
    pages_total: 0,
    pages_extracted: 0,
    pages_empty: 0,
    pages_needs_ocr: 0,
    pages_partial: 0,
    pages_failed: 0,
    pages_pending: 0,
    parser_name: null,
    parser_version: null,
    ocr_engine: null,
    ocr_version: null,
    started_at: null,
    finished_at: null,
    diagnostic: null,
    ...overrides,
  })

  it('reports counts and never a percentage or an estimate', () => {
    const line = extractionCountsLine(
      summary({ pages_total: 32, pages_extracted: 30, pages_needs_ocr: 2 }),
    )
    expect(line).toContain('Страниц: 32')
    expect(line).toContain('30 прочитано')
    expect(line).toContain('2 ждут распознавания')
    expect(line).not.toMatch(/%|осталось|минут/i)
  })

  it('says plainly when nothing has been read yet', () => {
    expect(extractionCountsLine(summary({ pages_total: 12, pages_pending: 0 }))).toMatch(
      /Ни одна ещё не прочитана/,
    )
  })

  it('names only the tools that actually ran', () => {
    const withParser = summary({ parser_name: 'pdf-extract', parser_version: '0.12' })
    expect(extractionToolsLine(withParser)).toBe('разбор: pdf-extract 0.12')
    // No engine recorded → nothing claimed about recognition.
    expect(extractionToolsLine(withParser)).not.toMatch(/распознавание/)
    expect(extractionToolsLine(summary({}))).toBeNull()
  })

  it('counts the pages whose outcome the owner may still change', () => {
    expect(
      pagesNeedingAttention(
        summary({ pages_needs_ocr: 2, pages_failed: 1, pages_partial: 1, pages_extracted: 28 }),
      ),
    ).toBe(4)
  })
})

describe('knowledge labels (phase 1C)', () => {
  function fact(overrides: Partial<KnowledgeFact>): KnowledgeFact {
    return {
      id: 'fact-1',
      partner_id: 'p',
      material_id: 'm',
      run_id: 'r',
      product_id: null,
      product_name: null,
      kind: 'characteristic',
      status: 'candidate',
      attribute: 'нагрузка',
      value_text: '3.5',
      unit: null,
      conditions: null,
      model_context: null,
      evidence: [],
      created_at: '2026-01-01T10:00:00Z',
      ...overrides,
    }
  }

  it('appends a unit only when the server recorded one', () => {
    expect(factValueLine(fact({ unit: 'kN' }))).toBe('3.5 kN')
    // The server stores a unit only when the source writes it; a bare value
    // must stay bare rather than acquire a plausible unit here.
    expect(factValueLine(fact({ unit: null }))).toBe('3.5')
    // A range or a designation is passed through exactly as stored.
    expect(factValueLine(fact({ value_text: '40…60', unit: null }))).toBe('40…60')
    expect(factValueLine(fact({ value_text: '– / 2074 / 2345', unit: null }))).toBe(
      '– / 2074 / 2345',
    )
  })

  it('does not call a missing key a failure', () => {
    const waiting = runStatusPresentation('needs_provider')
    expect(waiting.label).toBe('Ожидает настройки модели')
    expect(waiting.tone).toBe('warn')
    expect(waiting.label).not.toMatch(/ошибка/i)

    expect(runStatusPresentation('failed').tone).toBe('error')
    expect(runStatusPresentation('completed').tone).toBe('success')
    expect(runStatusPresentation('partial').label).toBe('Разобран частично')
  })

  it('names the kind of statement and the addressee of a question', () => {
    expect(factKindLabel('commercial')).toBe('коммерческое условие')
    expect(factKindLabel('limitation')).toBe('ограничение')
    expect(questionAudienceLabel('partner')).toBe('вопрос партнёру')
    expect(questionAudienceLabel('industry')).toContain('отраслевого исследования')
  })

  it('states a source as file and page', () => {
    expect(evidenceSourceLine('каталог.pdf', 7)).toBe('каталог.pdf, стр. 7')
  })
})

describe('research (phase 1D)', () => {
  it('renders money the way a person reads it, from integers', () => {
    expect(formatMicros(5_000, 'USD')).toBe('0,005 USD')
    expect(formatMicros(1_000_000, 'USD')).toBe('1 USD')
    expect(formatMicros(1_500_000, 'RUB')).toBe('1,5 RUB')
    expect(formatMicros(0, 'USD')).toBe('0 USD')
    expect(formatMicros(1, 'USD')).toBe('0,000001 USD')
    expect(formatMicros(12_345_678, 'EUR')).toBe('12,345678 EUR')
    // An over-spend reads as a negative amount rather than as a silent zero.
    expect(formatMicros(-5_000, 'USD')).toBe('-0,005 USD')
    // …and a value that is not a number does not become one.
    expect(formatMicros(Number.NaN, 'USD')).toBe('— USD')
  })

  it('does not call an unconfigured, stopped or budget-bound plan a failure', () => {
    const waiting = planStatusPresentation('needs_provider')
    expect(waiting.tone).toBe('warn')
    expect(waiting.label).not.toMatch(/ошибка/i)

    const exhausted = planStatusPresentation('budget_exhausted')
    expect(exhausted.label).toBe('Остановлено по бюджету')
    expect(exhausted.tone).toBe('warn')
    expect(exhausted.label).not.toMatch(/ошибка/i)
    // A budget doing its job is not a malfunction, and the hint says so.
    expect(exhausted.defaultHint).toMatch(/остановлена, а не продолжена/)

    const cancelled = planStatusPresentation('cancelled')
    expect(cancelled.tone).toBe('neutral')
    expect(cancelled.label).not.toMatch(/ошибка/i)

    // A real failure still reads as one.
    expect(planStatusPresentation('failed').tone).toBe('error')
    expect(planStatusPresentation('completed').tone).toBe('success')
  })

  it('gives every unread source its own reason', () => {
    const reasons = (
      ['skipped_host', 'skipped_robots', 'skipped_limit', 'skipped_type', 'failed'] as const
    ).map((status) => sourceStatusPresentation(status).label)

    // Five different outcomes, five different labels: a journal that said only
    // "не прочитано" would explain nothing.
    expect(new Set(reasons).size).toBe(reasons.length)
    expect(sourceStatusPresentation('skipped_host').label).toBe('Хост не разрешён')
    expect(sourceStatusPresentation('skipped_robots').label).toMatch(/robots/i)
    expect(sourceStatusPresentation('fetched').tone).toBe('success')
    // Only a read page is presented as read.
    expect(sourceStatusPresentation('discovered').label).toBe('Найдено, не читалось')
  })

  it('says plainly that an unknown outcome still cost money', () => {
    expect(queryOutcomeLabel('unknown')).toMatch(/сверка расхода/)
    expect(queryOutcomeLabel('refused')).toBe('не отправлялся')
    expect(queryOutcomeLabel('ok')).toBe('выполнен')
  })
})
