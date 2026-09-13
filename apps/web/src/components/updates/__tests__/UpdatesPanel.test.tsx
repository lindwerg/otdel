import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type {
  HistoryEvent,
  RefreshPlan,
  RefreshStatus,
  RetentionPolicy,
  SourceRefresh,
} from '../../../api/types'
import { AuthProvider } from '../../../auth/AuthContext'
import { installMockFetch, jsonResponse } from '../../../test/mockFetch'
import { UpdatesPanel } from '../UpdatesPanel'

const PARTNER = 'partner-a'
const MATERIAL = 'material-1'

function source(overrides: Partial<SourceRefresh> = {}): SourceRefresh {
  return {
    material_id: MATERIAL,
    filename: 'catalogue.pdf',
    material_status: 'completed',
    state: 'drafted',
    content_revision: 1,
    drafted_revision: 1,
    draft_status: 'completed',
    facts_drafted: 3,
    claims_in_published: 3,
    message: 'разобран по текущему чтению №1: фактов 3, из них в опубликованной версии 3',
    ...overrides,
  }
}

function status(overrides: Partial<RefreshStatus> = {}): RefreshStatus {
  return {
    state: 'current',
    published: { id: 'version-1', number: 1, status: 'published', published_at: '2026-01-01T10:00:00Z' },
    latest: { id: 'version-1', number: 1, status: 'published', published_at: '2026-01-01T10:00:00Z' },
    reasons: [],
    sources: [source()],
    candidate_fingerprint: 'a'.repeat(64),
    published_candidate_fingerprint: 'a'.repeat(64),
    checking: false,
    message: 'Опубликованная версия построена из тех же кандидатов, что есть сейчас.',
    computed_at: '2026-01-02T10:00:00Z',
    ...overrides,
  }
}

function event(overrides: Partial<HistoryEvent> = {}): HistoryEvent {
  return {
    id: 'event-1',
    partner_id: PARTNER,
    kind: 'version_published',
    actor: 'worker',
    material_id: null,
    version_id: 'version-1',
    job_id: null,
    run_id: null,
    summary: 'опубликована версия 1: утверждений 3, из них подтверждено источником 2',
    detail: { number: 1 },
    occurred_at: '2026-01-01T10:00:00Z',
    ...overrides,
  }
}

function retention(overrides: Partial<RetentionPolicy> = {}): RetentionPolicy {
  return {
    state: 'keep_everything',
    event_days: null,
    job_days: null,
    keep_per_kind: 20,
    sweep_interval_seconds: 3600,
    preview: {
      events_prunable: 0,
      jobs_prunable: 0,
      events_total: 12,
      jobs_total: 4,
      oldest_event: '2026-01-01T09:00:00Z',
    },
    protected: [
      'Опубликованная версия знаний и её снимок не удаляются никогда.',
      'Оригиналы материалов и их страницы не удаляются: на них ссылаются цитаты.',
    ],
    last_sweep: null,
    message: 'Очистка выключена: журнал событий и завершённые задания хранятся без ограничения срока.',
    ...overrides,
  }
}

interface Options {
  status?: RefreshStatus
  events?: HistoryEvent[]
  retention?: RetentionPolicy
  plan?: RefreshPlan
  refreshStatus?: number
}

function install(options: Options = {}) {
  return installMockFetch((req) => {
    if (req.url.endsWith('/api/session')) {
      return jsonResponse(200, { authenticated: true, csrf_token: 'csrf-1' })
    }
    if (req.url.endsWith('/refresh') && req.method === 'POST') {
      if (options.plan) return jsonResponse(options.refreshStatus ?? 200, options.plan)
      return jsonResponse(409, {
        error: { code: 'conflict', message: 'проверять нечего', retryable: false },
      })
    }
    if (req.url.endsWith('/refresh')) {
      return jsonResponse(200, options.status ?? status())
    }
    if (req.url.includes('/events')) {
      return jsonResponse(200, { items: options.events ?? [event()] })
    }
    if (req.url.endsWith('/api/retention')) {
      return jsonResponse(200, options.retention ?? retention())
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'нет', retryable: false } })
  })
}

function show(options: Options = {}) {
  install(options)
  return render(
    <AuthProvider>
      <UpdatesPanel partnerId={PARTNER} />
    </AuthProvider>,
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('UpdatesPanel', () => {
  it('says the published version matches the current candidates, without calling it "актуально"', async () => {
    show()

    expect(
      await screen.findByText('Опубликованная версия построена из текущих кандидатов'),
    ).toBeInTheDocument()
    expect(screen.getByText('Сейчас опубликована версия 1.')).toBeInTheDocument()
    // No reason list when there is nothing out of date.
    expect(screen.queryByRole('list', { name: 'Причины, по которым нужна перепроверка' })).toBeNull()
  })

  it('names the document a reason is about, with both reading numbers', async () => {
    show({
      status: status({
        state: 'revalidation_required',
        message: 'Нужна перепроверка: причин — 1.',
        sources: [
          source({ state: 'reread_after_draft', content_revision: 2, drafted_revision: 1 }),
        ],
        reasons: [
          {
            code: 'source_reread',
            message:
              'разбор сделан по чтению №1, документ прочитан заново (№2): кандидаты описывают прежний текст',
            material_id: MATERIAL,
            material_filename: 'catalogue.pdf',
            version_id: null,
            version_number: null,
            content_revision: 2,
            drafted_revision: 1,
          },
        ],
      }),
    })

    const reasons = await screen.findByRole('list', {
      name: 'Причины, по которым нужна перепроверка',
    })
    expect(within(reasons).getByText(/документ прочитан заново/)).toBeInTheDocument()
    expect(
      within(reasons).getByText('источник: catalogue.pdf (разбор по чтению №1, сейчас №2)'),
    ).toBeInTheDocument()
    // The published version is still named as published: a stale draft does not
    // take it away.
    expect(screen.getByText('Сейчас опубликована версия 1.')).toBeInTheDocument()
  })

  it('never shows a percentage or an estimate for server work', async () => {
    show({
      status: status({
        state: 'checking',
        checking: true,
        message: 'Проверка идёт. Опубликованная версия остаётся прежней.',
      }),
    })

    expect(await screen.findByText('Проверка идёт')).toBeInTheDocument()
    expect(screen.queryByText(/%/)).toBeNull()
    expect(screen.queryByRole('progressbar')).toBeNull()
    // And the button does not offer to start a second check.
    expect(screen.getByRole('button', { name: 'Обновить' })).toBeDisabled()
  })

  it('reports a stage that cannot run instead of hiding it', async () => {
    const plan: RefreshPlan = {
      steps: [
        {
          kind: 'understanding',
          outcome: 'needs_provider',
          material_id: MATERIAL,
          material_filename: 'catalogue.pdf',
          job_id: null,
          message:
            '«catalogue.pdf» ждёт разбора, но модель продуктолога не настроена: не заданы OTDEL_LLM_API_KEY, OTDEL_LLM_MODEL',
        },
        {
          kind: 'validation',
          outcome: 'waiting',
          material_id: null,
          material_filename: null,
          job_id: null,
          message: 'проверять нечего: кандидатов нет',
        },
      ],
      queued: 0,
      message: 'Ничего запускать не потребовалось. Причины по каждому шагу — ниже.',
      requested_at: '2026-01-02T10:00:00Z',
    }
    show({ status: status({ state: 'revalidation_required' }), plan })

    const user = userEvent.setup()
    await user.click(await screen.findByRole('button', { name: 'Обновить' }))

    const steps = await screen.findByRole('list', { name: 'Что было запущено и что нет' })
    expect(within(steps).getByText('Нужна настройка')).toBeInTheDocument()
    expect(within(steps).getByText(/OTDEL_LLM_API_KEY/)).toBeInTheDocument()
    expect(screen.getByText(/поставлено в очередь шагов: 0/)).toBeInTheDocument()
    expect(within(steps).queryByText('Поставлено в очередь')).toBeNull()
  })

  it('shows the server refusal of a refresh instead of inventing one', async () => {
    show({ status: status({ state: 'never_published' }) })

    const user = userEvent.setup()
    await user.click(await screen.findByRole('button', { name: 'Обновить' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('проверять нечего')
  })

  it('shows the history as the server wrote it, with no edit or delete offered', async () => {
    show({
      events: [
        event(),
        event({
          id: 'event-2',
          kind: 'version_retracted',
          actor: 'owner',
          summary: 'версия 1 отозвана: нашли опечатку. Снимок сохранён как история',
        }),
      ],
    })

    const log = await screen.findByRole('list', { name: 'Журнал событий партнёра' })
    expect(within(log).getByText(/опубликована версия 1/)).toBeInTheDocument()
    expect(within(log).getByText(/версия 1 отозвана: нашли опечатку/)).toBeInTheDocument()
    expect(within(log).getByText('владелец')).toBeInTheDocument()
    expect(within(log).getByText('обработчик')).toBeInTheDocument()
    // An append-only log offers no way to change itself.
    expect(within(log).queryByRole('button')).toBeNull()
  })

  it('states what retention will never remove, not only how long it keeps things', async () => {
    show()

    expect(await screen.findByText(/Очистка выключена/)).toBeInTheDocument()
    const protectedList = screen.getByRole('list', { name: 'Что защищено от очистки' })
    expect(
      within(protectedList).getByText(/Опубликованная версия знаний и её снимок не удаляются никогда/),
    ).toBeInTheDocument()
    expect(screen.getByText('Очистка не выполняется: она выключена.')).toBeInTheDocument()
  })

  it('does not report a configured sweep that removed nothing as one that never ran', async () => {
    // A sweep records itself only when it removed something. Turning that silence into
    // «ещё ни разу не выполнялась» would be a state nobody computed: with a policy
    // configured the sweep runs hourly and usually finds nothing to do.
    show({
      retention: retention({
        state: 'enabled',
        event_days: 90,
        job_days: 30,
        last_sweep: null,
        message: 'Журнал событий хранится 90 дн., завершённые задания — 30 дн.',
      }),
    })

    expect(await screen.findByText(/выполняется по расписанию/)).toBeInTheDocument()
    expect(screen.queryByText(/ни разу не выполнялась/)).toBeNull()
    expect(screen.queryByText(/не выполняется: она выключена/)).toBeNull()
  })

  it('does not offer an export when nothing is published', async () => {
    show({ status: status({ state: 'never_published', published: null, latest: null }) })

    expect(
      await screen.findByRole('button', { name: 'Выгрузить опубликованную версию' }),
    ).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Показать изменения версии' })).toBeDisabled()
  })
})
