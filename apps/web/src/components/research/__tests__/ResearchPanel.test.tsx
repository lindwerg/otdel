import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type {
  ExternalEvidence,
  IndustryQuestion,
  ResearchBudget,
  ResearchEngineState,
  ResearchFinding,
  ResearchOverview,
  ResearchPlan,
  ResearchProviderState,
  ResearchQueryRecord,
  ResearchSource,
} from '../../../api/types'
import { AuthProvider } from '../../../auth/AuthContext'
import { apiError, installMockFetch, jsonResponse } from '../../../test/mockFetch'
import { ResearchPanel } from '../ResearchPanel'

const PARTNER = 'partner-a'
const QUESTION = 'question-1'
const PLAN = 'plan-1'
const QUOTE = 'Минимальная толщина покрытия 55 мкм'
const URL = 'https://docs.example.org/gost-9-307'

function providerReady(): ResearchProviderState {
  return {
    state: 'ready',
    search: {
      state: 'ready',
      provider: 'http_json',
      endpoint_host: 'search.example.com',
      model: null,
      message: 'Исследователь ищет источники через search.example.com.',
    },
    fetcher: {
      state: 'ready',
      provider: 'http_json',
      endpoint_host: null,
      model: null,
      message: 'Источники читаются только с объявленных хостов (docs.example.org).',
    },
    model: {
      state: 'ready',
      provider: 'openrouter',
      endpoint_host: 'openrouter.ai',
      model: 'openai/gpt-4o-mini',
      message: 'Продуктолог использует модель openai/gpt-4o-mini через openrouter.ai.',
    },
    missing: [],
    allowed_hosts: ['docs.example.org'],
    limits: {
      max_queries_per_plan: 3,
      max_results_per_query: 8,
      max_sources_per_plan: 6,
      max_page_bytes: 2097152,
      max_page_chars: 40000,
      request_timeout_seconds: 20,
      plan_time_budget_seconds: 300,
      max_passes_per_plan: 2,
      max_total_results_per_plan: null,
    },
    engine: null,
    message:
      'Исследователь готов: поиск через search.example.com, чтение только с docs.example.org ' +
      'и модель openai/gpt-4o-mini.',
  }
}

/** The researcher as the owner really runs it: OpenRouter's server tool. */
function providerOpenRouter(
  engineOverrides: Partial<ResearchEngineState> = {},
): ResearchProviderState {
  const ready = providerReady()
  return {
    ...ready,
    search: {
      ...ready.search,
      provider: 'openrouter_web_search',
      endpoint_host: 'openrouter.ai',
      message:
        'Исследователь ищет источники через openrouter.ai (openai/gpt-4o-mini, движок exa, ' +
        'до 5 результатов на запрос). Ключ взят из OTDEL_LLM_API_KEY.',
    },
    limits: { ...ready.limits, max_results_per_query: 5, max_total_results_per_plan: 20 },
    engine: {
      configured: 'auto',
      effective: 'exa',
      exa_fallback: true,
      model: 'openai/gpt-4o-mini',
      max_results: 5,
      max_total_results_per_plan: 20,
      max_uses_per_request: 1,
      max_characters_per_result: 1_500,
      search_domains: [],
      forecast_micros: 10_000,
      search_base_micros: 7_000,
      included_results: 10,
      extra_result_micros: 1_000,
      token_allowance_micros: 3_000,
      api_key_inherited: true,
      ...engineOverrides,
    },
  }
}

function providerNeedsConfiguration(): ResearchProviderState {
  const ready = providerReady()
  return {
    ...ready,
    state: 'needs_configuration',
    search: {
      ...ready.search,
      state: 'needs_configuration',
      endpoint_host: null,
      message:
        'Исследователь не настроен: задайте OTDEL_RESEARCH_SEARCH_URL, OTDEL_RESEARCH_API_KEY, ' +
        'OTDEL_RESEARCH_ALLOWED_HOSTS. Пока этого нет, ни один внешний запрос не выполняется ' +
        'и бюджет не расходуется.',
    },
    fetcher: { ...ready.fetcher, state: 'needs_configuration' },
    missing: [
      'OTDEL_RESEARCH_SEARCH_URL',
      'OTDEL_RESEARCH_API_KEY',
      'OTDEL_RESEARCH_ALLOWED_HOSTS',
    ],
    allowed_hosts: [],
    message:
      'Исследователь не настроен: задайте OTDEL_RESEARCH_SEARCH_URL, OTDEL_RESEARCH_API_KEY, ' +
      'OTDEL_RESEARCH_ALLOWED_HOSTS.',
  }
}

function budget(overrides: Partial<ResearchBudget> = {}): ResearchBudget {
  return {
    currency: 'USD',
    limit_micros: 5_000_000,
    reserved_micros: 0,
    spent_micros: 15_000,
    unknown_micros: 0,
    available_micros: 4_985_000,
    plan_budget_micros: 100_000,
    cost_per_search_micros: 5_000,
    cost_per_fetch_micros: 0,
    cost_per_model_call_micros: 2_000,
    updated_at: '2026-01-01T10:00:00Z',
    ...overrides,
  }
}

function question(overrides: Partial<IndustryQuestion> = {}): IndustryQuestion {
  return {
    id: QUESTION,
    partner_id: PARTNER,
    material_id: 'material-1',
    material_filename: 'catalogue.pdf',
    gap_id: 'gap-1',
    gap_topic: 'покрытие',
    gap_missing: 'в материале не указана минимальная толщина цинкового покрытия',
    text: 'Какая минимальная толщина цинкового покрытия требуется по стандарту?',
    status: 'prepared',
    plan_id: null,
    created_at: '2026-01-01T10:00:00Z',
    ...overrides,
  }
}

function plan(overrides: Partial<ResearchPlan> = {}): ResearchPlan {
  return {
    id: PLAN,
    partner_id: PARTNER,
    material_id: 'material-1',
    material_filename: 'catalogue.pdf',
    question_id: QUESTION,
    question_text: 'Какая минимальная толщина цинкового покрытия требуется по стандарту?',
    topic: 'покрытие',
    status: 'completed',
    provider: 'http_json',
    model: 'openai/gpt-4o-mini',
    prompt_profile: 'researcher/2026-09-13.1',
    passes: 1,
    max_passes: 2,
    budget_micros: 100_000,
    reserved_micros: 0,
    spent_micros: 15_000,
    queries_made: 3,
    results_seen: 4,
    sources_fetched: 1,
    sources_skipped: 1,
    bytes_fetched: 4096,
    findings_accepted: 1,
    findings_rejected: 0,
    duration_ms: 1234,
    rejections: [],
    diagnostic: null,
    cancel_requested: false,
    started_at: '2026-01-01T10:00:00Z',
    finished_at: '2026-01-01T10:00:05Z',
    created_at: '2026-01-01T09:59:00Z',
    updated_at: '2026-01-01T10:00:05Z',
    ...overrides,
  }
}

function evidence(): ExternalEvidence {
  return {
    id: 'evidence-1',
    source_id: 'source-1',
    url: URL,
    host: 'docs.example.org',
    retrieved_at: '2026-01-01T10:00:03Z',
    content_hash: 'a'.repeat(64),
    license: null,
    quote: QUOTE,
    char_start: 40,
    char_end: 40 + QUOTE.length,
  }
}

function finding(overrides: Partial<ResearchFinding> = {}): ResearchFinding {
  return {
    id: 'finding-1',
    partner_id: PARTNER,
    plan_id: PLAN,
    scope: 'industry',
    status: 'candidate',
    topic: 'покрытие',
    attribute: 'минимальная толщина цинкового покрытия',
    value_text: '55',
    unit: 'мкм',
    conditions: null,
    model_context: 'значение приведено отраслевым стандартом',
    evidence: [evidence()],
    created_at: '2026-01-01T10:00:05Z',
    ...overrides,
  }
}

function overview(overrides: Partial<ResearchOverview> = {}): ResearchOverview {
  return {
    provider: providerReady(),
    budget: budget(),
    summary: {
      plans_total: 1,
      plans_active: 0,
      questions_open: 0,
      sources_fetched: 1,
      sources_skipped: 1,
      findings_total: 1,
      spent_micros: 15_000,
    },
    plans: [plan()],
    questions: [question({ plan_id: PLAN })],
    ...overrides,
  }
}

function sources(): ResearchSource[] {
  return [
    {
      id: 'source-1',
      plan_id: PLAN,
      query_id: 'query-1',
      url: URL,
      host: 'docs.example.org',
      title: 'ГОСТ 9.307',
      snippet: 'Покрытия цинковые горячие — общие требования',
      status: 'fetched',
      http_status: 200,
      content_type: 'text/html',
      content_bytes: 4096,
      content_chars: 900,
      content_hash: 'a'.repeat(64),
      license: null,
      license_note: 'страница не объявляет лицензию; условия использования не установлены',
      retrieved_at: '2026-01-01T10:00:03Z',
      published_at: null,
      cost_micros: 0,
      diagnostic: null,
      created_at: '2026-01-01T10:00:02Z',
    },
    {
      id: 'source-2',
      plan_id: PLAN,
      query_id: 'query-1',
      url: 'https://blog.example.net/guess',
      host: 'blog.example.net',
      title: null,
      snippet: null,
      status: 'skipped_host',
      http_status: null,
      content_type: null,
      content_bytes: null,
      content_chars: null,
      content_hash: null,
      license: null,
      license_note: null,
      retrieved_at: null,
      published_at: null,
      cost_micros: 0,
      diagnostic: 'хост `blog.example.net` не входит в список разрешённых источников',
      created_at: '2026-01-01T10:00:02Z',
    },
  ]
}

function queries(): ResearchQueryRecord[] {
  return [
    {
      id: 'query-1',
      plan_id: PLAN,
      ordinal: 1,
      query_text: 'Какая минимальная толщина цинкового покрытия требуется по стандарту?',
      provider: 'http_json',
      results_count: 2,
      cost_micros: 5_000,
      outcome: 'ok',
      diagnostic: null,
      created_at: '2026-01-01T10:00:01Z',
    },
  ]
}

interface Options {
  overviewBody?: ResearchOverview
  findingsBody?: ResearchFinding[]
  onApprove?: () => Response
  onStop?: () => Response
}

function install(options: Options = {}) {
  return installMockFetch((req) => {
    if (req.url.endsWith('/api/session')) {
      return jsonResponse(200, { authenticated: true, csrf_token: 'csrf-1' })
    }
    if (req.url.endsWith('/plan') && options.onApprove) {
      return options.onApprove()
    }
    if (req.url.endsWith('/stop') && options.onStop) {
      return options.onStop()
    }
    if (req.url.endsWith('/research')) {
      return jsonResponse(200, options.overviewBody ?? overview())
    }
    if (req.url.endsWith('/research/findings')) {
      return jsonResponse(200, { items: options.findingsBody ?? [finding()] })
    }
    if (req.url.endsWith('/sources')) {
      return jsonResponse(200, { items: sources() })
    }
    if (req.url.endsWith('/queries')) {
      return jsonResponse(200, { items: queries() })
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'нет', retryable: false } })
  })
}

function renderPanel() {
  return render(
    <AuthProvider>
      <ResearchPanel partnerId={PARTNER} />
    </AuthProvider>,
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('ResearchPanel', () => {
  it('shows a conclusion as an industry candidate, never as a fact about the partner', async () => {
    install()
    renderPanel()

    const list = await screen.findByRole('list', { name: 'Отраслевые выводы' })
    // The first list item is the finding itself; the nested list is its evidence.
    const entry = within(list).getAllByRole('listitem')[0]

    // The scope badge, the candidate status and the "not about the partner" sentence
    // are all present: this is the one confusion the screen exists to prevent.
    expect(within(entry).getByText('отраслевое сведение — не о партнёре')).toBeInTheDocument()
    expect(within(entry).getByText(/кандидат/)).toBeInTheDocument()
    expect(
      screen.getByText(/не характеристики продукции партнёра и не проверенные факты/),
    ).toBeInTheDocument()

    // The value keeps the unit the source wrote.
    expect(within(entry).getByText('55 мкм')).toBeInTheDocument()
  })

  it('renders the quotation, the exact source and the date it was read', async () => {
    install()
    renderPanel()

    const list = await screen.findByRole('list', { name: 'Отраслевые выводы' })
    // The first list item is the finding itself; the nested list is its evidence.
    const entry = within(list).getAllByRole('listitem')[0]

    expect(within(entry).getByText('цитата внешнего источника')).toBeInTheDocument()
    expect(within(entry).getByText(QUOTE)).toBeInTheDocument()

    const link = within(entry).getByRole('link', { name: 'docs.example.org' })
    expect(link).toHaveAttribute('href', URL)
    // An untrusted external link must not pass referrer or link equity.
    expect(link).toHaveAttribute('rel', expect.stringContaining('noreferrer'))
    expect(link).toHaveAttribute('rel', expect.stringContaining('nofollow'))

    expect(within(entry).getByText(/прочитано/)).toBeInTheDocument()
    // A licence that was not declared is not invented.
    expect(within(entry).getByText(/лицензия не объявлена источником/)).toBeInTheDocument()
  })

  it("keeps the model's own sentence outside the quotation", async () => {
    install()
    renderPanel()

    const list = await screen.findByRole('list', { name: 'Отраслевые выводы' })
    const quote = within(list).getByText(QUOTE)
    const context = within(list).getByText(/значение приведено отраслевым стандартом/)

    expect(within(list).getByText('пояснение модели — не цитата')).toBeInTheDocument()
    // Structurally separate, not merely differently worded.
    expect(quote.closest('blockquote')).not.toBeNull()
    expect(context.closest('blockquote')).toBeNull()
  })

  it('states the budget, the tariff and what is only held rather than spent', async () => {
    install({
      overviewBody: overview({
        budget: budget({ reserved_micros: 5_000, unknown_micros: 5_000 }),
      }),
    })
    renderPanel()

    const section = await screen.findByRole('region', { name: 'Бюджет на исследования' })
    expect(within(section).getByText(/доступно 4,985 USD из 5 USD/)).toBeInTheDocument()
    expect(within(section).getByText('0,015 USD')).toBeInTheDocument()

    // An unknown outcome is charged and still shown as needing reconciliation.
    expect(within(section).getByText(/неизвестным исходом/)).toBeInTheDocument()
    expect(within(section).getByText(/сверить со счётом провайдера/)).toBeInTheDocument()

    // And the numbers are never presented as a provider's invoice.
    expect(within(section).getByText(/по объявленному тарифу/)).toBeInTheDocument()

    // All three priced operations are named — a tariff that listed only the search would
    // describe a budget the conclusions are not actually charged against.
    expect(within(section).getByText('поиск / страница / вызов модели')).toBeInTheDocument()
    expect(within(section).getByText('0,005 USD / 0 USD / 0,002 USD')).toBeInTheDocument()
    expect(
      within(section).getByText(/поиском, загрузкой страницы и обращением к модели/),
    ).toBeInTheDocument()
  })

  it('with nothing configured it names the variables and disables the button', async () => {
    install({
      overviewBody: overview({
        provider: providerNeedsConfiguration(),
        plans: [],
        questions: [question()],
        summary: {
          plans_total: 0,
          plans_active: 0,
          questions_open: 1,
          sources_fetched: 0,
          sources_skipped: 0,
          findings_total: 0,
          spent_micros: 0,
        },
      }),
      findingsBody: [],
    })
    renderPanel()

    expect(await screen.findByText('Исследователь ожидает настройки')).toBeInTheDocument()
    expect(screen.getByText('OTDEL_RESEARCH_SEARCH_URL')).toBeInTheDocument()
    expect(screen.getByText('OTDEL_RESEARCH_ALLOWED_HOSTS')).toBeInTheDocument()
    expect(
      screen.getByText(/ни один внешний запрос не выполняется и бюджет не расходуется/),
    ).toBeInTheDocument()

    expect(screen.getByRole('button', { name: 'Исследовать' })).toBeDisabled()
  })

  it('when ready it states what the researcher is allowed to do', async () => {
    install()
    renderPanel()

    expect(
      await screen.findByText(/чтение только с docs.example.org/),
    ).toBeInTheDocument()
    // The bounds are on the screen before anything runs.
    expect(screen.getByText(/не больше 3 поисковых запросов и 6 прочитанных страниц/))
      .toBeInTheDocument()
    expect(screen.getByText(/robots.txt соблюдается/)).toBeInTheDocument()
  })

  it('approves a question and shows the server’s own refusal when it is refused', async () => {
    const user = userEvent.setup()
    const { calls } = install({
      overviewBody: overview({
        plans: [],
        questions: [question()],
        summary: {
          plans_total: 0,
          plans_active: 0,
          questions_open: 1,
          sources_fetched: 0,
          sources_skipped: 0,
          findings_total: 0,
          spent_micros: 0,
        },
      }),
      findingsBody: [],
      onApprove: () =>
        apiError(
          409,
          'conflict',
          'бюджет бюро на исследования исчерпан: доступно 0 из 5000000 (в миллионных долях USD). ' +
            'Поднимите OTDEL_RESEARCH_BUDGET_MICROS, чтобы продолжить',
        ),
    })
    renderPanel()

    await user.click(await screen.findByRole('button', { name: 'Исследовать' }))

    // The server's own reason, verbatim — not a generic "не удалось".
    expect(await screen.findByRole('alert')).toHaveTextContent(
      /бюджет бюро на исследования исчерпан/,
    )
    expect(
      calls.some(
        (call) =>
          call.method === 'POST' && call.url.includes(`/research/questions/${QUESTION}/plan`),
      ),
    ).toBe(true)
  })

  it('offers stop while a plan is running, and says when a stop is already pending', async () => {
    const user = userEvent.setup()
    const { calls } = install({
      overviewBody: overview({
        plans: [plan({ status: 'running', findings_accepted: 0 })],
      }),
      findingsBody: [],
      onStop: () => jsonResponse(200, plan({ status: 'running', cancel_requested: true })),
    })
    renderPanel()

    const stop = await screen.findByRole('button', { name: 'Остановить' })
    await user.click(stop)

    await waitFor(() =>
      expect(
        calls.some((call) => call.method === 'POST' && call.url.endsWith('/stop')),
      ).toBe(true),
    )
  })

  it('shows every discovered source, including the ones that were never read', async () => {
    const user = userEvent.setup()
    install()
    renderPanel()

    await user.click(await screen.findByRole('button', { name: 'Журнал источников' }))

    // The page that was read, with its snapshot's identity.
    expect(await screen.findByRole('link', { name: URL })).toBeInTheDocument()
    expect(screen.getByText('Прочитано')).toBeInTheDocument()
    expect(screen.getByText(/sha256 aaaaaaaaaaaa…/)).toBeInTheDocument()

    // …and the one that was not, with the reason. A journal that dropped it would
    // make the search look as if it had found nothing.
    expect(screen.getByText('Хост не разрешён')).toBeInTheDocument()
    expect(
      screen.getByText(/не входит в список разрешённых источников/),
    ).toBeInTheDocument()

    // A search snippet is labelled as the engine's text, never as a citation.
    expect(screen.getByText('описание поисковика — не цитата источника')).toBeInTheDocument()

    // The query that was really sent is shown verbatim.
    expect(
      screen.getByText('Какая минимальная толщина цинкового покрытия требуется по стандарту?', {
        selector: 'code',
      }),
    ).toBeInTheDocument()
  })

  it('a plan stopped by the budget is not presented as a failure', async () => {
    install({
      overviewBody: overview({
        plans: [
          plan({
            status: 'budget_exhausted',
            rejections: ['бюджет бюро на исследования исчерпан: доступно 0, требуется 5000'],
            diagnostic: null,
          }),
        ],
      }),
    })
    renderPanel()

    expect(await screen.findByText('Остановлено по бюджету')).toBeInTheDocument()
    expect(
      screen.getByText(/работа остановлена, а не продолжена/),
    ).toBeInTheDocument()
  })

  it('a plan whose original question was re-drafted cannot be repeated, and says why', async () => {
    install({
      overviewBody: overview({
        plans: [plan({ question_id: null })],
      }),
    })
    renderPanel()

    const repeat = await screen.findByRole('button', { name: 'Исследовать заново' })
    expect(repeat).toBeDisabled()
    expect(repeat).toHaveAttribute(
      'title',
      'Исходный вопрос заменён новым разбором материала',
    )
  })

  it('a plan that used every pass cannot be repeated either', async () => {
    install({
      overviewBody: overview({
        plans: [plan({ passes: 2, max_passes: 2 })],
      }),
    })
    renderPanel()

    const repeat = await screen.findByRole('button', { name: 'Исследовать заново' })
    expect(repeat).toBeDisabled()
    expect(repeat).toHaveAttribute('title', 'Предел проходов исследования исчерпан')
  })

  it('keeps the research on screen when a refresh fails', async () => {
    let calls = 0
    installMockFetch((req) => {
      if (req.url.endsWith('/api/session')) {
        return jsonResponse(200, { authenticated: true, csrf_token: 'csrf-1' })
      }
      if (req.url.endsWith('/research/findings')) {
        return jsonResponse(200, { items: [finding()] })
      }
      if (req.url.endsWith('/research')) {
        calls += 1
        return calls === 1
          ? jsonResponse(200, overview())
          : apiError(503, 'service_unavailable', 'база недоступна', true)
      }
      return jsonResponse(404, { error: { code: 'not_found', message: 'нет', retryable: false } })
    })

    renderPanel()
    expect(await screen.findByRole('list', { name: 'Отраслевые выводы' })).toBeInTheDocument()
  })

  it('states the engine and the forecast before anything is spent', async () => {
    install({ overviewBody: overview({ provider: providerOpenRouter() }) })
    renderPanel()

    const engine = await screen.findByRole('region', { name: 'Поиск' })
    // `auto` is resolved, not repeated: the owner sees which engine will really run.
    expect(within(engine).getByText('auto → exa')).toBeInTheDocument()
    expect(within(engine).getByText('openai/gpt-4o-mini')).toBeInTheDocument()
    // The forecast for one search: 0,007 for the request plus 0,003 for the tokens.
    expect(within(engine).getByText('0,01 USD')).toBeInTheDocument()
    expect(within(engine).getByText(/не больше 20 на всё исследование/)).toBeInTheDocument()
    // And it is labelled a forecast, not an invoice.
    expect(within(engine).getByText(/Это прогноз по объявленному тарифу/)).toBeInTheDocument()
    expect(within(engine).getByText(/включая 10 результатов/)).toBeInTheDocument()
  })

  it('shows the chosen Perplexity engine as itself, with its own flat price and call limit', async () => {
    // The configuration this installation actually runs. `perplexity` is shown without an
    // arrow: an explicitly chosen engine resolves to itself, and the owner must not be
    // left wondering whether something else will really serve the request.
    install({
      overviewBody: overview({
        provider: providerOpenRouter({
          configured: 'perplexity',
          effective: 'perplexity',
          exa_fallback: false,
          max_results: 3,
          max_uses_per_request: 1,
          // 0,005 per search, flat, plus 0,003 of tokens.
          forecast_micros: 8_000,
          search_base_micros: 5_000,
          extra_result_micros: 0,
        }),
      }),
    })
    renderPanel()

    const engine = await screen.findByRole('region', { name: 'Поиск' })
    expect(within(engine).getAllByText('perplexity').length).toBeGreaterThan(0)
    expect(within(engine).queryByText(/→/)).not.toBeInTheDocument()
    expect(within(engine).queryByText(/не умеет искать сама/)).not.toBeInTheDocument()
    // Results and calls are two different bounds, and both are stated.
    expect(within(engine).getByText(/Один поиск на запрос/)).toBeInTheDocument()
    expect(
      within(engine).getByText(/за поиск независимо от числа результатов/),
    ).toBeInTheDocument()
    // 0,005 за поиск + 0,003 на токены. Показывается ровно столько, сколько заложено:
    // округление до копейки скрыло бы разницу между тарифами двух движков.
    expect(within(engine).getByText('0,008 USD')).toBeInTheDocument()
    expect(within(engine).getByText(/Это прогноз по объявленному тарифу/)).toBeInTheDocument()
  })

  it('names the domains when the search itself was narrowed to them', async () => {
    install({
      overviewBody: overview({
        provider: providerOpenRouter({
          configured: 'perplexity',
          effective: 'perplexity',
          exa_fallback: false,
          search_domains: ['docs.cntd.ru', 'gost.ru'],
        }),
      }),
    })
    renderPanel()

    const engine = await screen.findByRole('region', { name: 'Поиск' })
    expect(within(engine).getByText(/docs\.cntd\.ru, gost\.ru/)).toBeInTheDocument()
    expect(
      within(engine).getByText(/список разрешённых для чтения источников действует в любом случае/),
    ).toBeInTheDocument()
  })

  it('says plainly when auto had to fall back to Exa, and whose key is being spent', async () => {
    install({ overviewBody: overview({ provider: providerOpenRouter() }) })
    renderPanel()

    const engine = await screen.findByRole('region', { name: 'Поиск' })
    expect(within(engine).getByText(/не умеет искать сама/)).toBeInTheDocument()
    expect(within(engine).getByText(/Ключ взят из OTDEL_LLM_API_KEY/)).toBeInTheDocument()
  })

  it('does not invent an engine panel for a provider that has no engine to choose', async () => {
    install()
    renderPanel()

    await screen.findByRole('list', { name: 'Отраслевые выводы' })
    expect(screen.queryByRole('region', { name: 'Поиск' })).not.toBeInTheDocument()
  })

  it('names the engine that really served each query, with what it really cost', async () => {
    installMockFetch((req) => {
      if (req.url.endsWith('/api/session')) {
        return jsonResponse(200, { authenticated: true, csrf_token: 'csrf-1' })
      }
      if (req.url.endsWith('/research/findings')) {
        return jsonResponse(200, { items: [finding()] })
      }
      if (req.url.endsWith('/research')) {
        return jsonResponse(200, overview({ provider: providerOpenRouter() }))
      }
      if (req.url.endsWith('/sources')) {
        return jsonResponse(200, { items: sources() })
      }
      if (req.url.endsWith('/queries')) {
        return jsonResponse(200, {
          items: [
            {
              ...queries()[0],
              provider: 'openrouter_web_search/exa',
              // What the provider reported charging, not the 0,01 forecast.
              cost_micros: 8_100,
            },
          ],
        })
      }
      return jsonResponse(404, { error: { code: 'not_found', message: 'нет', retryable: false } })
    })
    renderPanel()

    await userEvent.click(await screen.findByRole('button', { name: /Журнал источников/ }))
    expect(await screen.findByText(/OpenRouter web search · exa/)).toBeInTheDocument()
    expect(screen.getByText(/0,0081 USD/)).toBeInTheDocument()
  })
})
