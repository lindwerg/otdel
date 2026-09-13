import type { MaterialStatus } from '../api/types'

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
    label: 'Готово',
    defaultHint: 'Обработка завершена.',
    tone: 'success',
  },
  partial: {
    label: 'Частично готово',
    defaultHint: 'Часть страниц не удалось обработать.',
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

export const RETRYABLE_MATERIAL_STATUSES: MaterialStatus[] = ['failed', 'partial', 'quarantined']
