import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type {
  AnswerResponse,
  RetrievalLimits,
  VersionClaim,
  VersionEvidence,
  VersionGap,
} from '../../../api/types'
import { AuthProvider } from '../../../auth/AuthContext'
import { installMockFetch, jsonResponse } from '../../../test/mockFetch'
import { AskPanel } from '../AskPanel'

const PARTNER = 'partner-a'
const VERSION = 'version-1'
const QUOTE = 'Минимальная толщина покрытия 55 мкм'
const QUESTION = 'какая минимальная толщина покрытия?'

function limits(): RetrievalLimits {
  return {
    max_query_chars: 500,
    max_results: 20,
    max_answer_claims: 8,
    max_answer_chars: 2000,
    chunk_max_chars: 1200,
  }
}

function evidence(overrides: Partial<VersionEvidence> = {}): VersionEvidence {
  return {
    id: 'evidence-1',
    claim_id: 'claim-1',
    source_kind: 'material',
    material_id: 'material-1',
    material_filename: 'catalogue.pdf',
    page_number: 3,
    region_id: null,
    url: null,
    host: null,
    retrieved_at: null,
    content_hash: null,
    quote: QUOTE,
    char_start: 0,
    char_end: QUOTE.length,
    ...overrides,
  }
}

function claim(overrides: Partial<VersionClaim> = {}): VersionClaim {
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
    evidence: [evidence()],
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

function answer(overrides: Partial<AnswerResponse> = {}): AnswerResponse {
  return {
    state: 'answered',
    version: { id: VERSION, number: 1, status: 'published', published_at: '2026-01-01T10:00:00Z' },
    mode: 'keyword',
    degraded: ['embedding-провайдер не настроен: векторная половина поиска не выполнялась'],
    text: 'Минимальная толщина цинкового покрытия — 55 мкм.',
    answer_is_model_context: true,
    claims: [claim()],
    citations: [evidence()],
    conditions: [],
    gaps: [],
    readiness: [],
    limitations: ['готовность по коммерческим условиям ограничена: цен в материалах нет'],
    rejections: [],
    message: 'Ответ составлен по одной опубликованной версии.',
    ...overrides,
  }
}

function install(body: AnswerResponse) {
  return installMockFetch((req) => {
    if (req.url.endsWith('/api/session')) {
      return jsonResponse(200, { authenticated: true, csrf_token: 'csrf-1' })
    }
    if (req.url.endsWith('/retrieval/answer')) {
      return jsonResponse(200, body)
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'нет', retryable: false } })
  })
}

async function ask() {
  const user = userEvent.setup()
  render(
    <AuthProvider>
      <AskPanel partnerId={PARTNER} limits={limits()} />
    </AuthProvider>,
  )
  await user.type(screen.getByLabelText('Вопрос'), QUESTION)
  await user.click(screen.getByRole('button', { name: 'Спросить' }))
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('AskPanel', () => {
  it('marks an answered response as the model’s wording and shows its citations', async () => {
    install(answer())
    await ask()

    expect(await screen.findByText('ответ составлен моделью')).toBeInTheDocument()

    // The prose sits under the badge that says what it is…
    const prose = screen.getByText('Минимальная толщина цинкового покрытия — 55 мкм.')
    expect(screen.getByText('формулировка модели — не цитата источника')).toBeInTheDocument()
    // …and is structurally not a quotation.
    expect(prose.closest('blockquote')).toBeNull()

    // The citations the answer stands on are on screen, with the way back to the page.
    expect(screen.getAllByText(QUOTE).length).toBeGreaterThan(0)
    expect(
      screen.getAllByRole('link', { name: 'catalogue.pdf, стр. 3' })[0],
    ).toHaveAttribute('href', `/api/partners/${PARTNER}/materials/material-1/original#page=3`)

    // The pinned version is named, so two answers cannot be silently mixed.
    expect(screen.getByText(/закреплённая версия 1 · Опубликована/)).toBeInTheDocument()
  })

  it('shows limitations verbatim instead of reading as a commercial clearance', async () => {
    install(answer())
    await ask()

    const limitations = await screen.findByRole('list', { name: 'Оговорки к ответу' })
    expect(
      within(limitations).getByText(
        'готовность по коммерческим условиям ограничена: цен в материалах нет',
      ),
    ).toBeInTheDocument()
  })

  it('gives no prose and names the gap when the evidence is insufficient', async () => {
    install(
      answer({
        state: 'insufficient_evidence',
        text: null,
        answer_is_model_context: false,
        claims: [],
        citations: [],
        gaps: [gap()],
        limitations: [],
        message: 'В опубликованной версии нет утверждений по этому вопросу.',
      }),
    )
    await ask()

    expect(await screen.findByText('ответа нет: подходящих утверждений не нашлось')).toBeInTheDocument()
    expect(screen.getByText(/Догадка сюда не подставляется|догадка не подставляется/i)).toBeInTheDocument()

    // No prose at all, and no quotation pretending to be one.
    expect(screen.queryByText('формулировка модели — не цитата источника')).toBeNull()
    expect(screen.queryByRole('list', { name: 'Утверждения, найденные для ответа' })).toBeNull()

    // The gap is named rather than left as an empty result.
    const gaps = screen.getByRole('list', { name: 'Пробелы знаний в версии' })
    expect(within(gaps).getByText('в материалах нет ни цен, ни условий поставки')).toBeInTheDocument()
    // …together with which readiness this gap holds back.
    expect(within(gaps).getByText('ограничивает готовность:')).toBeInTheDocument()
    expect(within(gaps).getAllByText(/ответы о коммерческих условиях/).length).toBeGreaterThan(0)
  })

  it('says plainly that no prose was composed when only evidence is available', async () => {
    install(
      answer({
        state: 'evidence_only',
        text: null,
        answer_is_model_context: false,
        rejections: ['модель для ответов не настроена: прозаический ответ не составлялся'],
        message: 'Найдены утверждения с цитатами; связного ответа нет.',
      }),
    )
    await ask()

    expect(
      await screen.findByText(
        'прозаический ответ не составлен — показаны найденные утверждения',
      ),
    ).toBeInTheDocument()
    expect(screen.getByText(/Связного ответа не составлено/)).toBeInTheDocument()
    expect(screen.queryByText('формулировка модели — не цитата источника')).toBeNull()

    // The found statements are still shown, with their citations.
    expect(
      screen.getByRole('list', { name: 'Утверждения, найденные для ответа' }),
    ).toBeInTheDocument()

    const rejections = screen.getByRole('list', { name: 'Отклонённое при составлении ответа' })
    expect(
      within(rejections).getByText(
        'модель для ответов не настроена: прозаический ответ не составлялся',
      ),
    ).toBeInTheDocument()
  })

  it('presents a partner with nothing published as a named state, not an error', async () => {
    install(
      answer({
        state: 'no_published_version',
        version: null,
        text: null,
        answer_is_model_context: false,
        claims: [],
        citations: [],
        gaps: [],
        limitations: [],
        message: 'У партнёра нет опубликованной версии знаний.',
      }),
    )
    await ask()

    expect(await screen.findByText('у партнёра нет опубликованной версии')).toBeInTheDocument()
    expect(screen.getByText(/Это состояние, а не ошибка/)).toBeInTheDocument()
    expect(
      screen.getByText('версия не закреплена: у партнёра нет опубликованной версии'),
    ).toBeInTheDocument()

    // Nothing on the screen is an error: no alert is raised for this state.
    expect(screen.queryByRole('alert')).toBeNull()
  })
})
