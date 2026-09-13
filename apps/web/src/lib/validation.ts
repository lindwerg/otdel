import { MAX_UPLOAD_BYTES } from '../api/client'
import { formatBytes } from './format'

export type FieldResult = { ok: true; value: string } | { ok: false; message: string }

/** Partner.name: 1–200 characters after trim (docs/implementation-contract.md). */
export function validatePartnerName(raw: string): FieldResult {
  const value = raw.trim()
  if (value.length === 0) {
    return { ok: false, message: 'Введите название партнёра.' }
  }
  if (value.length > 200) {
    return { ok: false, message: 'Название не должно превышать 200 символов.' }
  }
  return { ok: true, value }
}

/** Partner.note: up to 10000 characters, optional. */
export function validatePartnerNote(raw: string): FieldResult {
  const value = raw.trim()
  if (value.length > 10000) {
    return { ok: false, message: 'Пояснение не должно превышать 10 000 символов.' }
  }
  return { ok: true, value }
}

/**
 * Client-side check for the one documented, unambiguous limit (file size).
 * File type is not second-guessed here: the server performs the real
 * signature check, and a client-only MIME allowlist would just produce
 * confusing false rejections for files the server would have accepted.
 */
export function validateFileSize(file: File): string | null {
  if (file.size > MAX_UPLOAD_BYTES) {
    return `Файл «${file.name}» превышает допустимый размер ${formatBytes(MAX_UPLOAD_BYTES)} (сейчас ${formatBytes(file.size)}).`
  }
  if (file.size === 0) {
    return `Файл «${file.name}» пустой.`
  }
  return null
}
