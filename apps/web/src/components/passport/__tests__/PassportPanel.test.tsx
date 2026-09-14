import { render, screen, within } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type {
  CoverageReport,
  KnowledgeUncertainty,
  ProductApplication,
  ProductPassport,
} from '../../../api/types'
import { AuthProvider } from '../../../auth/AuthContext'
import { installMockFetch, jsonResponse } from '../../../test/mockFetch'
import { PassportPanel } from '../PassportPanel'

const PARTNER = 'partner-a'
const MATERIAL = 'material-1'
const QUOTE = 'BP21 1200 3.5 kN при опирании на две опоры'

/**
 * Exactly the audited run, as the interface receives it: a 44-page catalogue,
 * 36 pages read, 8 waiting on a budget, and a draft that does not carry what a
 * passport needs.
 */
function auditedCoverage(overrides: Partial<CoverageReport> = {}): CoverageReport {
  return {
    run_id: 'run-1',
    material_id: MATERIAL,
    material_filename: 'catalogue.pdf',
    status: 'partial',
    pages_total: 44,
    pages_offered: 36,
    pages_processed: 36,
    pages_deferred: 8,
    pages_unreadable: 0,
    state: 'incomplete',
    notes: ['страница отложена: исчерпан бюджет запросов: 8 стр. (37–44)'],
    requirements: 'unmet',
    requirements_missing: [
      'glossary: нет терминов и не сказано, что материал не вводит терминов',
      'commercial_unknowns: коммерческие неизвестные не зафиксированы',
    ],
    prompt_tokens: null,
    completion_tokens: null,
    cost_micro_usd: null,
    allows_automatic_publication: false,
    resumable_pages: [37, 38],
    pages: [
      {
        id: 'pc-1',
        material_id: MATERIAL,
        run_id: 'run-1',
        page_id: 'page-1',
        page_number: 1,
        disposition: 'processed',
        offered: true,
        chars_sent: 900,
        batch_index: 1,
        reason: null,
        created_at: '2026-01-01T10:00:00Z',
      },
      {
        id: 'pc-2',
        material_id: MATERIAL,
        run_id: 'run-1',
        page_id: 'page-37',
        page_number: 37,
        disposition: 'deferred_budget',
        offered: false,
        chars_sent: 0,
        batch_index: null,
        reason: 'страница отложена: исчерпан бюджет запросов',
        created_at: '2026-01-01T10:00:00Z',
      },
    ],
    declarations: [],
    ...overrides,
  }
}

function passport(overrides: Partial<ProductPassport> = {}): ProductPassport {
  return {
    product: {
      id: 'product-1',
      partner_id: PARTNER,
      material_id: MATERIAL,
      run_id: 'run-1',
      category_id: null,
      kind: 'product',
      name: 'BP21',
      summary: 'профиль монтажный',
      created_at: '2026-01-01T10:00:00Z',
    },
    category: null,
    material_filename: 'catalogue.pdf',
    aliases: [
      {
        id: 'alias-1',
        product_id: 'product-1',
        material_id: MATERIAL,
        run_id: 'run-1',
        surface: 'Профиль BP 21',
        relation: 'unclear',
        note: 'встречается в этом же абзаце',
        page_id: 'page-4',
        page_number: 4,
        quote: 'Профиль BP 21 монтажный',
        char_start: 0,
        char_end: 23,
        created_at: '2026-01-01T10:00:00Z',
      },
    ],
    facts: [
      {
        id: 'fact-1',
        partner_id: PARTNER,
        material_id: MATERIAL,
        run_id: 'run-1',
        product_id: 'product-1',
        product_name: 'BP21',
        kind: 'characteristic',
        status: 'candidate',
        attribute: 'нагрузка',
        value_text: '3.5',
        unit: 'kN',
        conditions: 'при опирании на две опоры',
        model_context: null,
        evidence: [
          {
            id: 'ev-1',
            material_id: MATERIAL,
            material_filename: 'catalogue.pdf',
            page_id: 'page-3',
            page_number: 3,
            region_id: null,
            quote: QUOTE,
            char_start: 0,
            char_end: QUOTE.length,
          },
        ],
        origin: {
          source: 'table_cell',
          cell_id: 'cell-1',
          subject: 'BP21',
          property: 'Безопасная рабочая нагрузка',
          unit: 'кН',
          conditions: ['две опоры'],
        },
        created_at: '2026-01-01T10:00:00Z',
      },
    ],
    applications: [],
    gaps: [
      {
        id: 'gap-1',
        partner_id: PARTNER,
        material_id: MATERIAL,
        run_id: 'run-1',
        product_id: 'product-1',
        product_name: 'BP21',
        topic: 'price',
        missing: 'цена в материале не указана',
        blocks: null,
        nature: 'commercial',
        question: null,
        created_at: '2026-01-01T10:00:00Z',
      },
    ],
    uncertainties: [
      {
        id: 'unc-1',
        partner_id: PARTNER,
        material_id: MATERIAL,
        run_id: 'run-1',
        product_id: 'product-1',
        kind: 'unresolved_unit',
        subject: 'колонка «Нагрузка»',
        detail: 'единица измерения нигде на странице не написана (ячеек: 4)',
        reasons: ['unit_not_stated'],
        quote: '3,5',
        page_id: 'page-9',
        page_number: 9,
        region_id: 'region-1',
        status: 'open',
        created_at: '2026-01-01T10:00:00Z',
      },
    ],
    identity_links: [
      {
        id: 'link-1',
        product_id: 'product-1',
        other_product_id: 'product-2',
        state: 'unclear',
        basis: 'name_similarity_only',
        note: 'названия совпадают, но обозначение не процитировано с обеих сторон',
        material_id: null,
        page_id: null,
        page_number: null,
        other_material_id: null,
        other_page_id: null,
        other_page_number: null,
        created_at: '2026-01-01T10:00:00Z',
      },
    ],
    ...overrides,
  }
}

function install(options: {
  coverage?: CoverageReport[]
  passports?: ProductPassport[]
  applications?: ProductApplication[]
  uncertainties?: KnowledgeUncertainty[]
} = {}) {
  return installMockFetch((req) => {
    if (req.url.includes('/coverage')) {
      return jsonResponse(200, { items: options.coverage ?? [auditedCoverage()] })
    }
    if (req.url.includes('/passports')) {
      return jsonResponse(200, { items: options.passports ?? [passport()] })
    }
    if (req.url.includes('/applications')) {
      return jsonResponse(200, { items: options.applications ?? [] })
    }
    if (req.url.includes('/uncertainties')) {
      return jsonResponse(200, { items: options.uncertainties ?? [] })
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'нет', retryable: false } })
  })
}

function renderPanel() {
  return render(
    <AuthProvider>
      <PassportPanel partnerId={PARTNER} />
    </AuthProvider>,
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('PassportPanel', () => {
  it('shows the denominator, not only what was read', async () => {
    install()
    renderPanel()

    // «36 из 44», never a bare «36». The bare number is the report the audit
    // could not act on.
    expect(await screen.findByText(/разобрано 36 из 44/)).toBeTruthy()
    expect(screen.getByText(/отложено 8/)).toBeTruthy()
  })

  it('states that the material may not be published without a person, and why', async () => {
    install()
    renderPanel()

    expect(
      await screen.findByText('Публиковать автоматически нельзя: нужен человек.'),
    ).toBeTruthy()
    // The two verdicts stay separate: coverage and content fail for different
    // reasons and are fixed by different actions.
    expect(screen.getByText('разбор не завершён')).toBeTruthy()
    expect(screen.getByText('паспорту не хватает данных')).toBeTruthy()
    // Each unmet requirement is named in words the owner can act on.
    expect(
      screen.getByText('нет терминов и не сказано, что материал не вводит терминов'),
    ).toBeTruthy()
  })

  it('never renders an unconfirmed alias as another name for the product', async () => {
    install()
    renderPanel()

    const passports = await screen.findByLabelText('Паспорт изделия «BP21»')
    expect(within(passports).getByText('Профиль BP 21')).toBeTruthy()
    // Not «возможно, то же»: an interface that hedges towards identity will be
    // read as asserting one.
    expect(within(passports).getByText('связь не подтверждена материалом')).toBeTruthy()
  })

  it('shows the gaps and the unreadable values beside the facts, not instead of them', async () => {
    install()
    renderPanel()

    const card = await screen.findByLabelText('Паспорт изделия «BP21»')
    expect(within(card).getByText('3.5 kN')).toBeTruthy()
    // …and, without the reader asking for it, everything that is still open.
    expect(within(card).getByText('цена в материале не указана')).toBeTruthy()
    expect(within(card).getByText('коммерческий')).toBeTruthy()
    expect(within(card).getByText('единица измерения нигде не написана')).toBeTruthy()
  })

  it('says which table column a number came from, beside its quotation', async () => {
    install()
    renderPanel()

    const card = await screen.findByLabelText('Паспорт изделия «BP21»')
    expect(within(card).getByText('из ячейки таблицы')).toBeTruthy()
    expect(within(card).getByText(/Безопасная рабочая нагрузка/)).toBeTruthy()
    // The quotation is still there: the structural origin is a second kind of
    // provenance, never a replacement for the first.
    expect(within(card).getByText(QUOTE)).toBeTruthy()
  })

  it('never presents an identity proposal as a merge', async () => {
    install()
    renderPanel()

    const card = await screen.findByLabelText('Паспорт изделия «BP21»')
    expect(within(card).getByText('совпадение не подтверждено')).toBeTruthy()
    expect(
      within(card).getByText('совпадают только названия — это не доказательство'),
    ).toBeTruthy()
    expect(
      within(card).getByText('Записи остаются раздельными: система ничего не объединяет сама.'),
    ).toBeTruthy()
  })

  it('names a product row that carries nothing beyond its name', async () => {
    const bare = passport({
      product: {
        id: 'product-9',
        partner_id: PARTNER,
        material_id: MATERIAL,
        run_id: 'run-1',
        category_id: null,
        kind: 'product',
        name: 'BP99',
        summary: null,
        created_at: '2026-01-01T10:00:00Z',
      },
      aliases: [],
      facts: [],
      applications: [],
      gaps: [],
      uncertainties: [],
      identity_links: [],
    })
    install({ passports: [bare] })
    renderPanel()

    const card = await screen.findByLabelText('Паспорт изделия «BP99»')
    // The audited run produced forty-four of exactly this shape and reported
    // them as products. The interface says what it is instead.
    expect(within(card).getByText(/это строка каталога/)).toBeTruthy()
  })

  it('never shows a percentage or an estimated time', async () => {
    install()
    renderPanel()

    await screen.findByText(/разобрано 36 из 44/)
    const text = document.body.textContent ?? ''
    expect(text).not.toMatch(/%/)
    expect(text).not.toMatch(/осталось|примерно|минут/i)
  })
})
