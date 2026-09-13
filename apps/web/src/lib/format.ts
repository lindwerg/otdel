import type {
  AnswerState,
  ChangeCounts,
  ChangeKind,
  ClaimStatus,
  EventActor,
  EventKind,
  ExtractionSummary,
  FactKind,
  KnowledgeFact,
  KnowledgeRunStatus,
  KnowledgeVersion,
  MatchKind,
  MaterialStatus,
  PageStatus,
  QueryOutcome,
  QuestionAudience,
  ReadinessState,
  ReadinessTopic,
  RefreshState,
  RefreshStepKind,
  RefreshStepOutcome,
  ResearchPlanStatus,
  SearchMode,
  SearchState,
  SourceState,
  SourceStatus,
  TextSource,
  ValidationRunStatus,
  VersionClaim,
  VersionStatus,
} from '../api/types'

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '—'
  if (bytes < 1024) return `${bytes} Б`
  const units = ['КиБ', 'МиБ', 'ГиБ']
  let value = bytes / 1024
  let unitIndex = 0
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024
    unitIndex += 1
  }
  const rounded = value >= 10 ? Math.round(value) : Math.round(value * 10) / 10
  return `${rounded} ${units[unitIndex]}`
}

const dateTimeFormatter = new Intl.DateTimeFormat('ru-RU', {
  dateStyle: 'medium',
  timeStyle: 'short',
})

export function formatDateTime(iso: string): string {
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return iso
  return dateTimeFormatter.format(date)
}

export interface StatusPresentation {
  label: string
  /** Calm, non-alarming default explanation shown when the server gives no `error`. */
  defaultHint: string
  tone: 'neutral' | 'progress' | 'success' | 'warn' | 'error'
}

// Russian labels, deliberately calm (no exclamation marks) per
// docs/block-01-design.md §6 and docs/implementation-contract.md's note that
// "queued" means waiting to be processed, not that the file was read yet.
const MATERIAL_STATUS: Record<MaterialStatus, StatusPresentation> = {
  queued: {
    label: 'В очереди',
    defaultHint: 'Ожидает обработки. Файл ещё не прочитан.',
    tone: 'neutral',
  },
  processing: {
    label: 'Обрабатывается',
    defaultHint: 'Извлечение содержимого выполняется.',
    tone: 'progress',
  },
  completed: {
    label: 'Прочитано',
    defaultHint: 'Все страницы обработаны.',
    tone: 'success',
  },
  partial: {
    label: 'Прочитано частично',
    defaultHint: 'Часть страниц не удалось прочитать.',
    tone: 'warn',
  },
  failed: {
    label: 'Ошибка обработки',
    defaultHint: 'Обработка не выполнена.',
    tone: 'error',
  },
  quarantined: {
    label: 'Заблокировано проверкой',
    defaultHint: 'Файл не прошёл проверку безопасности и не будет обработан.',
    tone: 'error',
  },
}

export function materialStatusPresentation(status: MaterialStatus): StatusPresentation {
  return (
    MATERIAL_STATUS[status] ?? {
      label: status,
      defaultHint: '',
      tone: 'neutral',
    }
  )
}

/**
 * Statuses the server actually accepts for a material retry.
 *
 * `quarantined` is deliberately absent: the backend refuses it (the content
 * itself was rejected, so repeating cannot change the outcome), and offering a
 * button that can only ever fail is worse than offering none.
 */
export const RETRYABLE_MATERIAL_STATUSES: MaterialStatus[] = ['failed', 'partial']

// --- Phase 1B: pages -------------------------------------------------------

/**
 * Per-page labels. Wording matters here more than anywhere else in the app:
 * `needs_ocr` must not read as a failure of the document and must never read as
 * success, and `empty` is reserved for a page that genuinely holds nothing.
 */
const PAGE_STATUS: Record<PageStatus, StatusPresentation> = {
  pending: {
    label: 'Ожидает чтения',
    defaultHint: 'Страница учтена, но ещё не прочитана.',
    tone: 'neutral',
  },
  extracted: {
    label: 'Прочитана',
    defaultHint: 'Содержимое страницы извлечено.',
    tone: 'success',
  },
  empty: {
    label: 'Пустая',
    defaultHint: 'На странице нет ни текста, ни изображений.',
    tone: 'neutral',
  },
  needs_ocr: {
    label: 'Нужно распознавание',
    defaultHint: 'Текстового слоя нет; распознавание не выполнено.',
    tone: 'warn',
  },
  partial: {
    label: 'Прочитана частично',
    defaultHint: 'Часть содержимого страницы осталась непрочитанной.',
    tone: 'warn',
  },
  failed: {
    label: 'Ошибка чтения',
    defaultHint: 'Страницу не удалось прочитать.',
    tone: 'error',
  },
}

export function pageStatusPresentation(status: PageStatus): StatusPresentation {
  return PAGE_STATUS[status] ?? { label: status, defaultHint: '', tone: 'neutral' }
}

/** Page statuses the server accepts for a single-page retry. */
export const RETRYABLE_PAGE_STATUSES: PageStatus[] = [
  'pending',
  'needs_ocr',
  'partial',
  'failed',
]

const TEXT_SOURCE_LABEL: Record<TextSource, string> = {
  none: 'текст не получен',
  text_layer: 'из текстового слоя',
  ocr: 'распознаванием',
}

export function textSourceLabel(source: TextSource): string {
  return TEXT_SOURCE_LABEL[source] ?? source
}

/**
 * Counts of page outcomes, as a plain sentence.
 *
 * Deliberately counts rather than a percentage: a share of pages read is not a
 * measure of how much of the document is understood, and a progress bar with an
 * invented estimate is exactly what docs/block-01-spec.md §11 forbids.
 */
export function extractionCountsLine(summary: ExtractionSummary): string {
  const parts: string[] = []
  const add = (count: number, label: string) => {
    if (count > 0) parts.push(`${count} ${label}`)
  }
  add(summary.pages_extracted, 'прочитано')
  add(summary.pages_partial, 'частично')
  add(summary.pages_needs_ocr, 'ждут распознавания')
  add(summary.pages_empty, 'пустых')
  add(summary.pages_failed, 'с ошибкой')
  add(summary.pages_pending, 'в очереди')

  if (parts.length === 0) {
    return `Страниц: ${summary.pages_total}. Ни одна ещё не прочитана.`
  }
  return `Страниц: ${summary.pages_total} — ${parts.join(', ')}.`
}

/**
 * Which tools produced this result. Returns `null` when nothing is recorded, so
 * the interface stays silent rather than claiming an engine that never ran.
 */
export function extractionToolsLine(summary: ExtractionSummary): string | null {
  const parts: string[] = []
  if (summary.parser_name) {
    parts.push(`разбор: ${summary.parser_name} ${summary.parser_version ?? ''}`.trim())
  }
  if (summary.ocr_engine) {
    parts.push(`распознавание: ${summary.ocr_version ?? summary.ocr_engine}`)
  }
  return parts.length > 0 ? parts.join(' · ') : null
}

// --- Phase 1C: the product draft -------------------------------------------

/**
 * Labels for an understanding run.
 *
 * `needs_provider` is deliberately not an error: nothing failed, the model
 * adapter simply has not been configured yet. Calling it "ошибка" would send
 * the owner looking for a problem in the material.
 */
const RUN_STATUS: Record<KnowledgeRunStatus, StatusPresentation> = {
  queued: {
    label: 'В очереди на разбор',
    defaultHint: 'Материал прочитан и ждёт продуктолога.',
    tone: 'neutral',
  },
  running: {
    label: 'Разбирается',
    defaultHint: 'Продуктолог читает страницы материала.',
    tone: 'progress',
  },
  completed: {
    label: 'Разобран',
    defaultHint: 'Все предложения модели подтверждены источником.',
    tone: 'success',
  },
  partial: {
    label: 'Разобран частично',
    defaultHint: 'Часть предложений модели отклонена — причины ниже.',
    tone: 'warn',
  },
  failed: {
    label: 'Разбор не выполнен',
    defaultHint: 'Черновик не создан.',
    tone: 'error',
  },
  needs_provider: {
    label: 'Ожидает настройки модели',
    defaultHint: 'Ключ провайдера не задан: обращений к модели не было.',
    tone: 'warn',
  },
}

export function runStatusPresentation(status: KnowledgeRunStatus): StatusPresentation {
  return RUN_STATUS[status] ?? { label: status, defaultHint: '', tone: 'neutral' }
}

const FACT_KIND_LABEL: Record<FactKind, string> = {
  characteristic: 'характеристика',
  limitation: 'ограничение',
  application: 'применение',
  commercial: 'коммерческое условие',
}

export function factKindLabel(kind: FactKind): string {
  return FACT_KIND_LABEL[kind] ?? kind
}

const AUDIENCE_LABEL: Record<QuestionAudience, string> = {
  partner: 'вопрос партнёру',
  industry: 'вопрос для отраслевого исследования',
}

export function questionAudienceLabel(audience: QuestionAudience): string {
  return AUDIENCE_LABEL[audience] ?? audience
}

/**
 * The value with its unit, exactly as recorded.
 *
 * The unit is appended only when the server stored one — it does that only when
 * the unit is literally written in the source — so a bare number stays a bare
 * number instead of acquiring a plausible unit here.
 */
export function factValueLine(fact: KnowledgeFact): string {
  return fact.unit ? `${fact.value_text} ${fact.unit}` : fact.value_text
}

/** Where a quotation comes from, as a sentence: «каталог.pdf, стр. 3». */
export function evidenceSourceLine(filename: string, pageNumber: number): string {
  return `${filename}, стр. ${pageNumber}`
}

// --- Phase 1D: bounded industry research -----------------------------------

/**
 * Labels for a research plan.
 *
 * Three of these are deliberately not errors. `needs_provider` means nothing is
 * configured — no request was made and no money was reserved. `budget_exhausted`
 * means the run stopped where it was told to stop, which is the feature working.
 * `cancelled` means the owner pressed stop. Calling any of them "ошибка" would
 * send somebody looking for a problem that is not there.
 */
const PLAN_STATUS: Record<ResearchPlanStatus, StatusPresentation> = {
  queued: {
    label: 'В очереди на исследование',
    defaultHint: 'Вопрос утверждён и ждёт исследователя.',
    tone: 'neutral',
  },
  running: {
    label: 'Исследуется',
    defaultHint: 'Исследователь ищет и читает источники.',
    tone: 'progress',
  },
  completed: {
    label: 'Исследовано',
    defaultHint: 'Все найденные источники прочитаны, выводы подтверждены цитатами.',
    tone: 'success',
  },
  partial: {
    label: 'Исследовано частично',
    defaultHint: 'Часть источников не прочитана или часть выводов отклонена — причины ниже.',
    tone: 'warn',
  },
  failed: {
    label: 'Исследование не выполнено',
    defaultHint: 'Выводы не получены.',
    tone: 'error',
  },
  needs_provider: {
    label: 'Ожидает настройки исследователя',
    defaultHint: 'Поиск не настроен: внешних запросов не было, бюджет не расходовался.',
    tone: 'warn',
  },
  budget_exhausted: {
    label: 'Остановлено по бюджету',
    defaultHint: 'Деньги на исследования закончились; работа остановлена, а не продолжена.',
    tone: 'warn',
  },
  cancelled: {
    label: 'Остановлено владельцем',
    defaultHint: 'Исследование прервано по вашей команде.',
    tone: 'neutral',
  },
}

export function planStatusPresentation(status: ResearchPlanStatus): StatusPresentation {
  return PLAN_STATUS[status] ?? { label: status, defaultHint: '', tone: 'neutral' }
}

/**
 * What became of one discovered URL.
 *
 * Every value except `fetched` means *no content was obtained*, and each names a
 * different reason. A journal that said only "не прочитано" would be useless,
 * and one that silently dropped the row would make the search look empty.
 */
const SOURCE_STATUS: Record<SourceStatus, StatusPresentation> = {
  discovered: {
    label: 'Найдено, не читалось',
    defaultHint: 'Ссылка получена от поиска; страница ещё не загружалась.',
    tone: 'neutral',
  },
  skipped_host: {
    label: 'Хост не разрешён',
    defaultHint: 'Этот сайт не входит в список разрешённых источников.',
    tone: 'neutral',
  },
  skipped_robots: {
    label: 'Запрещено robots.txt',
    defaultHint: 'Сайт просит не читать эту страницу автоматически.',
    tone: 'neutral',
  },
  skipped_limit: {
    label: 'Не вошло в лимит',
    defaultHint: 'Предел страниц, времени или бюджета исчерпан до этой ссылки.',
    tone: 'warn',
  },
  skipped_type: {
    label: 'Формат не читается',
    defaultHint: 'На этом этапе читаются только HTML и текст.',
    tone: 'neutral',
  },
  fetched: {
    label: 'Прочитано',
    defaultHint: 'Страница загружена; сохранены снимок текста и хеш содержимого.',
    tone: 'success',
  },
  failed: {
    label: 'Ошибка загрузки',
    defaultHint: 'Страницу не удалось прочитать.',
    tone: 'error',
  },
}

export function sourceStatusPresentation(status: SourceStatus): StatusPresentation {
  return SOURCE_STATUS[status] ?? { label: status, defaultHint: '', tone: 'neutral' }
}

const QUERY_OUTCOME_LABEL: Record<QueryOutcome, string> = {
  ok: 'выполнен',
  failed: 'ошибка провайдера',
  // Sent, no answer: charged anyway, because the provider may well have billed it.
  unknown: 'исход неизвестен — требуется сверка расхода',
  refused: 'не отправлялся',
}

export function queryOutcomeLabel(outcome: QueryOutcome): string {
  return QUERY_OUTCOME_LABEL[outcome] ?? outcome
}

const SEARCH_ADAPTER_LABEL: Record<string, string> = {
  openrouter_web_search: 'OpenRouter web search',
  http_json: 'внешний поиск',
  fake: 'тестовый поиск',
}

/**
 * The provider string of a journalled query, as a person reads it.
 *
 * The worker writes `adapter/engine` (`openrouter_web_search/exa`) when it knows
 * which engine served the call, and the bare adapter when nothing ran. The engine
 * is kept visible rather than folded into the adapter name, because with `auto`
 * the two are different answers and the price follows the engine.
 */
export function searchProviderLabel(provider: string): string {
  const [adapter, engine] = provider.split('/', 2)
  const name = SEARCH_ADAPTER_LABEL[adapter] ?? adapter
  return engine ? `${name} · ${engine}` : name
}

/**
 * An amount of research money, as a person reads it: `0,005 USD`.
 *
 * Amounts are integers — millionths of a currency unit — everywhere, and are
 * never converted between currencies. Trailing zeros of the fraction carry no
 * information and are dropped, so a whole number stays a whole number.
 */
export function formatMicros(micros: number, currency: string): string {
  if (!Number.isFinite(micros)) return `— ${currency}`
  const negative = micros < 0
  const absolute = Math.abs(Math.trunc(micros))
  const units = Math.floor(absolute / 1_000_000)
  const fraction = absolute % 1_000_000

  const rendered =
    fraction === 0
      ? String(units)
      : `${units},${String(fraction).padStart(6, '0').replace(/0+$/, '')}`
  return `${negative ? '-' : ''}${rendered} ${currency}`
}

/** Bytes downloaded, as a sentence. Reuses the upload formatter's units. */
export function formatFetchedBytes(bytes: number): string {
  return formatBytes(bytes)
}

// --- Phase 1E: versions, readiness, search and answers ---------------------

/**
 * Labels for a knowledge version.
 *
 * Only one of these is an error, and it is not on this list. `blocked` means the
 * readiness rules were not met, so the version stayed unpublished *on purpose* —
 * the check worked exactly as designed. `superseded` means a newer version took
 * over, `revoked` means somebody withdrew it for a stated reason. Painting any of
 * them red would send the owner hunting for a malfunction that never happened.
 */
const VERSION_STATUS: Record<VersionStatus, StatusPresentation> = {
  draft: {
    label: 'Черновик версии',
    defaultHint: 'Версия собрана, но проверка ещё не завершена. Она не опубликована.',
    tone: 'neutral',
  },
  validating: {
    label: 'Проверяется',
    defaultHint: 'Правила проверки применяются к кандидатам. Версия ещё не опубликована.',
    tone: 'progress',
  },
  published: {
    label: 'Опубликована',
    defaultHint: 'По этой версии выполняются поиск и ответы.',
    tone: 'success',
  },
  blocked: {
    label: 'Не опубликована: правила готовности не выполнены',
    defaultHint:
      'Версия существует как запись проверки и не публикуется. Причины перечислены ниже дословно.',
    tone: 'warn',
  },
  superseded: {
    label: 'Заменена более новой',
    defaultHint:
      'Версия остаётся читаемой: закреплённая когда-то версия не исчезает, по ней можно искать.',
    tone: 'neutral',
  },
  revoked: {
    label: 'Отозвана',
    defaultHint: 'Версия снята с публикации владельцем; причина указана рядом.',
    tone: 'warn',
  },
}

export function versionStatusPresentation(status: VersionStatus): StatusPresentation {
  return VERSION_STATUS[status] ?? { label: status, defaultHint: '', tone: 'neutral' }
}

/**
 * Labels for a check.
 *
 * There is no `needs_provider` here and there cannot be: the check is
 * deterministic, and `model_reviewed = 0` is the normal outcome of an
 * installation without a model key, not a degraded one.
 */
const VALIDATION_RUN_STATUS: Record<ValidationRunStatus, StatusPresentation> = {
  queued: {
    label: 'Проверка в очереди',
    defaultHint: 'Кандидаты поставлены на проверку и ждут исполнителя.',
    tone: 'neutral',
  },
  running: {
    label: 'Проверка идёт',
    defaultHint: 'Правила применяются к кандидатам партнёра.',
    tone: 'progress',
  },
  completed: {
    label: 'Проверка завершена',
    defaultHint: 'Все кандидаты получили статус проверки.',
    tone: 'success',
  },
  partial: {
    label: 'Проверка завершена частично',
    defaultHint: 'Часть кандидатов не вошла в версию — причины ниже дословно.',
    tone: 'warn',
  },
  failed: {
    label: 'Проверка не выполнена',
    defaultHint: 'Версия не собрана.',
    tone: 'error',
  },
}

export function validationRunStatusPresentation(
  status: ValidationRunStatus,
): StatusPresentation {
  return VALIDATION_RUN_STATUS[status] ?? { label: status, defaultHint: '', tone: 'neutral' }
}

/**
 * Verdicts on a statement.
 *
 * The wording of `source_supported` is the single most load-bearing string in
 * this phase. It means the cited source says so — not that the manufacturer
 * confirmed it, not that anybody measured it, not that it is true. So the label
 * says «подтверждено источником» and the hint immediately says what that is not.
 * Every other verdict is a form of "not confirmed by its source", which is why
 * none of them is styled as success and none is styled as an error either: a
 * hypothesis is a legitimate, recorded outcome of the check.
 */
const CLAIM_STATUS: Record<ClaimStatus, StatusPresentation> = {
  source_supported: {
    label: 'подтверждено источником',
    defaultHint:
      'Процитированный источник это утверждает. Это не независимая проверка и не гарантия истинности.',
    tone: 'success',
  },
  hypothesis: {
    label: 'гипотеза — источником не подтверждено',
    defaultHint: 'Источник этого не утверждает; формулировка остаётся предположением.',
    tone: 'warn',
  },
  unknown: {
    label: 'не установлено — источником не подтверждено',
    defaultHint: 'Правилам проверки не хватило данных, чтобы признать утверждение.',
    tone: 'warn',
  },
  conflicted: {
    label: 'противоречие — источником не подтверждено',
    defaultHint: 'Источники говорят разное; выбор между ними здесь не делается.',
    tone: 'warn',
  },
  stale: {
    label: 'источник устарел — не подтверждено',
    defaultHint: 'Утверждение опирается на источник, который с тех пор изменился или устарел.',
    tone: 'warn',
  },
}

export function claimStatusPresentation(status: ClaimStatus): StatusPresentation {
  return CLAIM_STATUS[status] ?? { label: status, defaultHint: '', tone: 'neutral' }
}

/** Only `source_supported` may look confirmed. Everything else must not. */
export function isClaimSourceSupported(status: ClaimStatus): boolean {
  return status === 'source_supported'
}

const READINESS_TOPIC_LABEL: Record<ReadinessTopic, string> = {
  product_description: 'описание продукта',
  audience_hypotheses: 'гипотезы аудитории',
  characteristic_answers: 'ответы о характеристиках',
  commercial_answers: 'ответы о коммерческих условиях',
}

export function readinessTopicLabel(topic: ReadinessTopic): string {
  return READINESS_TOPIC_LABEL[topic] ?? topic
}

/** The four topics, in the order §7 names them. Rendered even when the server
 *  sent no entry for one, so the matrix is never quietly three rows long. */
export const READINESS_TOPICS: ReadinessTopic[] = [
  'product_description',
  'audience_hypotheses',
  'characteristic_answers',
  'commercial_answers',
]

/**
 * Readiness of one topic.
 *
 * `blocked` here is a warning, not an error: it says knowledge is missing, which
 * is a fact about the materials and not a malfunction. And `ready` is worded as
 * availability — «знания есть» — because readiness is never a permission to send
 * anything or to promise anything.
 */
const READINESS_STATE: Record<ReadinessState, StatusPresentation> = {
  ready: {
    label: 'знания есть',
    defaultHint: 'По этой теме в версии есть подтверждённые источником утверждения.',
    tone: 'success',
  },
  limited: {
    label: 'знания неполные',
    defaultHint: 'Отвечать можно с оговорками; оговорки названы рядом.',
    tone: 'warn',
  },
  blocked: {
    label: 'знаний недостаточно',
    defaultHint: 'По этой теме отвечать нечем: пробел записан и назван рядом.',
    tone: 'warn',
  },
}

export function readinessStatePresentation(state: ReadinessState): StatusPresentation {
  return READINESS_STATE[state] ?? { label: state, defaultHint: '', tone: 'neutral' }
}

/**
 * States of a search response.
 *
 * `no_published_version` is neither an error nor an empty result — it is the
 * named state of a partner who has nothing published yet, and it is styled
 * neutrally so nobody starts debugging the search.
 */
const SEARCH_STATE: Record<SearchState, StatusPresentation> = {
  ok: {
    label: 'найдено',
    defaultHint: 'Все результаты принадлежат одной опубликованной версии.',
    tone: 'success',
  },
  no_published_version: {
    label: 'у партнёра нет опубликованной версии',
    defaultHint:
      'Искать пока не по чему: проверка не проходила, была заблокирована или версия отозвана. Это состояние, а не ошибка.',
    tone: 'neutral',
  },
  insufficient_evidence: {
    label: 'подходящих утверждений нет',
    defaultHint: 'Версия есть, но в ней нет утверждений по этому запросу. Догадка не подставляется.',
    tone: 'warn',
  },
}

export function searchStatePresentation(state: SearchState): StatusPresentation {
  return SEARCH_STATE[state] ?? { label: state, defaultHint: '', tone: 'neutral' }
}

/**
 * States of an answer.
 *
 * `evidence_only` is the honest half-answer: statements with citations exist,
 * prose does not, because no model is configured or because the model's answer
 * failed its citation check. That is a working system saying what it has, so it
 * is a warning at most — never an error.
 */
const ANSWER_STATE: Record<AnswerState, StatusPresentation> = {
  answered: {
    label: 'ответ составлен моделью',
    defaultHint: 'Текст — формулировка модели; под ним цитаты утверждений закреплённой версии.',
    tone: 'success',
  },
  evidence_only: {
    label: 'прозаический ответ не составлен — показаны найденные утверждения',
    defaultHint:
      'Найденные утверждения и цитаты есть, связного текста нет: модель не настроена либо её ответ не прошёл проверку цитат.',
    tone: 'warn',
  },
  insufficient_evidence: {
    label: 'ответа нет: подходящих утверждений не нашлось',
    defaultHint: 'Версия есть, но отвечать нечем. Догадка не подставляется.',
    tone: 'warn',
  },
  no_published_version: {
    label: 'у партнёра нет опубликованной версии',
    defaultHint:
      'Отвечать пока не по чему: проверка не проходила, была заблокирована или версия отозвана. Это состояние, а не ошибка.',
    tone: 'neutral',
  },
}

export function answerStatePresentation(state: AnswerState): StatusPresentation {
  return ANSWER_STATE[state] ?? { label: state, defaultHint: '', tone: 'neutral' }
}

const MATCH_KIND_LABEL: Record<MatchKind, string> = {
  exact: 'точное совпадение',
  keyword: 'по ключевым словам',
  vector: 'по смыслу (вектор)',
}

export function matchKindLabel(kind: MatchKind): string {
  return MATCH_KIND_LABEL[kind] ?? kind
}

/**
 * The mode one response ran in.
 *
 * `keyword` is not a failure and not a lesser result set — it is the whole
 * search minus its vector half, and `degraded[]` next to it says why that half
 * was unavailable.
 */
const SEARCH_MODE_LABEL: Record<SearchMode, string> = {
  hybrid: 'поиск по ключевым словам и по смыслу',
  keyword: 'только по ключевым словам',
}

export function searchModeLabel(mode: SearchMode): string {
  return SEARCH_MODE_LABEL[mode] ?? mode
}

/** The value with its unit, exactly as the version recorded it. */
export function claimValueLine(claim: VersionClaim): string {
  return claim.unit ? `${claim.value_text} ${claim.unit}` : claim.value_text
}

/**
 * What one version holds, in the counters it really has.
 *
 * Counts, never a share: "80% подтверждено" would read as a measure of quality,
 * and the number of unconfirmed statements is exactly the thing that must stay
 * legible.
 */
export function versionCountsLine(version: KnowledgeVersion): string {
  const parts = [
    `утверждений: ${version.claims_total}`,
    `подтверждено источником: ${version.claims_source_supported}`,
    `гипотез: ${version.claims_hypothesis}`,
    `не установлено: ${version.claims_unknown}`,
    `противоречий: ${version.claims_conflicted}`,
    `устаревших: ${version.claims_stale}`,
    `пробелов: ${version.gaps_open}`,
  ]
  return parts.join(' · ')
}

/** Pages whose outcome the owner may still be able to change. */
export function pagesNeedingAttention(summary: ExtractionSummary): number {
  return (
    summary.pages_needs_ocr +
    summary.pages_partial +
    summary.pages_failed +
    summary.pages_pending
  )
}

// --- phase 1F: the update cycle -------------------------------------------------------

/**
 * Labels for the refresh status.
 *
 * `current` is deliberately not called «актуально»: what the server checked is
 * that the published version was built from the candidates that exist now, and
 * that is what the sentence says.
 */
const REFRESH_STATE: Record<RefreshState, StatusPresentation> = {
  never_published: {
    label: 'Ещё ничего не опубликовано',
    defaultHint: 'Версии знаний у партнёра пока нет. Причины перечислены ниже.',
    tone: 'neutral',
  },
  current: {
    label: 'Опубликованная версия построена из текущих кандидатов',
    defaultHint: 'Перепроверка ничего бы не изменила.',
    tone: 'success',
  },
  revalidation_required: {
    label: 'Нужна перепроверка',
    defaultHint:
      'Что-то изменилось после публикации. Опубликованная версия остаётся прежней, пока новая не пройдёт правила.',
    tone: 'warn',
  },
  checking: {
    label: 'Проверка идёт',
    defaultHint: 'Опубликованная версия не меняется, пока проверка не закончится.',
    tone: 'progress',
  },
  retracted: {
    label: 'Версия отозвана',
    defaultHint: 'Поиск и ответы по опубликованной версии недоступны, пока не выйдет новая.',
    tone: 'error',
  },
}

export function refreshStatePresentation(state: RefreshState): StatusPresentation {
  return REFRESH_STATE[state] ?? { label: state, defaultHint: '', tone: 'neutral' }
}

/** Where one document stands in the cycle. */
const SOURCE_STATE: Record<SourceState, StatusPresentation> = {
  reading: {
    label: 'Читается',
    defaultHint: 'Материал в очереди на чтение или читается сейчас.',
    tone: 'progress',
  },
  unreadable: {
    label: 'Пригодного текста нет',
    defaultHint: 'Из материала не получено текста, разбирать нечего.',
    tone: 'warn',
  },
  not_drafted: {
    label: 'Не разобран',
    defaultHint: 'Материал прочитан, но продуктолог его ещё не разбирал.',
    tone: 'warn',
  },
  drafted: {
    label: 'Разобран по текущему чтению',
    defaultHint: 'Кандидаты сделаны из того текста, который сейчас хранится.',
    tone: 'success',
  },
  reread_after_draft: {
    label: 'Перечитан после разбора',
    defaultHint: 'Кандидаты описывают прежнее чтение документа.',
    tone: 'warn',
  },
}

export function sourceStatePresentation(state: SourceState): StatusPresentation {
  return SOURCE_STATE[state] ?? { label: state, defaultHint: '', tone: 'neutral' }
}

/**
 * What happened to one stage of a requested refresh.
 *
 * Only `queued` means work will happen. Every other value is a refusal with a
 * reason, and the interface shows the server's sentence rather than implying
 * progress.
 */
const REFRESH_OUTCOME: Record<RefreshStepOutcome, StatusPresentation> = {
  queued: {
    label: 'Поставлено в очередь',
    defaultHint: 'Работа принята. Сколько она займёт — неизвестно, и оценка не показывается.',
    tone: 'progress',
  },
  already_running: {
    label: 'Уже выполняется',
    defaultHint: 'Второй запуск не создаётся: работа уже идёт.',
    tone: 'neutral',
  },
  up_to_date: {
    label: 'Не требуется',
    defaultHint: 'На этом шаге нечего делать.',
    tone: 'success',
  },
  needs_provider: {
    label: 'Нужна настройка',
    defaultHint: 'Шаг не запускался: адаптер не настроен. Недостающие переменные названы рядом.',
    tone: 'warn',
  },
  waiting: {
    label: 'Ждёт предыдущий шаг',
    defaultHint: 'Запускать нечего, пока предыдущий этап ничего не дал.',
    tone: 'neutral',
  },
}

export function refreshOutcomePresentation(outcome: RefreshStepOutcome): StatusPresentation {
  return REFRESH_OUTCOME[outcome] ?? { label: outcome, defaultHint: '', tone: 'neutral' }
}

const REFRESH_STEP_KIND: Record<RefreshStepKind, string> = {
  extraction: 'Чтение документа',
  understanding: 'Разбор продуктологом',
  validation: 'Проверка и публикация',
}

export function refreshStepLabel(kind: RefreshStepKind): string {
  return REFRESH_STEP_KIND[kind] ?? kind
}

/** One line of the history, titled. The sentence itself comes from the server. */
const EVENT_KIND: Record<EventKind, string> = {
  material_uploaded: 'Материал загружен',
  material_duplicate: 'Повторная загрузка',
  material_reprocess_requested: 'Запрошено повторное чтение',
  material_extraction_finished: 'Чтение завершено',
  understanding_queued: 'Разбор поставлен в очередь',
  understanding_finished: 'Разбор завершён',
  validation_queued: 'Проверка поставлена в очередь',
  validation_finished: 'Проверка завершена',
  version_published: 'Версия опубликована',
  version_blocked: 'Версия не опубликована',
  version_superseded: 'Версия заменена',
  version_retracted: 'Версия отозвана',
  refresh_requested: 'Запрошено обновление',
  export_read: 'Версия выгружена',
  job_failed: 'Задание не выполнено',
  retention_applied: 'Очистка истории',
}

export function eventKindLabel(kind: EventKind): string {
  return EVENT_KIND[kind] ?? kind
}

const EVENT_ACTOR: Record<EventActor, string> = {
  owner: 'владелец',
  worker: 'обработчик',
  system: 'обслуживание',
}

export function eventActorLabel(actor: EventActor): string {
  return EVENT_ACTOR[actor] ?? actor
}

const CHANGE_KIND: Record<ChangeKind, string> = {
  added: 'добавлено',
  removed: 'исчезло',
  changed: 'изменилось',
}

export function changeKindLabel(kind: ChangeKind): string {
  return CHANGE_KIND[kind] ?? kind
}

/**
 * Counts of a comparison. Counts, never a percentage: "версия обновилась на
 * 30%" would be a measure of change that nothing here computes.
 */
export function changeCountsLine(counts: ChangeCounts): string {
  return [
    `изменилось: ${counts.changed}`,
    `добавлено: ${counts.added}`,
    `исчезло: ${counts.removed}`,
    `без изменений: ${counts.unchanged}`,
  ].join(' · ')
}
