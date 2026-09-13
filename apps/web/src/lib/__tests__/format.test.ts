import { describe, expect, it } from 'vitest'
import type { ExtractionSummary } from '../../api/types'
import {
  extractionCountsLine,
  extractionToolsLine,
  formatBytes,
  materialStatusPresentation,
  pageStatusPresentation,
  pagesNeedingAttention,
  RETRYABLE_MATERIAL_STATUSES,
  RETRYABLE_PAGE_STATUSES,
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
