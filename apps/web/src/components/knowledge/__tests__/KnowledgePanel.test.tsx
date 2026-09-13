import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type {
  FactEvidence,
  GlossaryTerm,
  KnowledgeFact,
  KnowledgeGap,
  KnowledgeOverview,
  KnowledgeRun,
  ProductNode,
  ProviderState,
  QaEntry,
} from '../../../api/types'
import { AuthProvider } from '../../../auth/AuthContext'
import { apiError, installMockFetch, jsonResponse } from '../../../test/mockFetch'
import { KnowledgePanel } from '../KnowledgePanel'

const PARTNER = 'partner-a'
const MATERIAL = 'material-1'

const QUOTE = 'BP21 1200 3.5 kN при опирании на две опоры'

function providerReady(): ProviderState {
  return {
    state: 'ready',
    provider: 'openrouter',
    model: 'openai/gpt-4o-mini',
    endpoint_host: 'openrouter.ai',
    missing: [],
    message: 'Продуктолог использует модель openai/gpt-4o-mini через openrouter.ai.',
  }
}

function providerNeedsKey(): ProviderState {
  return {
    state: 'needs_configuration',
    provider: 'openrouter',
    model: null,
    endpoint_host: 'openrouter.ai',
    missing: ['OTDEL_LLM_API_KEY', 'OTDEL_LLM_MODEL'],
    message:
      'Продуктолог ожидает настройки: не заданы OTDEL_LLM_API_KEY, OTDEL_LLM_MODEL. ' +
      'Пока ключа нет, обращений к модели не происходит и знания не создаются.',
  }
}

function evidence(): FactEvidence {
  return {
    id: 'evidence-1',
    material_id: MATERIAL,
    material_filename: 'catalogue.pdf',
    page_id: 'page-3',
    page_number: 3,
    region_id: null,
    quote: QUOTE,
    char_start: 120,
    char_end: 120 + QUOTE.length,
  }
}

function fact(overrides: Partial<KnowledgeFact> = {}): KnowledgeFact {
  return {
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
    model_context: 'Значение приведено для профиля без дополнительных креплений.',
    evidence: [evidence()],
    created_at: '2026-01-01T10:00:00Z',
    ...overrides,
  }
}

function run(overrides: Partial<KnowledgeRun> = {}): KnowledgeRun {
  return {
    id: 'run-1',
    partner_id: PARTNER,
    material_id: MATERIAL,
    material_filename: 'catalogue.pdf',
    status: 'partial',
    provider: 'openrouter',
    model: 'openai/gpt-4o-mini',
    prompt_profile: 'productologist/2026-09-13',
    pages_considered: 3,
    requests_made: 1,
    input_chars: 4200,
    categories_created: 1,
    products_created: 1,
    facts_accepted: 1,
    facts_rejected: 2,
    terms_created: 1,
    qa_created: 1,
    gaps_created: 1,
    questions_created: 1,
    rejections: [
      'факт «цена» отклонён: нет ни одной подтверждённой цитаты из этого материала',
      'факт «нагрузка» отклонён: цитата не найдена дословно на указанной странице (страница 3)',
    ],
    diagnostic: null,
    started_at: '2026-01-01T10:00:00Z',
    finished_at: '2026-01-01T10:00:09Z',
    created_at: '2026-01-01T09:59:00Z',
    ...overrides,
  }
}

function overview(overrides: Partial<KnowledgeOverview> = {}): KnowledgeOverview {
  return {
    provider: providerReady(),
    summary: {
      categories_total: 1,
      products_total: 1,
      facts_total: 1,
      terms_total: 1,
      qa_total: 1,
      gaps_total: 1,
      questions_total: 1,
      materials_readable: 1,
      materials_understood: 1,
    },
    runs: [run()],
    pending_materials: [],
    ...overrides,
  }
}

function products(): ProductNode[] {
  return [
    {
      product: {
        id: 'product-1',
        partner_id: PARTNER,
        material_id: MATERIAL,
        run_id: 'run-1',
        category_id: 'category-1',
        kind: 'product',
        name: 'BP21',
        summary: 'профиль монтажный',
        created_at: '2026-01-01T10:00:00Z',
      },
      category: {
        id: 'category-1',
        partner_id: PARTNER,
        material_id: MATERIAL,
        run_id: 'run-1',
        kind: 'direction',
        name: 'Монтажные системы',
        summary: null,
        created_at: '2026-01-01T10:00:00Z',
      },
      facts: [fact()],
    },
  ]
}

function glossary(): GlossaryTerm[] {
  return [
    {
      id: 'term-1',
      partner_id: PARTNER,
      material_id: MATERIAL,
      run_id: 'run-1',
      term: 'консоль',
      definition: 'опорный элемент крепления (формулировка модели)',
      definition_is_model_context: true,
      evidence: [evidence()],
      created_at: '2026-01-01T10:00:00Z',
    },
  ]
}

function qa(): QaEntry[] {
  return [
    {
      id: 'qa-1',
      partner_id: PARTNER,
      material_id: MATERIAL,
      run_id: 'run-1',
      question: 'Какая нагрузка у BP21?',
      answer: '3.5 kN при опирании на две опоры.',
      answer_is_model_context: true,
      evidence: [evidence()],
      created_at: '2026-01-01T10:00:00Z',
    },
  ]
}

function gaps(): KnowledgeGap[] {
  return [
    {
      id: 'gap-1',
      partner_id: PARTNER,
      material_id: MATERIAL,
      run_id: 'run-1',
      product_id: 'product-1',
      product_name: 'BP21',
      topic: 'price',
      missing: 'цена в материале не указана',
      blocks: 'коммерческое предложение',
      question: {
        id: 'question-1',
        audience: 'partner',
        text: 'Какая отпускная цена профиля BP21?',
        status: 'prepared',
        created_at: '2026-01-01T10:00:00Z',
      },
      created_at: '2026-01-01T10:00:00Z',
    },
  ]
}

interface Options {
  overviewBody?: KnowledgeOverview
  onUnderstand?: () => Response
}

function install(options: Options = {}) {
  return installMockFetch((req) => {
    if (req.url.endsWith('/api/session')) {
      return jsonResponse(200, { authenticated: true, csrf_token: 'csrf-1' })
    }
    if (req.url.endsWith('/understand') && options.onUnderstand) {
      return options.onUnderstand()
    }
    if (req.url.endsWith('/knowledge')) {
      return jsonResponse(200, options.overviewBody ?? overview())
    }
    if (req.url.endsWith('/knowledge/products')) {
      return jsonResponse(200, { items: products() })
    }
    if (req.url.endsWith('/knowledge/glossary')) {
      return jsonResponse(200, { items: glossary() })
    }
    if (req.url.endsWith('/knowledge/qa')) {
      return jsonResponse(200, { items: qa() })
    }
    if (req.url.endsWith('/knowledge/gaps')) {
      return jsonResponse(200, { items: gaps() })
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'нет', retryable: false } })
  })
}

function renderPanel() {
  return render(
    <AuthProvider>
      <KnowledgePanel partnerId={PARTNER} />
    </AuthProvider>,
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('KnowledgePanel', () => {
  it('shows the exact quotation and its source, and marks model context as not a quote', async () => {
    install()
    renderPanel()

    // Both a fact and a Q&A entry offer their source; this test is about the fact,
    // so the query is scoped to the products section rather than made ambiguous.
    const facts = await screen.findByRole('list', { name: 'Продукты и характеристики' })
    await userEvent.click(within(facts).getByRole('button', { name: 'Показать источник' }))

    // The quotation is a quotation, verbatim. Scoped to the fact: the glossary
    // entry cites the same fragment, which is legitimate, not a duplicate render.
    const quote = within(facts).getByText(QUOTE)
    expect(quote.closest('blockquote')).not.toBeNull()
    expect(within(quote.closest('blockquote')!).getByText('цитата источника')).toBeTruthy()

    // The source is named exactly and links to that page of the original.
    const link = within(facts).getByRole('link', { name: 'catalogue.pdf, стр. 3' })
    expect(link.getAttribute('href')).toBe(
      `/api/partners/${PARTNER}/materials/${MATERIAL}/original#page=3`,
    )

    // The model's own wording is labelled and lives outside the quotation.
    const context = within(facts).getByText(/Значение приведено для профиля/)
    expect(context.closest('blockquote')).toBeNull()
    expect(screen.getByText('пояснение модели — не цитата')).toBeTruthy()
  })

  it('prints the value with its unit and the conditions it holds under', async () => {
    install()
    renderPanel()

    const facts = await screen.findByRole('list', { name: 'Продукты и характеристики' })
    expect(within(facts).getByText('3.5 kN')).toBeTruthy()
    // The label and the value are separate nodes; the paragraph carries both.
    expect(within(facts).getByText(/условия:/).closest('p')?.textContent).toContain(
      'при опирании на две опоры',
    )
    // A candidate is described as a candidate, not as a verified fact — both on
    // the panel and next to the fact itself.
    expect(screen.getAllByText(/не независимая проверка/).length).toBe(2)
  })

  it('shows a value without a unit when the source did not print one', async () => {
    installMockFetch((req) => {
      if (req.url.endsWith('/api/session')) {
        return jsonResponse(200, { authenticated: true, csrf_token: 'csrf-1' })
      }
      if (req.url.endsWith('/knowledge')) return jsonResponse(200, overview())
      if (req.url.endsWith('/knowledge/products')) {
        const nodes = products()
        nodes[0].facts = [fact({ unit: null, conditions: null, model_context: null })]
        return jsonResponse(200, { items: nodes })
      }
      if (req.url.endsWith('/knowledge/glossary')) return jsonResponse(200, { items: [] })
      if (req.url.endsWith('/knowledge/qa')) return jsonResponse(200, { items: [] })
      if (req.url.endsWith('/knowledge/gaps')) return jsonResponse(200, { items: [] })
      return jsonResponse(404, { error: { code: 'not_found', message: 'нет', retryable: false } })
    })
    renderPanel()

    expect(await screen.findByText('3.5')).toBeTruthy()
    // Nothing invents "kN" where the server stored no unit.
    expect(screen.queryByText('3.5 kN')).toBeNull()
    expect(screen.queryByText(/условия:/)).toBeNull()
  })

  it('says what the product role is waiting for and disables the button until it is configured', async () => {
    install({ overviewBody: overview({ provider: providerNeedsKey() }) })
    renderPanel()

    expect(await screen.findByText('Продуктолог ожидает настройки')).toBeTruthy()
    expect(screen.getByText('OTDEL_LLM_API_KEY')).toBeTruthy()
    expect(screen.getByText('OTDEL_LLM_MODEL')).toBeTruthy()
    // Said twice on purpose — in the server's message and in the panel's own
    // footnote — so either one alone is enough to understand the state.
    expect(screen.getAllByText(/обращений к модели не происходит/).length).toBeGreaterThan(0)

    const button = screen.getByRole('button', { name: 'Разобрать заново' })
    expect(button).toBeDisabled()

    // The state is named honestly: waiting for configuration is not a failure.
    expect(screen.queryByText('Разбор не выполнен')).toBeNull()
  })

  it('repeats the server’s reason when queueing a draft is refused', async () => {
    install({
      onUnderstand: () =>
        apiError(
          409,
          'conflict',
          'Продуктолог ожидает настройки: не заданы OTDEL_LLM_API_KEY.',
          true,
        ),
    })
    renderPanel()

    const button = await screen.findByRole('button', { name: 'Разобрать заново' })
    await userEvent.click(button)

    await waitFor(() =>
      expect(screen.getByRole('alert').textContent).toContain('OTDEL_LLM_API_KEY'),
    )
  })

  it('shows why candidates were refused, in the server’s own words', async () => {
    install()
    renderPanel()

    expect(await screen.findByText('Разобран частично')).toBeTruthy()
    await userEvent.click(screen.getByText('Почему предложения модели отклонены'))
    expect(screen.getByText(/нет ни одной подтверждённой цитаты/)).toBeTruthy()
    expect(screen.getByText(/не найдена дословно/)).toBeTruthy()
    expect(screen.getByText(/отклонено: 2/)).toBeTruthy()
  })

  it('marks a glossary definition written by the model, and shows a gap as a gap', async () => {
    install()
    renderPanel()

    expect(await screen.findByText('консоль')).toBeTruthy()
    expect(screen.getByText('формулировка модели')).toBeTruthy()

    // A missing price is a gap with a prepared question — never a number.
    expect(screen.getByText('цена в материале не указана')).toBeTruthy()
    expect(screen.getByText(/вопрос партнёру · подготовлен, канал не подключён/)).toBeTruthy()
    expect(screen.getByText('Какая отпускная цена профиля BP21?')).toBeTruthy()
  })

  it('marks a Q&A answer as the model’s sentence, not as a quotation', async () => {
    install()
    renderPanel()

    const answers = await screen.findByRole('list', {
      name: 'Вопросы и ответы по материалам',
    })
    expect(within(answers).getByText('3.5 kN при опирании на две опоры.')).toBeTruthy()
    expect(
      within(answers).getByText('ответ сформулирован моделью — не цитата'),
    ).toBeTruthy()
  })

  it('offers a first draft for a material that has never been drafted', async () => {
    const { calls } = install({
      overviewBody: overview({
        runs: [],
        pending_materials: [
          { material_id: MATERIAL, filename: 'presentation.pdf', pages_with_text: 4 },
        ],
      }),
    })
    renderPanel()

    // The material is named, with the only number that matters for a draft.
    expect(await screen.findByText('presentation.pdf')).toBeTruthy()
    expect(screen.getByText('страниц с текстом: 4')).toBeTruthy()

    const button = screen.getByRole('button', { name: 'Разобрать' })
    expect(button).toBeEnabled()
    await userEvent.click(button)

    await waitFor(() => {
      const posted = calls.find((call) => call.url.endsWith('/understand'))
      expect(posted?.method).toBe('POST')
      expect(posted?.headers['x-csrf-token']).toBe('csrf-1')
    })
  })

  it('keeps the draft on screen when a refresh fails', async () => {
    let overviews = 0
    installMockFetch((req) => {
      if (req.url.endsWith('/api/session')) {
        return jsonResponse(200, { authenticated: true, csrf_token: 'csrf-1' })
      }
      if (req.url.endsWith('/understand')) {
        return jsonResponse(200, run({ status: 'queued' }))
      }
      if (req.url.endsWith('/knowledge')) {
        overviews += 1
        // The first load succeeds; the refresh that follows the action fails, the
        // way a two-second network blip would.
        return overviews === 1
          ? jsonResponse(200, overview())
          : apiError(503, 'service_unavailable', 'База данных недоступна.', true)
      }
      if (req.url.endsWith('/knowledge/products')) {
        return jsonResponse(200, { items: products() })
      }
      if (req.url.endsWith('/knowledge/glossary')) return jsonResponse(200, { items: glossary() })
      if (req.url.endsWith('/knowledge/qa')) return jsonResponse(200, { items: qa() })
      if (req.url.endsWith('/knowledge/gaps')) return jsonResponse(200, { items: gaps() })
      return jsonResponse(404, { error: { code: 'not_found', message: 'нет', retryable: false } })
    })
    renderPanel()

    const facts = await screen.findByRole('list', { name: 'Продукты и характеристики' })
    expect(within(facts).getByText('3.5 kN')).toBeTruthy()

    // Queueing a draft triggers a refresh, and that refresh fails.
    await userEvent.click(screen.getByRole('button', { name: 'Разобрать заново' }))

    await waitFor(() => expect(screen.getByText('База данных недоступна.')).toBeTruthy())
    // The failure is reported *next to* the draft, not instead of it.
    expect(
      within(screen.getByRole('list', { name: 'Продукты и характеристики' })).getByText('3.5 kN'),
    ).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Повторить' })).toBeTruthy()
  })

  it('never shows a percentage or an estimated time', async () => {
    install()
    renderPanel()

    await screen.findAllByText('BP21')
    const text = document.body.textContent ?? ''
    expect(text).not.toMatch(/%/)
    expect(text).not.toMatch(/осталось|примерно|минут/i)
  })
})
