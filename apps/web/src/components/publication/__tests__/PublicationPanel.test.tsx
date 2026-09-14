import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type {
  KnowledgeVersion,
  RetrievalProviderState,
  ValidationOverview,
  ValidationRun,
  VersionClaim,
  VersionEvidence,
  VersionGap,
} from '../../../api/types'
import { AuthProvider } from '../../../auth/AuthContext'
import { apiError, installMockFetch, jsonResponse } from '../../../test/mockFetch'
import { PublicationPanel } from '../PublicationPanel'

const PARTNER = 'partner-a'
const VERSION = 'version-1'
const MATERIAL = 'material-1'
const MATERIAL_QUOTE = 'Минимальная толщина покрытия 55 мкм'
const EXTERNAL_QUOTE = 'Для класса 2 толщина покрытия не менее 55 мкм'
const EXTERNAL_URL = 'https://docs.example.org/gost-9-307'

function provider(overrides: Partial<RetrievalProviderState> = {}): RetrievalProviderState {
  return {
    state: 'needs_configuration',
    validation: {
      mode: 'deterministic',
      message: 'Проверка выполняется детерминированными правилами и всегда доступна.',
    },
    embedding: {
      state: 'needs_configuration',
      provider: 'openrouter',
      endpoint_host: null,
      model: null,
      message: 'Embedding-провайдер не настроен: векторы не считаются.',
    },
    answer: {
      state: 'needs_configuration',
      provider: 'openrouter',
      endpoint_host: null,
      model: null,
      message: 'Модель для прозаических ответов не настроена.',
    },
    vector: {
      state: 'no_embeddings',
      profile: null,
      message: 'Расширение pgvector установлено, embedding-провайдера нет.',
    },
    search_mode: 'keyword_only',
    missing: ['OTDEL_EMBEDDING_API_KEY', 'OTDEL_ANSWER_MODEL'],
    limits: {
      max_query_chars: 500,
      max_results: 20,
      max_answer_claims: 8,
      max_answer_chars: 2000,
      chunk_max_chars: 1200,
    },
    message:
      'Проверка и публикация работают. Прозаические ответы и векторный поиск не настроены.',
    ...overrides,
  }
}

function version(overrides: Partial<KnowledgeVersion> = {}): KnowledgeVersion {
  return {
    id: VERSION,
    partner_id: PARTNER,
    number: 1,
    status: 'published',
    validation_run_id: 'run-1',
    input_fingerprint: 'sha256:ab12',
    // Phase 1F records this alongside the verdict-sensitive fingerprint.
    candidate_fingerprint: 'sha256:cd34',
    claims_total: 3,
    claims_source_supported: 2,
    claims_hypothesis: 1,
    claims_unknown: 0,
    claims_conflicted: 0,
    claims_stale: 0,
    gaps_open: 1,
    chunks_total: 4,
    chunks_embedded: 0,
    embedding_profile: null,
    readiness: [
      {
        topic: 'product_description',
        state: 'ready',
        reason: 'описание продукта собрано из каталога партнёра',
      },
      {
        topic: 'audience_hypotheses',
        state: 'limited',
        reason: 'аудитория описана гипотезами, источник их не подтверждает',
      },
      {
        topic: 'characteristic_answers',
        state: 'ready',
        reason: 'характеристики подтверждены цитатами каталога',
      },
      {
        topic: 'commercial_answers',
        state: 'blocked',
        reason: 'в материалах нет ни цен, ни условий поставки',
      },
    ],
    blocked_reasons: [],
    created_at: '2026-01-01T09:00:00Z',
    published_at: '2026-01-01T10:00:00Z',
    superseded_at: null,
    revoked_at: null,
    revoked_reason: null,
    ...overrides,
  }
}

function run(overrides: Partial<ValidationRun> = {}): ValidationRun {
  return {
    id: 'run-1',
    partner_id: PARTNER,
    status: 'completed',
    prompt_profile: 'validator/2026-09-13.1',
    version_id: VERSION,
    version_number: 1,
    claims_considered: 4,
    claims_source_supported: 2,
    claims_hypothesis: 1,
    claims_unknown: 0,
    claims_conflicted: 0,
    claims_stale: 0,
    claims_rejected: 1,
    gaps_carried: 1,
    chunks_created: 4,
    chunks_embedded: 0,
    model_reviewed: 0,
    published: true,
    rejections: ['кандидат без цитаты не перенесён в версию'],
    blocked_reasons: [],
    diagnostic: null,
    started_at: '2026-01-01T09:59:00Z',
    finished_at: '2026-01-01T10:00:00Z',
    created_at: '2026-01-01T09:58:00Z',
    ...overrides,
  }
}

function materialEvidence(overrides: Partial<VersionEvidence> = {}): VersionEvidence {
  return {
    id: 'evidence-material-1',
    claim_id: 'claim-1',
    source_kind: 'material',
    material_id: MATERIAL,
    material_filename: 'catalogue.pdf',
    page_number: 3,
    region_id: 'region-1',
    url: null,
    host: null,
    retrieved_at: null,
    content_hash: null,
    quote: MATERIAL_QUOTE,
    char_start: 10,
    char_end: 10 + MATERIAL_QUOTE.length,
    ...overrides,
  }
}

function externalEvidence(overrides: Partial<VersionEvidence> = {}): VersionEvidence {
  return {
    id: 'evidence-external-1',
    claim_id: 'claim-2',
    source_kind: 'external',
    material_id: null,
    material_filename: null,
    page_number: null,
    region_id: null,
    url: EXTERNAL_URL,
    host: 'docs.example.org',
    retrieved_at: '2026-01-01T09:30:00Z',
    content_hash: 'a'.repeat(64),
    quote: EXTERNAL_QUOTE,
    char_start: 40,
    char_end: 40 + EXTERNAL_QUOTE.length,
    ...overrides,
  }
}

function supportedClaim(overrides: Partial<VersionClaim> = {}): VersionClaim {
  return {
    id: 'claim-1',
    version_id: VERSION,
    origin: 'partner_material',
    origin_id: 'fact-1',
    scope: 'partner',
    product_name: 'Профнастил С8',
    kind: 'characteristic',
    status: 'source_supported',
    attribute: 'минимальная толщина цинкового покрытия',
    value_text: '55',
    unit: 'мкм',
    conditions: null,
    model_context: null,
    check_note: 'значение найдено в цитате каталога как отдельный токен',
    evidence: [materialEvidence()],
    created_at: '2026-01-01T10:00:00Z',
    ...overrides,
  }
}

function hypothesisClaim(overrides: Partial<VersionClaim> = {}): VersionClaim {
  return {
    id: 'claim-2',
    version_id: VERSION,
    origin: 'industry_research',
    origin_id: 'finding-1',
    scope: 'industry',
    product_name: null,
    kind: 'characteristic',
    status: 'hypothesis',
    attribute: 'срок службы покрытия',
    value_text: '25 лет',
    unit: null,
    conditions: null,
    model_context: 'модель предположила срок по классу покрытия',
    check_note: 'источник называет класс, но не срок: значение осталось предположением',
    evidence: [externalEvidence()],
    created_at: '2026-01-01T10:00:00Z',
    ...overrides,
  }
}

function gap(overrides: Partial<VersionGap> = {}): VersionGap {
  return {
    id: 'gap-1',
    version_id: VERSION,
    origin_id: 'gap-source-1',
    product_name: null,
    topic: 'коммерческие условия',
    missing: 'в материалах нет ни цен, ни условий поставки',
    blocks: 'ответы о коммерческих условиях',
    blocks_topics: ['commercial_answers'],
    created_at: '2026-01-01T10:00:00Z',
    ...overrides,
  }
}

function overview(overrides: Partial<ValidationOverview> = {}): ValidationOverview {
  return {
    provider: provider(),
    published: version(),
    runs: [run()],
    versions: [version()],
    candidates: { facts: 6, findings: 2, gaps_open: 1, materials_drafted: 1 },
    ...overrides,
  }
}

interface Options {
  overviewBody?: ValidationOverview
  claimsBody?: VersionClaim[]
  gapsBody?: VersionGap[]
  onValidate?: () => Response
  onRetract?: () => Response
}

function install(options: Options = {}) {
  return installMockFetch((req) => {
    if (req.url.endsWith('/api/session')) {
      return jsonResponse(200, { authenticated: true, csrf_token: 'csrf-1' })
    }
    if (req.url.endsWith('/retrieval/provider')) {
      return jsonResponse(200, (options.overviewBody ?? overview()).provider)
    }
    if (req.url.endsWith('/validation')) {
      return jsonResponse(200, options.overviewBody ?? overview())
    }
    if (req.url.endsWith('/validate')) {
      return options.onValidate ? options.onValidate() : jsonResponse(200, run({ status: 'queued' }))
    }
    if (req.url.endsWith('/retract')) {
      return options.onRetract
        ? options.onRetract()
        : jsonResponse(200, version({ status: 'revoked', revoked_reason: 'новый прайс' }))
    }
    if (req.url.endsWith('/claims')) {
      return jsonResponse(200, { items: options.claimsBody ?? [supportedClaim(), hypothesisClaim()] })
    }
    if (req.url.endsWith('/gaps')) {
      return jsonResponse(200, { items: options.gapsBody ?? [gap()] })
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'нет', retryable: false } })
  })
}

function renderPanel() {
  return render(
    <AuthProvider>
      <PublicationPanel partnerId={PARTNER} />
    </AuthProvider>,
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('PublicationPanel', () => {
  it('does not present a claim that is not source-supported as confirmed', async () => {
    install()
    renderPanel()

    const list = await screen.findByRole('list', { name: 'Утверждения опубликованной версии' })
    const entries = within(list).getAllByRole('listitem')
    const hypothesis = entries.find((item) => item.getAttribute('data-status') === 'hypothesis')
    expect(hypothesis).toBeDefined()

    // Two independent markers, so the verdict survives being skimmed.
    expect(within(hypothesis!).getByText('гипотеза — источником не подтверждено')).toBeInTheDocument()
    expect(within(hypothesis!).getByText('не подтверждено источником')).toBeInTheDocument()
    expect(within(hypothesis!).queryByText('подтверждено источником')).toBeNull()

    // …and the server's own explanation of the verdict, verbatim.
    expect(
      within(hypothesis!).getByText(/источник называет класс, но не срок/),
    ).toBeInTheDocument()
  })

  it('words a source-supported claim as support by a source, not as verification', async () => {
    install()
    renderPanel()

    const list = await screen.findByRole('list', { name: 'Утверждения опубликованной версии' })
    const supported = within(list)
      .getAllByRole('listitem')
      .find((item) => item.getAttribute('data-status') === 'source_supported')

    expect(within(supported!).getByText('подтверждено источником')).toBeInTheDocument()
    expect(
      within(supported!).getByText(/Это не независимая проверка и не гарантия истинности/),
    ).toBeInTheDocument()
    expect(within(supported!).queryByText('не подтверждено источником')).toBeNull()
  })

  it('links a material citation to the original document at the cited page', async () => {
    install()
    renderPanel()

    const list = await screen.findByRole('list', { name: 'Утверждения опубликованной версии' })
    const link = within(list).getByRole('link', { name: 'catalogue.pdf, стр. 3' })

    expect(link).toHaveAttribute(
      'href',
      `/api/partners/${PARTNER}/materials/${MATERIAL}/original#page=3`,
    )
    expect(within(list).getByText(MATERIAL_QUOTE)).toBeInTheDocument()
  })

  it('marks an external citation as untrusted and says when it was read', async () => {
    install()
    renderPanel()

    const list = await screen.findByRole('list', { name: 'Утверждения опубликованной версии' })
    const link = within(list).getByRole('link', { name: 'docs.example.org' })

    expect(link).toHaveAttribute('href', EXTERNAL_URL)
    expect(link).toHaveAttribute('rel', expect.stringContaining('noreferrer'))
    expect(link).toHaveAttribute('rel', expect.stringContaining('nofollow'))
    expect(within(list).getByText(/прочитано/)).toBeInTheDocument()

    // The model's sentence stays structurally outside the quotation.
    const quote = within(list).getByText(EXTERNAL_QUOTE)
    const context = within(list).getByText(/модель предположила срок по классу покрытия/)
    expect(quote.closest('blockquote')).not.toBeNull()
    expect(context.closest('blockquote')).toBeNull()
  })

  it('shows all four readiness topics and says readiness is not a permission', async () => {
    install()
    renderPanel()

    const matrix = await screen.findByRole('list', {
      name: 'Готовность знаний по четырём темам',
    })
    expect(within(matrix).getAllByRole('listitem')).toHaveLength(4)

    expect(within(matrix).getByText('описание продукта')).toBeInTheDocument()
    expect(within(matrix).getByText('гипотезы аудитории')).toBeInTheDocument()
    expect(within(matrix).getByText('ответы о характеристиках')).toBeInTheDocument()
    expect(within(matrix).getByText('ответы о коммерческих условиях')).toBeInTheDocument()

    // The reason of each topic, verbatim.
    expect(within(matrix).getByText(/в материалах нет ни цен, ни условий поставки/)).toBeInTheDocument()

    expect(
      screen.getByText(
        /Готовность — это доступность знаний, а не разрешение на рассылку, не обещание совместимости и не принятое обязательство/,
      ),
    ).toBeInTheDocument()
  })

  it('shows a blocked version’s reasons verbatim and never as published', async () => {
    const blocked = version({
      id: 'version-2',
      number: 2,
      status: 'blocked',
      published_at: null,
      blocked_reasons: [
        'нет ни одного утверждения, подтверждённого источником, по теме «описание продукта»',
        'пробел «коммерческие условия» блокирует ответы о коммерческих условиях',
      ],
    })
    install({
      overviewBody: overview({
        published: null,
        versions: [blocked],
        runs: [
          run({
            status: 'partial',
            published: false,
            version_id: 'version-2',
            version_number: 2,
            blocked_reasons: ['правила готовности не выполнены'],
          }),
        ],
      }),
      claimsBody: [],
      gapsBody: [],
    })
    renderPanel()

    const history = await screen.findByRole('list', { name: 'История версий знаний' })
    expect(
      within(history).getByText('Не опубликована: правила готовности не выполнены'),
    ).toBeInTheDocument()
    expect(within(history).queryByText('Опубликована')).toBeNull()

    const reasons = within(history).getByRole('list', {
      name: 'Причины, по которым версия не опубликована',
    })
    expect(
      within(reasons).getByText(
        'нет ни одного утверждения, подтверждённого источником, по теме «описание продукта»',
      ),
    ).toBeInTheDocument()
    expect(
      within(reasons).getByText(
        'пробел «коммерческие условия» блокирует ответы о коммерческих условиях',
      ),
    ).toBeInTheDocument()

    // The check itself says the version was not published, in those words.
    expect(screen.getByText('версия не опубликована')).toBeInTheDocument()
  })

  it('refuses to retract until a reason is typed, then sends that reason', async () => {
    const user = userEvent.setup()
    const { calls } = install()
    renderPanel()

    const button = await screen.findByRole('button', { name: 'Отозвать версию' })
    expect(button).toBeDisabled()
    expect(button).toHaveAttribute(
      'title',
      'Укажите причину: отзыв без причины не отличить от сбоя',
    )
    expect(calls.some((call) => call.url.endsWith('/retract'))).toBe(false)

    await user.type(screen.getByLabelText('Причина отзыва версии'), 'партнёр прислал новый прайс')
    expect(button).toBeEnabled()
    await user.click(button)

    await waitFor(() => {
      const retract = calls.find((call) => call.url.endsWith('/retract'))
      expect(retract).toBeDefined()
      expect(retract!.method).toBe('POST')
      expect(retract!.body).toEqual({ reason: 'партнёр прислал новый прайс' })
    })
  })

  it('will not offer a check when the partner has no candidates, and says why', async () => {
    install({
      overviewBody: overview({
        published: null,
        versions: [],
        runs: [],
        candidates: { facts: 0, findings: 0, gaps_open: 0, materials_drafted: 0 },
      }),
      claimsBody: [],
      gapsBody: [],
    })
    renderPanel()

    const button = await screen.findByRole('button', { name: 'Проверить и опубликовать' })
    expect(button).toBeDisabled()
    expect(button).toHaveAttribute(
      'title',
      'Проверять нечего: у партнёра нет ни одного кандидата',
    )
  })

  it('shows the server’s own refusal when a check is refused', async () => {
    const user = userEvent.setup()
    install({
      onValidate: () =>
        apiError(409, 'conflict', 'у партнёра нет ни одного кандидата для проверки'),
    })
    renderPanel()

    await user.click(await screen.findByRole('button', { name: 'Проверить и опубликовать' }))

    expect(await screen.findByRole('alert')).toHaveTextContent(
      /у партнёра нет ни одного кандидата для проверки/,
    )
  })

  it('says verification needs no model at all while naming the missing variables', async () => {
    install()
    renderPanel()

    expect(
      await screen.findByText(/Модель для этого не нужна вовсе — правила детерминированы/),
    ).toBeInTheDocument()

    const missing = screen.getByRole('list', { name: 'Незаданные переменные окружения' })
    expect(within(missing).getByText('OTDEL_EMBEDDING_API_KEY')).toBeInTheDocument()
    expect(within(missing).getByText('OTDEL_ANSWER_MODEL')).toBeInTheDocument()
  })

  it('keeps the versions on screen when a refresh fails', async () => {
    let validationCalls = 0
    installMockFetch((req) => {
      if (req.url.endsWith('/api/session')) {
        return jsonResponse(200, { authenticated: true, csrf_token: 'csrf-1' })
      }
      if (req.url.endsWith('/retrieval/provider')) {
        return jsonResponse(200, provider())
      }
      if (req.url.endsWith('/claims')) {
        return jsonResponse(200, { items: [supportedClaim()] })
      }
      if (req.url.endsWith('/gaps')) {
        return jsonResponse(200, { items: [gap()] })
      }
      if (req.url.endsWith('/validation')) {
        validationCalls += 1
        return validationCalls === 1
          ? jsonResponse(200, overview())
          : apiError(503, 'service_unavailable', 'база недоступна', true)
      }
      return jsonResponse(404, { error: { code: 'not_found', message: 'нет', retryable: false } })
    })

    renderPanel()
    expect(
      await screen.findByRole('list', { name: 'Утверждения опубликованной версии' }),
    ).toBeInTheDocument()
  })
})
