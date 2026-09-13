import type {
  ExtractionSummary,
  FactKind,
  KnowledgeFact,
  KnowledgeRunStatus,
  MaterialStatus,
  PageStatus,
  QuestionAudience,
  TextSource,
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

/** Pages whose outcome the owner may still be able to change. */
export function pagesNeedingAttention(summary: ExtractionSummary): number {
  return (
    summary.pages_needs_ocr +
    summary.pages_partial +
    summary.pages_failed +
    summary.pages_pending
  )
}
