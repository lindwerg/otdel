import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type {
  ExtractionSummary,
  Material,
  MaterialPage,
  PageDetail,
  PageRegion,
} from '../../api/types'
import { AuthProvider } from '../../auth/AuthContext'
import { installMockFetch, jsonResponse } from '../../test/mockFetch'
import { MaterialPages } from '../MaterialPages'

const PARTNER = 'partner-a'
const MATERIAL = 'material-1'

function summary(overrides: Partial<ExtractionSummary> = {}): ExtractionSummary {
  return {
    pages_total: 3,
    pages_extracted: 2,
    pages_empty: 0,
    pages_needs_ocr: 1,
    pages_partial: 0,
    pages_failed: 0,
    pages_pending: 0,
    parser_name: 'pdf-extract',
    parser_version: '0.12',
    ocr_engine: null,
    ocr_version: null,
    started_at: '2026-01-01T10:00:00Z',
    finished_at: '2026-01-01T10:00:04Z',
    diagnostic: null,
    ...overrides,
  }
}

function material(overrides: Partial<Material> = {}): Material {
  return {
    id: MATERIAL,
    partner_id: PARTNER,
    filename: 'catalogue.pdf',
    media_type: 'application/pdf',
    size_bytes: 2048,
    sha256: 'a'.repeat(64),
    status: 'partial',
    page_count: 3,
    created_at: '2026-01-01T09:00:00Z',
    error: null,
    extraction: summary(),
    ...overrides,
  }
}

function page(overrides: Partial<MaterialPage>): MaterialPage {
  return {
    id: `page-${overrides.page_number ?? 1}`,
    material_id: MATERIAL,
    page_number: 1,
    status: 'extracted',
    text_source: 'text_layer',
    char_count: 420,
    word_count: 70,
    image_count: 0,
    width_pt: 595,
    height_pt: 842,
    rotation: 0,
    parser_name: 'pdf-extract',
    parser_version: '0.12',
    ocr_engine: null,
    ocr_version: null,
    ocr_language: null,
    duration_ms: 8,
    attempts: 1,
    diagnostic: null,
    extracted_at: '2026-01-01T10:00:01Z',
    region_count: 2,
    table_count: 0,
    ...overrides,
  }
}

const PAGES: MaterialPage[] = [
  page({ page_number: 1 }),
  page({
    page_number: 2,
    status: 'needs_ocr',
    text_source: 'none',
    char_count: 0,
    image_count: 1,
    region_count: 0,
    diagnostic:
      'страница без текстового слоя, содержит изображений: 1; распознавание недоступно: исполняемый файл `tesseract` не найден',
  }),
  page({ page_number: 3, table_count: 1, region_count: 3 }),
]

function tableRegion(): PageRegion {
  return {
    id: 'region-table',
    page_id: 'page-3',
    page_number: 3,
    ordinal: 0,
    kind: 'table',
    text: 'Profile Length, mm',
    source: 'text_layer',
    bbox: { x0: 50, y0: 620, x1: 460, y1: 710 },
    row_count: 3,
    column_count: 3,
    cells: [
      cell(0, 0, 'Profile', 'text', true),
      cell(0, 1, 'Length, mm', 'text', true),
      cell(0, 2, 'Load (kN)', 'text', true),
      cell(1, 0, 'BP21', 'text'),
      cell(1, 1, '1200', 'number', false, 'mm', 'Length, mm'),
      cell(1, 2, '3.5', 'number', false, 'kN', 'Load (kN)'),
      cell(2, 0, 'BP40', 'text'),
      cell(2, 1, '2000', 'number', false, 'mm', 'Length, mm'),
      // The source does not print this load.
      cell(2, 2, '', 'empty'),
    ],
  }
}

function cell(
  row: number,
  column: number,
  raw: string,
  kind: 'text' | 'number' | 'empty',
  header = false,
  unit: string | null = null,
  columnHeader: string | null = null,
) {
  return {
    id: `cell-${row}-${column}`,
    region_id: 'region-table',
    row_index: row,
    column_index: column,
    is_header: header,
    raw_text: raw,
    value_kind: kind,
    unit,
    column_header: columnHeader,
    bbox: null,
  }
}

function detail(pageNumber: number): PageDetail {
  if (pageNumber === 2) {
    return { page: PAGES[1], text: null, regions: [] }
  }
  return {
    page: PAGES[2],
    text: 'Profile load table\nProfile Length, mm Load (kN)',
    regions: [tableRegion()],
  }
}

function install(handler?: (url: string, method: string) => Response) {
  return installMockFetch((req) => {
    if (req.url.endsWith('/api/session')) {
      return jsonResponse(200, { authenticated: true, csrf_token: 'csrf-1' })
    }
    const custom = handler?.(req.url, req.method)
    if (custom) return custom
    if (req.url.includes('/pages/')) {
      const pageNumber = Number(req.url.split('/pages/')[1].split('/')[0])
      return jsonResponse(200, detail(pageNumber))
    }
    if (req.url.endsWith('/pages')) {
      return jsonResponse(200, { items: PAGES })
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'nope', retryable: false } })
  })
}

function renderPanel(overrides: Partial<Material> = {}) {
  return render(
    <AuthProvider>
      <MaterialPages material={material(overrides)} onPageChanged={() => {}} />
    </AuthProvider>,
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('MaterialPages', () => {
  it('shows every page with its own outcome and no invented progress', async () => {
    install()
    renderPanel()

    await waitFor(() => expect(screen.getAllByRole('listitem')).toHaveLength(3))

    const items = screen.getAllByRole('listitem')
    expect(within(items[0]).getByText('Прочитана')).toBeTruthy()

    // The scanned page is neither "read" nor "empty".
    const scanned = items[1]
    expect(within(scanned).getByText('Нужно распознавание')).toBeTruthy()
    expect(within(scanned).queryByText('Пустая')).toBeNull()
    expect(within(scanned).queryByText('Прочитана')).toBeNull()

    // Nothing anywhere claims a percentage or a time estimate.
    const text = document.body.textContent ?? ''
    expect(text).not.toMatch(/%/)
    expect(text).not.toMatch(/осталось|примерно|минут/i)
  })

  it('repeats the server’s own reason for a page that needs recognition', async () => {
    install()
    renderPanel()

    await waitFor(() =>
      expect(screen.getByText(/tesseract` не найден/)).toBeTruthy(),
    )
  })

  it('links each page to that page of the original document', async () => {
    install()
    renderPanel()

    const link = await screen.findByRole('link', { name: /Открыть оригинал, стр\. 2/ })
    expect(link.getAttribute('href')).toBe(
      `/api/partners/${PARTNER}/materials/${MATERIAL}/original#page=2`,
    )
  })

  it('offers a retry only for pages the server would accept', async () => {
    install()
    renderPanel()

    await waitFor(() => expect(screen.getAllByRole('listitem')).toHaveLength(3))
    const items = screen.getAllByRole('listitem')

    expect(within(items[0]).queryByRole('button', { name: /Перечитать страницу/ })).toBeNull()
    expect(within(items[1]).getByRole('button', { name: /Перечитать страницу/ })).toBeTruthy()
  })

  it('sends a page retry and shows the page waiting to be read again', async () => {
    const user = userEvent.setup()
    const { calls } = install((url, method) => {
      if (method === 'POST' && url.endsWith('/pages/2/retry')) {
        return jsonResponse(200, { ...PAGES[1], status: 'pending', diagnostic: null, attempts: 1 })
      }
      return undefined as unknown as Response
    })
    renderPanel()

    await waitFor(() => expect(screen.getAllByRole('listitem')).toHaveLength(3))
    await user.click(screen.getByRole('button', { name: /Перечитать страницу/ }))

    await waitFor(() => expect(screen.getByText('Ожидает чтения')).toBeTruthy())
    const retry = calls.find((call) => call.url.endsWith('/pages/2/retry'))
    expect(retry?.method).toBe('POST')
    expect(retry?.headers['x-csrf-token']).toBe('csrf-1')
  })

  it('renders a table with its units and leaves an unprinted value blank', async () => {
    const user = userEvent.setup()
    install()
    renderPanel()

    await waitFor(() => expect(screen.getAllByRole('listitem')).toHaveLength(3))
    const third = screen.getAllByRole('listitem')[2]
    await user.click(within(third).getByRole('button', { name: 'Что извлечено' }))

    const table = await screen.findByRole('table')
    expect(within(table).getByRole('columnheader', { name: 'Length, mm' })).toBeTruthy()

    const rows = within(table).getAllByRole('row')
    // Header row plus two data rows.
    expect(rows).toHaveLength(3)

    const firstData = within(rows[1]).getAllByRole('cell')
    expect(firstData[1].textContent).toContain('1200')
    expect(firstData[1].textContent).toContain('mm')

    // The load the catalogue does not print stays blank — not 0, not a dash.
    const secondData = within(rows[2]).getAllByRole('cell')
    expect(secondData[2].textContent?.trim()).toBe('')
    expect(secondData[2].textContent).not.toContain('0')
  })

  it('says plainly that no text was stored for an unrecognised page', async () => {
    const user = userEvent.setup()
    install()
    renderPanel()

    await waitFor(() => expect(screen.getAllByRole('listitem')).toHaveLength(3))
    const second = screen.getAllByRole('listitem')[1]
    await user.click(within(second).getByRole('button', { name: 'Что извлечено' }))

    expect(await screen.findByText(/не выдумывает его/)).toBeTruthy()
  })

  it('names the parser that produced the result and claims no engine that never ran', async () => {
    install()
    renderPanel()

    await waitFor(() => expect(screen.getByText(/разбор: pdf-extract 0\.12/)).toBeTruthy())
    expect(document.body.textContent).not.toMatch(/распознавание: /)
  })
})
