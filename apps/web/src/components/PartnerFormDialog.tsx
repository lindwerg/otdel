import { useEffect, useId, useRef, useState, type FormEvent } from 'react'
import type { Partner } from '../api/types'
import { validatePartnerName, validatePartnerNote } from '../lib/validation'
import { Dialog } from './Dialog'

interface PartnerFormDialogProps {
  open: boolean
  mode: 'create' | 'edit'
  partner?: Partner | null
  onClose: () => void
  onSubmit: (input: { name: string; note?: string }) => Promise<void>
}

export function PartnerFormDialog({ open, mode, partner, onClose, onSubmit }: PartnerFormDialogProps) {
  const titleId = useId()
  const nameErrorId = useId()
  const noteErrorId = useId()
  const nameInputRef = useRef<HTMLInputElement>(null)

  const [name, setName] = useState('')
  const [note, setNote] = useState('')
  const [nameError, setNameError] = useState<string | null>(null)
  const [noteError, setNoteError] = useState<string | null>(null)
  const [submitError, setSubmitError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  useEffect(() => {
    if (!open) return
    setName(mode === 'edit' ? (partner?.name ?? '') : '')
    setNote(mode === 'edit' ? (partner?.note ?? '') : '')
    setNameError(null)
    setNoteError(null)
    setSubmitError(null)
    setSubmitting(false)
  }, [open, mode, partner])

  async function handleSubmit(event: FormEvent) {
    event.preventDefault()
    const nameResult = validatePartnerName(name)
    const noteResult = validatePartnerNote(note)
    let hasError = false
    if (!nameResult.ok) {
      setNameError(nameResult.message)
      hasError = true
    } else {
      setNameError(null)
    }
    if (!noteResult.ok) {
      setNoteError(noteResult.message)
      hasError = true
    } else {
      setNoteError(null)
    }
    if (hasError) {
      nameInputRef.current?.focus()
      return
    }
    setSubmitError(null)
    setSubmitting(true)
    const trimmedName = (nameResult as { ok: true; value: string }).value
    const trimmedNote = (noteResult as { ok: true; value: string }).value
    try {
      await onSubmit(
        // Editing must always send `note` explicitly — even an empty string
        // — so clearing an existing note actually clears it server-side
        // (an omitted/undefined field means "leave note unchanged" per the
        // PATCH contract, which would otherwise silently no-op a deletion).
        // Creating with a blank note simply omits the optional field.
        mode === 'edit'
          ? { name: trimmedName, note: trimmedNote }
          : trimmedNote
            ? { name: trimmedName, note: trimmedNote }
            : { name: trimmedName },
      )
    } catch (err) {
      setSubmitError(err instanceof Error ? err.message : 'Не удалось сохранить партнёра.')
      setSubmitting(false)
    }
  }

  const title = mode === 'create' ? 'Добавить партнёра' : 'Изменить партнёра'
  const submitLabel = mode === 'create' ? 'Создать' : 'Сохранить'

  return (
    <Dialog open={open} onClose={onClose} labelledBy={titleId} preventClose={submitting}>
      <form onSubmit={handleSubmit} noValidate>
        <h2 id={titleId}>{title}</h2>
        <label className="field">
          <span>
            Название <span aria-hidden="true">*</span>
          </span>
          <input
            ref={nameInputRef}
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            aria-invalid={nameError ? 'true' : undefined}
            aria-describedby={nameError ? nameErrorId : undefined}
            maxLength={200}
            required
          />
        </label>
        {nameError ? (
          <p className="field-error" id={nameErrorId}>
            {nameError}
          </p>
        ) : null}

        <label className="field">
          <span>Пояснение (необязательно)</span>
          <textarea
            value={note}
            onChange={(e) => setNote(e.target.value)}
            aria-invalid={noteError ? 'true' : undefined}
            aria-describedby={noteError ? noteErrorId : undefined}
            maxLength={10000}
          />
        </label>
        {noteError ? (
          <p className="field-error" id={noteErrorId}>
            {noteError}
          </p>
        ) : null}

        {submitError ? (
          <p className="field-error" role="alert">
            {submitError}
          </p>
        ) : null}

        <div className="dialog-actions">
          <button type="button" className="button button-ghost" onClick={onClose} disabled={submitting}>
            Отмена
          </button>
          <button type="submit" className="button button-primary" disabled={submitting}>
            {submitting ? 'Сохраняем…' : submitLabel}
          </button>
        </div>
      </form>
    </Dialog>
  )
}
