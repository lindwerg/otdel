import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type {
  ExtractionSummary,
  Material,
  MaterialPage,
  PageDetail,
  PageView,
} from '../../api/types'
import { AuthProvider } from '../../auth/AuthContext'
import { exactSpan, loadTableRegion, unlocatedSpan } from '../../test/extraction'
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
    extraction_revision: 'r2',
    drawing_count: 0,
    diagram_interpretation: 'none',
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
    // A page that holds a drawing nobody interpreted — and says so.
    drawing_count: 2,
    diagram_interpretation: 'not_attempted',
    diagnostic:
      'страница без текстового слоя, содержит изображений: 1; распознавание недоступно: исполняемый файл `tesseract` не найден',
  }),
  page({ page_number: 3, table_count: 1, region_count: 3 }),
]

function detail(pageNumber: number): PageDetail {
  if (pageNumber === 2) {
    return { page: PAGES[1], text: null, regions: [] }
  }
  return {
    page: PAGES[2],
    text: 'Таблица нагрузок\nПрофиль Длина, мм безопасная рабочая нагрузка (Н)',
    regions: [loadTableRegion()],
  }
}

function pageView(pageNumber: number): PageView {
  if (pageNumber === 2) {
    return {
      page_number: 2,
      width_pt: 595,
      height_pt: 842,
      rotation: 0,
      original_url: `/api/partners/${PARTNER}/materials/${MATERIAL}/original#page=2`,
      diagram_interpretation: 'not_attempted',
      regions: [],
      unplaced: [],
    }
  }
  return {
    page_number: 3,
    width_pt: 595,
    height_pt: 842,
    rotation: 0,
    original_url: `/api/partners/${PARTNER}/materials/${MATERIAL}/original#page=3`,
    diagram_interpretation: 'none',
    regions: [
      {
        region_id: 'region-table',
        ordinal: 0,
        kind: 'table',
        span: exactSpan(3, { x0: 50, y0: 620, x1: 460, y1: 710 }),
      },
      {
        region_id: 'region-ocr-note',
        ordinal: 1,
        kind: 'footnote',
        span: unlocatedSpan(3, 'движок распознавания не возвращает координат слов'),
      },
    ],
    unplaced: [],
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
      if (req.url.endsWith('/view')) return jsonResponse(200, pageView(pageNumber))
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
    expect(within(table).getByRole('columnheader', { name: /Длина, мм/ })).toBeTruthy()

    const rows = within(table).getAllByRole('row')
    // Header row plus two data rows.
    expect(rows).toHaveLength(3)

    // The row's own label is a header, so only the two measured columns are cells.
    const firstData = within(rows[1]).getAllByRole('cell')
    expect(firstData).toHaveLength(2)
    expect(firstData[0].textContent).toContain('1200')
    expect(firstData[0].textContent).toContain('мм')

    // The load the catalogue does not print stays blank — not 0, not a dash.
    // Asserted on the value itself: the cell around it now also carries the
    // product and the property this missing value would have belonged to.
    const secondData = within(rows[2]).getAllByRole('cell')
    const blankLine = secondData[1].querySelector('.cell-line')
    expect(blankLine?.textContent?.trim()).toBe('')
    expect(blankLine?.textContent).not.toContain('0')
    expect(blankLine?.textContent).not.toContain('—')
  })

  it('never presents a column label as a value of anything', async () => {
    const user = userEvent.setup()
    install()
    renderPanel()

    await waitFor(() => expect(screen.getAllByRole('listitem')).toHaveLength(3))
    const third = screen.getAllByRole('listitem')[2]
    await user.click(within(third).getByRole('button', { name: 'Что извлечено' }))

    const table = await screen.findByRole('table')
    const header = within(table).getByRole('columnheader', {
      name: /безопасная рабочая нагрузка/,
    })
    expect(within(header).getByText(/это заголовок таблицы, а не значение/)).toBeTruthy()

    for (const cell of within(table).getAllByRole('cell')) {
      expect(cell.textContent).not.toContain('безопасная рабочая нагрузка (Н)')
    }
  })

  it('shows a schematic of region positions and never calls it the page', async () => {
    const user = userEvent.setup()
    install()
    renderPanel()

    await waitFor(() => expect(screen.getAllByRole('listitem')).toHaveLength(3))
    const third = screen.getAllByRole('listitem')[2]
    await user.click(within(third).getByRole('button', { name: 'Что извлечено' }))

    expect(await screen.findByText(/не изображение страницы/)).toBeTruthy()

    const placed = within(
      screen.getByRole('list', { name: 'Области с известными координатами' }),
    ).getAllByRole('listitem')
    expect(placed).toHaveLength(1)
    expect(placed[0].dataset.regionId).toBe('region-table')

    // The region whose coordinates the engine never returned is listed with the
    // server's reason and gets no rectangle.
    expect(
      within(screen.getByRole('list', { name: 'Области без координат' })).getByText(
        /движок распознавания не возвращает координат слов/,
      ),
    ).toBeTruthy()
    expect(document.querySelector('[data-region-id="region-ocr-note"].page-map__region')).toBeNull()
  })

  it('says that a drawing on the page was not interpreted', async () => {
    const user = userEvent.setup()
    install()
    renderPanel()

    await waitFor(() => expect(screen.getAllByRole('listitem')).toHaveLength(3))
    const second = screen.getAllByRole('listitem')[1]
    await user.click(within(second).getByRole('button', { name: 'Что извлечено' }))

    expect(await screen.findByText(/Схема на странице не интерпретирована/)).toBeTruthy()
    expect(screen.getByText(/не чтение самой схемы/)).toBeTruthy()
  })

  it('names the reading each page came from', async () => {
    install()
    renderPanel()

    await waitFor(() => expect(screen.getAllByText(/ревизия чтения: r2/)).toHaveLength(3))
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
