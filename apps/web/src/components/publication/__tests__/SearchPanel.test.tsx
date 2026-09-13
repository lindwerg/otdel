import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type {
  RetrievalLimits,
  SearchResponse,
  VersionClaim,
  VersionEvidence,
} from '../../../api/types'
import { AuthProvider } from '../../../auth/AuthContext'
import { installMockFetch, jsonResponse } from '../../../test/mockFetch'
import { SearchPanel } from '../SearchPanel'

const PARTNER = 'partner-a'
const VERSION = 'version-1'
const QUOTE = 'Минимальная толщина покрытия 55 мкм'
const NO_EMBEDDINGS =
  'embedding-провайдер не настроен: векторная половина поиска не выполнялась'

function limits(): RetrievalLimits {
  return {
    max_query_chars: 500,
    max_results: 20,
    max_answer_claims: 8,
    max_answer_chars: 2000,
    chunk_max_chars: 1200,
  }
}

function evidence(): VersionEvidence {
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

function response(overrides: Partial<SearchResponse> = {}): SearchResponse {
  return {
    state: 'ok',
    version: { id: VERSION, number: 1, status: 'published', published_at: '2026-01-01T10:00:00Z' },
    mode: 'keyword',
    degraded: [NO_EMBEDDINGS],
    items: [{ claim: claim(), score: 0.82, matched_by: ['exact', 'keyword'] }],
    gaps: [],
    message: 'Найдено в опубликованной версии 1.',
    ...overrides,
  }
}

function install(body: SearchResponse) {
  return installMockFetch((req) => {
    if (req.url.endsWith('/api/session')) {
      return jsonResponse(200, { authenticated: true, csrf_token: 'csrf-1' })
    }
    if (req.url.endsWith('/retrieval/search')) {
      return jsonResponse(200, body)
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'нет', retryable: false } })
  })
}

async function search() {
  const user = userEvent.setup()
  const rendered = render(
    <AuthProvider>
      <SearchPanel partnerId={PARTNER} limits={limits()} />
    </AuthProvider>,
  )
  await user.type(screen.getByLabelText('Запрос'), 'толщина покрытия')
  await user.click(screen.getByRole('button', { name: 'Найти' }))
  return rendered
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('SearchPanel', () => {
  it('says keyword-only search is keyword-only and why, instead of looking complete', async () => {
    install(response())
    await search()

    expect(await screen.findByText('режим: только по ключевым словам')).toBeInTheDocument()

    const degraded = screen.getByRole('list', { name: 'Почему режим поиска неполный' })
    expect(within(degraded).getByText(NO_EMBEDDINGS)).toBeInTheDocument()

    // No claim of a full hybrid search anywhere on the result.
    expect(screen.queryByText(/поиск по ключевым словам и по смыслу/)).toBeNull()
  })

  it('says why each result matched, without a confidence percentage', async () => {
    install(response())
    await search()

    const hits = await screen.findByRole('list', { name: 'Найденные утверждения' })
    expect(within(hits).getByText('точное совпадение, по ключевым словам')).toBeInTheDocument()
    // `score` is comparable only inside one response, so it is never shown as a percentage.
    expect(within(hits).queryByText(/%/)).toBeNull()
  })

  it('names the version every result came from', async () => {
    install(response())
    await search()

    expect(await screen.findByText('версия 1 · Опубликована')).toBeInTheDocument()
  })

  it('presents a partner with nothing published as a named state, not an empty list', async () => {
    install(
      response({
        state: 'no_published_version',
        version: null,
        items: [],
        message: 'У партнёра нет опубликованной версии: искать не по чему.',
      }),
    )
    await search()

    expect(await screen.findByText('у партнёра нет опубликованной версии')).toBeInTheDocument()
    expect(screen.getByText(/искать не по чему/)).toBeInTheDocument()
    expect(screen.queryByRole('list', { name: 'Найденные утверждения' })).toBeNull()
    expect(screen.queryByRole('alert')).toBeNull()
  })

  it('sends the query in the body, never in the URL', async () => {
    const { calls } = install(response())
    await search()

    const request = calls.find((call) => call.url.endsWith('/retrieval/search'))
    expect(request).toBeDefined()
    expect(request!.method).toBe('POST')
    expect(request!.body).toEqual({ query: 'толщина покрытия' })
    // A POST body is CSRF-protected; the text never reaches a log or history.
    expect(request!.headers['x-csrf-token']).toBe('csrf-1')
    expect(request!.url).not.toContain('толщина')
  })
})
